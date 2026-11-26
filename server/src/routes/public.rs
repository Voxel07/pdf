//! Public signing endpoints. Access requires the random link token plus a check of the signer's
//! registered name. After the name check a short-lived session token is issued (sent back in
//! the `X-Session` header).
//!
//! General (QR) links never hand out the personal link token: their session comes with a
//! `qr-...` handle that stands in for the token in these routes and ends with the session.

use std::collections::HashMap;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::models::*;
use crate::service::{self, *};
use crate::state::AppState;
use crate::util::*;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/s/{token}", get(info))
        .route("/s/{token}/unlock", post(unlock).layer(DefaultBodyLimit::max(4096)))
        .route("/s/{token}/form", get(form))
        .route("/s/{token}/template.pdf", get(template_pdf))
        .route("/s/{token}/submit", post(submit))
        .route("/s/{token}/signed.pdf", get(signed_pdf))
        .route("/p/{token}", get(link_info))
        .route("/p/{token}/identify", post(link_identify).layer(DefaultBodyLimit::max(4096)))
}

const SESSION_HOURS: i64 = 2;

const HANDLE_PREFIX: &str = "qr-";

/// The submission behind a page address: a personal link token, or the handle of a live
/// general-link session.
async fn resolve_link<'e>(ex: impl sqlx::SqliteExecutor<'e>, token: &str) -> AppResult<Submission> {
    if !token.starts_with(HANDLE_PREFIX) {
        return submission_by_token(ex, token).await;
    }
    sqlx::query_as(
        "SELECT s.* FROM submissions s JOIN signer_sessions ss ON ss.submission_id = s.id
         WHERE ss.handle = ? AND ss.expires_at > ?",
    )
    .bind(token)
    .bind(Utc::now().to_rfc3339())
    .fetch_optional(ex)
    .await?
    .ok_or_else(|| AppError::new(StatusCode::GONE, "session_ended", "this signing session has ended - scan the code again to continue"))
}

async fn info(State(st): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Value>> {
    let sub = resolve_link(&st.pool, &token).await?;
    let t = get_template(&st.pool, &sub.template_id).await?;
    Ok(Json(json!({
        "template_name": t.name,
        "locked": sub.failed_attempts >= st.cfg.max_name_attempts,
        "revoked": sub.status == "revoked",
    })))
}

#[derive(Deserialize)]
struct UnlockReq {
    name: String,
}

async fn unlock(
    State(st): State<AppState>,
    Path(token): Path<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<UnlockReq>,
) -> AppResult<Json<Value>> {
    let ip = client_ip(&st, &headers, addr);
    limit_attempts(&st, "unlock", &ip, 50)?;
    if token.starts_with(HANDLE_PREFIX) {
        return Err(AppError::new(StatusCode::GONE, "session_ended", "this signing session has ended - scan the code again to continue"));
    }
    if req.name.len() > 1024 {
        return Err(AppError::bad_request("name is too long"));
    }
    let mut tx = service::begin_write(&st.pool).await?;
    let sub = submission_by_token(&mut *tx, &token).await?;
    check_signing_link(&st, &sub)?;
    let person = get_person(&mut *tx, &sub.person_id).await?;
    if normalize_name(&req.name) != normalize_name(&person.name) {
        sqlx::query("UPDATE submissions SET failed_attempts = failed_attempts + 1 WHERE id = ?")
            .bind(&sub.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let left = st.cfg.max_name_attempts - sub.failed_attempts - 1;
        if left == 0 {
            let t = get_template(&st.pool, &sub.template_id).await?;
            let msg = format!("signing link for '{}' of {} locked after too many wrong names", t.name, person.name);
            crate::activity::warn(&st.pool, "signing", None, msg).await;
        }
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "wrong_name",
            format!("the name does not match ({} attempts left)", left.max(0)),
        ));
    }
    if sub.failed_attempts > 0 {
        sqlx::query("UPDATE submissions SET failed_attempts = 0 WHERE id = ?").bind(&sub.id).execute(&mut *tx).await?;
    }
    let (session, expires) = issue_session(&mut tx, &sub.id, None).await?;
    tx.commit().await?;
    let form = build_form(&st, &sub).await?;
    Ok(Json(json!({ "session": session, "expires_at": expires, "form": form })))
}

