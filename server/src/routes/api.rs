//! Integration API for bots (`Authorization: Bearer <api key>`).

use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::ApiClient;
use crate::models::*;
use crate::routes::public::pdf_response;
use crate::service::*;
use crate::state::AppState;
use crate::util::*;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/templates", get(templates))
        .route("/persons/{discord_id}", get(person).put(put_person))
        .route("/persons/{discord_id}/status", get(status))
        .route("/persons/link", post(link_accounts))
        .route("/links", post(create_link))
        .route("/submissions/{id}", get(submission))
        .route("/submissions/{id}/pdf", get(submission_pdf))
}

async fn templates(_c: ApiClient, State(st): State<AppState>) -> AppResult<Json<Value>> {
    let list = list_templates(&st.pool, false).await?;
    Ok(Json(json!(list
        .iter()
        .map(|t| json!({
            "id": t.id, "slug": t.slug, "name": t.name, "description": t.description,
            "validity_days": t.validity_days, "sections": t.sections,
        }))
        .collect::<Vec<_>>())))
}

async fn person_view(st: &AppState, p: &Person) -> AppResult<Value> {
    let accounts = discord_accounts(&st.pool, &p.id).await?;
    let subs: Vec<Submission> = sqlx::query_as("SELECT * FROM submissions WHERE person_id = ? ORDER BY created_at DESC")
        .bind(&p.id)
        .fetch_all(&st.pool)
        .await?;
    let mut docs = Vec::new();
    for s in &subs {
        let t = get_template(&st.pool, &s.template_id).await?;
        docs.push(submission_json(st, s, &t).await?);
    }
    Ok(json!({ "person": p, "discord_accounts": accounts, "submissions": docs }))
}

async fn person(_c: ApiClient, State(st): State<AppState>, Path(discord_id): Path<String>) -> AppResult<Json<Value>> {
    let p = person_by_discord(&st.pool, &discord_id).await?.ok_or_else(|| AppError::not_found("not registered"))?;
    Ok(Json(person_view(&st, &p).await?))
}

#[derive(Deserialize)]
struct PutPerson {
    name: Option<String>,
    username: Option<String>,
    zip: Option<String>,
}

async fn put_person(
    _c: ApiClient,
    State(st): State<AppState>,
    Path(discord_id): Path<String>,
    Json(req): Json<PutPerson>,
) -> AppResult<Json<Value>> {
    let p = upsert_discord_person(&st.pool, &discord_id, req.name.as_deref(), req.username.as_deref(), req.zip.as_deref()).await?;
    Ok(Json(person_view(&st, &p).await?))
}

#[derive(Deserialize)]
struct StatusQuery {
    template: String,
}

/// Does this Discord user have a valid document for the template?
async fn status(
    _c: ApiClient,
    State(st): State<AppState>,
    Path(discord_id): Path<String>,
    Query(q): Query<StatusQuery>,
) -> AppResult<Json<Value>> {
    let t = get_template(&st.pool, &q.template).await?;
    let Some(p) = person_by_discord(&st.pool, &discord_id).await? else {
        return Ok(Json(json!({ "registered": false, "valid": false, "submission": null })));
    };
    let subs: Vec<Submission> = sqlx::query_as(
        "SELECT * FROM submissions WHERE person_id = ? AND template_id = ? AND status != 'revoked' ORDER BY created_at DESC",
    )
    .bind(&p.id)
    .bind(&t.id)
    .fetch_all(&st.pool)
    .await?;
    let best = subs.iter().find(|s| validity(&t, s).valid).or_else(|| subs.first());
    let sub_json = match best {
        Some(s) => Some(submission_json(&st, s, &t).await?),
        None => None,
    };
    let valid = best.is_some_and(|s| validity(&t, s).valid);
    Ok(Json(json!({
        "registered": true,
        "person_name": p.name,
        "valid": valid,
        "valid_until": sub_json.as_ref().and_then(|s| s.get("valid_until").cloned()),
        "submission": sub_json,
    })))
}

#[derive(Deserialize)]
struct LinkReq {
    template: String,
    discord_id: String,
    name: Option<String>,
    username: Option<String>,
    zip: Option<String>,
    #[serde(default)]
    force_new: bool,
}

/// Returns the signing link for a user (reusing an open or still valid document).
async fn create_link(_c: ApiClient, State(st): State<AppState>, Json(req): Json<LinkReq>) -> AppResult<Json<Value>> {
    let t = get_template(&st.pool, &req.template).await?;
    let p = upsert_discord_person(&st.pool, &req.discord_id, req.name.as_deref(), req.username.as_deref(), req.zip.as_deref()).await?;
    let (sub, created) = find_or_create_submission(&st.pool, &t, &p, req.force_new, None).await?;
    let mut v = submission_json(&st, &sub, &t).await?;
    v["created"] = json!(created);
    Ok(Json(v))
}

#[derive(Deserialize)]
struct LinkAccountsReq {
    old_discord_id: String,
    new_discord_id: String,
    new_username: Option<String>,
}

async fn link_accounts(_c: ApiClient, State(st): State<AppState>, Json(req): Json<LinkAccountsReq>) -> AppResult<Json<Value>> {
    let p = link_discord_accounts(&st.pool, &req.old_discord_id, &req.new_discord_id, req.new_username.as_deref()).await?;
    Ok(Json(person_view(&st, &p).await?))
}

async fn submission(_c: ApiClient, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let s = get_submission(&st.pool, &id).await?;
    let t = get_template(&st.pool, &s.template_id).await?;
    Ok(Json(submission_json(&st, &s, &t).await?))
}

async fn submission_pdf(_c: ApiClient, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Response> {
    let s = get_submission(&st.pool, &id).await?;
    let (name, bytes) = render_submission(&st, &s).await?;
    Ok(pdf_response(bytes, &name, false))
}