fn check_signing_link(st: &AppState, sub: &Submission) -> AppResult<()> {
    if sub.status == "revoked" {
        return Err(AppError::new(StatusCode::GONE, "revoked", "this link has been revoked"));
    }
    if sub.failed_attempts >= st.cfg.max_name_attempts {
        return Err(AppError::new(StatusCode::LOCKED, "locked", "too many wrong attempts - please ask for a new link"));
    }
    Ok(())
}

async fn issue_session(tx: &mut sqlx::SqliteConnection, submission_id: &str, handle: Option<&str>) -> AppResult<(String, String)> {
    let session = random_token(32);
    let expires = (Utc::now() + Duration::hours(SESSION_HOURS)).to_rfc3339();
    sqlx::query("DELETE FROM signer_sessions WHERE expires_at < ?").bind(Utc::now().to_rfc3339()).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO signer_sessions (token_hash, submission_id, expires_at, handle) VALUES (?, ?, ?, ?)")
        .bind(sha256_hex(&session))
        .bind(submission_id)
        .bind(&expires)
        .bind(handle)
        .execute(&mut *tx)
        .await?;
    Ok((session, expires))
}

/// Validates the session (header `X-Session`) for the given link token. Sessions are not
/// accepted in the URL, so they stay out of request logs, history and referrers.
async fn session_submission(st: &AppState, token: &str, headers: &HeaderMap) -> AppResult<Submission> {
    Ok(session_submission_hash(st, token, headers).await?.0)
}

/// Like [`session_submission`], also returning the session's hash for rechecks.
async fn session_submission_hash(st: &AppState, token: &str, headers: &HeaderMap) -> AppResult<(Submission, String)> {
    let session = headers.get("x-session").and_then(|v| v.to_str().ok()).map(String::from).ok_or_else(AppError::unauthorized)?;
    let sub = resolve_link(&st.pool, token).await?;
    let hash = sha256_hex(&session);
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT submission_id, expires_at FROM signer_sessions WHERE token_hash = ?")
            .bind(&hash)
            .fetch_optional(&st.pool)
            .await?;
    match row {
        Some((sid, exp)) if sid == sub.id && parse_ts(&exp).is_some_and(|e| e > Utc::now()) => Ok((sub, hash)),
        _ => Err(AppError::new(StatusCode::UNAUTHORIZED, "session_expired", "session expired - please enter your name again")),
    }
}

async fn build_form(st: &AppState, sub: &Submission) -> AppResult<Value> {
    let t = get_template(&st.pool, &sub.template_id).await?;
    let person = get_person(&st.pool, &sub.person_id).await?;
    let values = load_values(&st.pool, &sub.id).await?;
    let rounds = load_rounds(&st.pool, &sub.id).await?;
    let done = done_sections(&t, &values, &rounds);
    let b64 = base64::engine::general_purpose::STANDARD;
    let vals: serde_json::Map<String, Value> = values
        .iter()
        .map(|v| {
            let image = v.image.as_ref().map(|i| format!("data:image/png;base64,{}", b64.encode(i)));
            let display = t
                .fields
                .iter()
                .find(|f| f.id == v.field_id)
                .filter(|f| f.kind == FieldKind::Date)
                .map(|f| format_date(&v.value, f.format.as_deref()));
            (v.field_id.clone(), json!({ "value": v.value, "display": display, "image": image, "round_id": v.round_id }))
        })
        .collect();
    let section_done: serde_json::Map<String, Value> = t
        .sections
        .iter()
        .map(|s| {
            let at = rounds
                .iter()
                .find(|r| serde_json::from_str::<Vec<u32>>(&r.sections).unwrap_or_default().contains(&s.id))
                .map(|r| r.created_at.clone());
            (s.id.to_string(), json!({ "done": done.contains(&s.id), "at": at }))
        })
        .collect();
    let v = validity(&t, sub);
    Ok(json!({
        "template": {
            "name": t.name,
            "description": t.description,
            "page_count": t.page_count,
            "fields": t.fields,
            "sections": t.sections,
        },
        "person_name": person.name,
        "status": if v.expired { "expired" } else { sub.status.as_str() },
        "completed_at": sub.completed_at,
        "valid_until": v.valid_until,
        "values": vals,
        "sections": section_done,
    }))
}

async fn form(
    State(st): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let sub = session_submission(&st, &token, &headers).await?;
    Ok(Json(build_form(&st, &sub).await?))
}

pub fn pdf_response(bytes: Vec<u8>, filename: &str, inline: bool) -> Response {
    let disp = format!("{}; filename=\"{}\"", if inline { "inline" } else { "attachment" }, filename.replace('"', ""));
    (
        [(header::CONTENT_TYPE, "application/pdf".to_string()), (header::CONTENT_DISPOSITION, disp), (header::CACHE_CONTROL, "private, no-store".to_string())],
        bytes,
    )
        .into_response()
}

async fn template_pdf(
    State(st): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let sub = session_submission(&st, &token, &headers).await?;
    let bytes = service::template_pdf(&st, &sub.template_id).await?;
    Ok(pdf_response(bytes, "template.pdf", true))
}

#[derive(Deserialize)]
struct SubmitReq {
    values: HashMap<String, String>,
}

fn client_ip(st: &AppState, headers: &HeaderMap, addr: SocketAddr) -> String {
    if headers.contains_key("x-forwarded-for") && !st.cfg.trusted_proxies.iter().any(|n| n.contains(addr.ip())) {
        st.warn_untrusted_forwarding();
    }
    crate::util::client_ip(headers, Some(addr.ip()), &st.cfg.trusted_proxies)
}

fn limit_attempts(st: &AppState, action: &str, ip: &str, max: usize) -> AppResult<()> {
    if !st.rate_ok(&format!("{action}:{}", crate::state::rate_key(ip)), max) {
        return Err(AppError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many attempts - please wait a few minutes"));
    }
    Ok(())
}

async fn submit(
    State(st): State<AppState>,
    Path(token): Path<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SubmitReq>,
) -> AppResult<Json<Value>> {
    let (sub, session_hash) = session_submission_hash(&st, &token, &headers).await?;
    let t = get_template(&st.pool, &sub.template_id).await?;
    let person = get_person(&st.pool, &sub.person_id).await?;
    let ip = client_ip(&st, &headers, addr);
    let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let sub = submit_round(
        &st,
        &sub,
        &t,
        &person,
        &req.values,
        SubmitContext { signer_name: &person.name, ip: &ip, user_agent: &ua, session_hash: Some(&session_hash) },
    )
    .await?;
    let msg = if sub.completed_at.as_deref() == Some(sub.updated_at.as_str()) {
        format!("{} signed '{}'", person.name, t.name)
    } else {
        format!("{} continued '{}'", person.name, t.name)
    };
    crate::activity::info(&st.pool, "signing", Some(ip.as_str()), msg).await;
    Ok(Json(build_form(&st, &sub).await?))
}

async fn signed_pdf(
    State(st): State<AppState>,
    Path(token): Path<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let sub = session_submission(&st, &token, &headers).await?;
    // every download renders the PDF anew
    limit_attempts(&st, "pdf", &client_ip(&st, &headers, addr), 60)?;
    let (name, bytes) = render_submission(&st, &sub).await?;
    Ok(pdf_response(bytes, &name, false))
}

// ---------------------------------------------------------------------------------------
// General links (QR codes): identify by name (+ zip if ambiguous), then sign on the spot.

async fn active_link<'e>(ex: impl sqlx::SqliteExecutor<'e>, token: &str) -> AppResult<PublicLink> {
    let link: PublicLink = sqlx::query_as("SELECT * FROM public_links WHERE token = ?")
        .bind(token)
        .fetch_optional(ex)
        .await?
        .ok_or_else(|| AppError::not_found("this link is invalid"))?;
    let expired = link.expires_at.as_deref().and_then(parse_ts).is_some_and(|e| e <= Utc::now());
    if !link.active || expired {
        return Err(AppError::new(StatusCode::GONE, "inactive", "this link is no longer active"));
    }
    Ok(link)
}

async fn link_info(State(st): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Value>> {
    let link = active_link(&st.pool, &token).await?;
    let t = get_template(&st.pool, &link.template_id).await?;
    if t.archived {
        return Err(AppError::new(StatusCode::GONE, "inactive", "this document is no longer available"));
    }
    Ok(Json(json!({ "template_name": t.name, "description": t.description, "label": link.label, "validity_days": t.validity_days })))
}

#[derive(Deserialize)]
struct IdentifyReq {
    name: String,
    zip: Option<String>,
}

async fn link_identify(
    State(st): State<AppState>,
    Path(token): Path<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<IdentifyReq>,
) -> AppResult<Json<Value>> {
    let ip = client_ip(&st, &headers, addr);
    // A first-time signer uses two requests: one for the name and one with the ZIP code.
    limit_attempts(&st, "identify", &ip, 100)?;
    let link = active_link(&st.pool, &token).await?;
    let t = get_template(&st.pool, &link.template_id).await?;
    if t.archived {
        return Err(AppError::new(StatusCode::GONE, "inactive", "this document is no longer available"));
    }
    let person = match identify_by_name(&st.pool, &req.name, req.zip.as_deref()).await? {
        Identified::NeedZip => return Ok(Json(json!({ "result": "need_zip", "reason": "verify" }))),
        Identified::Person(p) => p,
        Identified::New(name, zip) => {
            // first-time signers become new people; bound how many one client can create
            limit_attempts(&st, "qr-new", &ip, 10)?;
            create_person(&st.pool, &name, None, Some(&zip)).await?
        }
    };
    let subs: Vec<Submission> = sqlx::query_as(
        "SELECT * FROM submissions WHERE person_id = ? AND template_id = ? AND status != 'revoked' ORDER BY created_at DESC",
    )
    .bind(&person.id)
    .bind(&t.id)
    .fetch_all(&st.pool)
    .await?;
    if let Some(s) = subs.iter().find(|s| validity(&t, s).valid) {
        let v = validity(&t, s);
        return Ok(Json(json!({
            "result": "valid", "person_name": person.name, "valid_until": v.valid_until, "completed_at": s.completed_at,
        })));
    }
    // reuses the open document or creates one, atomically with concurrent bot / QR requests
    let (sub, _) = find_or_create_submission(&st.pool, &t, &person, false, Some(&format!("link:{}", link.id))).await?;
    let mut tx = service::begin_write(&st.pool).await?;
    active_link(&mut *tx, &token).await?;
    let sub = submission_by_token(&mut *tx, &sub.token).await?;
    check_signing_link(&st, &sub)?;
    let handle = format!("{HANDLE_PREFIX}{}", random_token(32));
    let (session, _) = issue_session(&mut tx, &sub.id, Some(&handle)).await?;
    sqlx::query("UPDATE public_links SET uses = uses + 1 WHERE id = ?").bind(&link.id).execute(&mut *tx).await?;
    tx.commit().await?;
    tracing::info!("general link {} opened for submission {}", link.id, sub.id);
    Ok(Json(json!({ "result": "sign", "handle": handle, "session": session, "person_name": person.name })))
}
