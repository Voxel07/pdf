//! Admin API used by the web UI (OIDC session cookie; see `Staff` / `AdminUser` in auth.rs).

use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{create_api_key, AdminUser, Staff};
use crate::backup;
use crate::docuseal;
use crate::models::*;
use crate::pdf::{self, DrawItem, DrawValue};
use crate::routes::public::pdf_response;
use crate::service::{self, *};
use crate::state::AppState;
use crate::util::*;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/stats", get(stats))
        .route("/users", get(users))
        .route("/templates", get(templates).post(upload_template))
        .route("/templates/{id}", get(template).put(update).delete(archive_template))
        .route("/templates/{id}/pdf", get(template_pdf))
        .route("/templates/{id}/preview.pdf", get(preview_pdf))
        .route("/templates/{id}/links", get(links).post(create_link))
        .route("/links/{id}", axum::routing::put(update_link).delete(delete_link))
        .route("/links/{id}/qr.svg", get(link_qr))
        .route("/submissions", get(submissions).post(create_submission_admin))
        .route("/submissions/{id}", get(submission).delete(delete_submission))
        .route("/submissions/{id}/pdf", get(submission_pdf))
        .route("/submissions/{id}/revoke", post(revoke))
        .route("/submissions/{id}/reset-lock", post(reset_lock))
        .route("/submissions/{id}/rotate-token", post(rotate_token))
        .route("/persons", get(persons).post(create_person_admin))
        .route("/persons/merge", post(merge))
        .route("/persons/link-discord", post(link_discord))
        .route("/persons/{id}", get(person).put(update_person).delete(delete_person))
        .route("/persons/{id}/discord", post(add_discord))
        .route("/discord/{discord_id}", delete(remove_discord))
        .route("/api-keys", get(api_keys).post(new_api_key))
        .route("/api-keys/{id}", delete(delete_api_key))
        .route("/docuseal/templates", post(docuseal_templates))
        .route("/docuseal/import", post(docuseal_import))
        .route("/backup", get(backup_overview))
        .route("/backup/config", axum::routing::put(backup_config))
        .route("/backup/run", post(backup_run))
        .route("/backup/files/{name}", get(backup_download).delete(backup_delete))
        .route("/export", post(export_disk))
        .route("/export.zip", get(export_zip))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
}

async fn me(Staff(u): Staff, State(st): State<AppState>) -> Json<Value> {
    Json(json!({ "user": u, "public_url": st.cfg.public_url, "export_dir": st.cfg.export_dir, "version": crate::version() }))
}

async fn users(_u: AdminUser, State(st): State<AppState>) -> AppResult<Json<Value>> {
    let list: Vec<crate::auth::User> = sqlx::query_as("SELECT * FROM users ORDER BY last_login_at DESC").fetch_all(&st.pool).await?;
    Ok(Json(json!(list)))
}

async fn stats(_u: Staff, State(st): State<AppState>) -> AppResult<Json<Value>> {
    let count = |q: &'static str| {
        let pool = st.pool.clone();
        async move { sqlx::query_as::<_, (i64,)>(q).fetch_one(&pool).await.map(|r| r.0) }
    };
    Ok(Json(json!({
        "templates": count("SELECT COUNT(*) FROM templates WHERE archived = 0").await?,
        "persons": count("SELECT COUNT(*) FROM persons").await?,
        "pending": count("SELECT COUNT(*) FROM submissions WHERE status = 'pending'").await?,
        "completed": count("SELECT COUNT(*) FROM submissions WHERE status = 'completed'").await?,
    })))
}

// ---------------------------------------------------------------------------------------
// Templates

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    archived: bool,
}

async fn templates(_u: Staff, State(st): State<AppState>, Query(q): Query<ListQuery>) -> AppResult<Json<Value>> {
    let list = list_templates(&st.pool, q.archived).await?;
    let mut out = Vec::new();
    for t in list {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM submissions WHERE template_id = ?").bind(&t.id).fetch_one(&st.pool).await?;
        let mut v = serde_json::to_value(&t).unwrap();
        v["submission_count"] = json!(n);
        out.push(v);
    }
    Ok(Json(json!(out)))
}

async fn upload_template(_u: Staff, State(st): State<AppState>, mut mp: Multipart) -> AppResult<Json<Template>> {
    let mut name = None;
    let mut pdf_bytes = None;
    while let Some(field) = mp.next_field().await.map_err(|e| AppError::bad_request(e.to_string()))? {
        match field.name() {
            Some("name") => name = Some(field.text().await.map_err(|e| AppError::bad_request(e.to_string()))?),
            Some("file") => {
                let fname = field.file_name().map(String::from);
                let data = field.bytes().await.map_err(|e| AppError::bad_request(e.to_string()))?;
                if name.as_deref().is_none_or(str::is_empty) {
                    name = fname.map(|f| f.trim_end_matches(".pdf").trim_end_matches(".PDF").to_string());
                }
                pdf_bytes = Some(data.to_vec());
            }
            _ => {}
        }
    }
    let pdf_bytes = pdf_bytes.ok_or_else(|| AppError::bad_request("missing file"))?;
    let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Untitled".into());
    Ok(Json(create_template(&st, name.trim(), &pdf_bytes, vec![], vec![], None).await?))
}

async fn template(_u: Staff, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Template>> {
    Ok(Json(get_template(&st.pool, &id).await?))
}

async fn update(
    _u: Staff,
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(up): Json<TemplateUpdate>,
) -> AppResult<Json<Template>> {
    Ok(Json(update_template(&st, &id, up).await?))
}

async fn archive_template(_u: Staff, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let t = get_template(&st.pool, &id).await?;
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM submissions WHERE template_id = ?").bind(&t.id).fetch_one(&st.pool).await?;
    if n == 0 {
        sqlx::query("DELETE FROM templates WHERE id = ?").bind(&t.id).execute(&st.pool).await?;
        let _ = tokio::fs::remove_file(st.template_pdf_path(&t.id)).await;
        Ok(Json(json!({ "deleted": true })))
    } else {
        sqlx::query("UPDATE templates SET archived = 1 WHERE id = ?").bind(&t.id).execute(&st.pool).await?;
        Ok(Json(json!({ "archived": true })))
    }
}

async fn template_pdf(_u: Staff, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Response> {
    let t = get_template(&st.pool, &id).await?;
    Ok(pdf_response(service::template_pdf(&st, &t.id).await?, &format!("{}.pdf", t.slug), true))
}

/// Renders the template with sample values so positions can be checked.
async fn preview_pdf(_u: Staff, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Response> {
    let t = get_template(&st.pool, &id).await?;
    let bytes = service::template_pdf(&st, &t.id).await?;
    let sig = sample_signature();
    let items: Vec<DrawItem> = t
        .fields
        .iter()
        .map(|f| DrawItem {
            field: f,
            value: match f.kind {
                FieldKind::Signature => DrawValue::Image(sig.clone()),
                FieldKind::Date => DrawValue::Text(format_date("2025-12-31", f.format.as_deref())),
                FieldKind::Number => DrawValue::Text("42".into()),
                FieldKind::Select => DrawValue::Text(f.options.first().cloned().unwrap_or_else(|| f.name.clone())),
                FieldKind::Text => DrawValue::Text(f.name.clone()),
            },
        })
        .collect();
    let out = pdf::render(&bytes, &items, None)?;
    Ok(pdf_response(out, &format!("{}-preview.pdf", t.slug), true))
}

fn sample_signature() -> Vec<u8> {
    let mut img = image::RgbaImage::new(300, 100);
    for x in 10..290u32 {
        let y = (50.0 + 30.0 * ((x as f64) / 25.0).sin()) as u32;
        for dy in 0..4 {
            img.put_pixel(x, (y + dy).min(99), image::Rgba([20, 30, 120, 255]));
        }
    }
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}

// ---------------------------------------------------------------------------------------
// General links + QR codes

fn link_json(st: &AppState, l: &PublicLink) -> Value {
    let mut v = serde_json::to_value(l).unwrap();
    v["url"] = json!(st.public_link_url(&l.token));
    v
}

async fn links(_u: Staff, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let t = get_template(&st.pool, &id).await?;
    let list: Vec<PublicLink> =
        sqlx::query_as("SELECT * FROM public_links WHERE template_id = ? ORDER BY created_at DESC").bind(&t.id).fetch_all(&st.pool).await?;
    Ok(Json(json!(list.iter().map(|l| link_json(&st, l)).collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct LinkBody {
    label: Option<String>,
    active: Option<bool>,
    /// RFC3339 or YYYY-MM-DD (end of that day, UTC); empty string clears it.
    expires_at: Option<String>,
}

fn parse_expiry(s: &str) -> AppResult<Option<String>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if let Some(ts) = parse_ts(s) {
        return Ok(Some(ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)));
    }
    let d = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| AppError::bad_request("invalid expiry date"))?;
    Ok(Some(format!("{}T23:59:59Z", d.format("%Y-%m-%d"))))
}

async fn create_link(Staff(u): Staff, State(st): State<AppState>, Path(id): Path<String>, Json(b): Json<LinkBody>) -> AppResult<Json<Value>> {
    let t = get_template(&st.pool, &id).await?;
    let lid = new_id();
    sqlx::query("INSERT INTO public_links (id, template_id, token, label, active, expires_at, created_by, created_at) VALUES (?, ?, ?, ?, 1, ?, ?, ?)")
        .bind(&lid)
        .bind(&t.id)
        .bind(random_token(12))
        .bind(b.label.unwrap_or_default().trim())
        .bind(parse_expiry(b.expires_at.as_deref().unwrap_or(""))?)
        .bind(&u.username)
        .bind(now())
        .execute(&st.pool)
        .await?;
    let l: PublicLink = sqlx::query_as("SELECT * FROM public_links WHERE id = ?").bind(&lid).fetch_one(&st.pool).await?;
    Ok(Json(link_json(&st, &l)))
}

async fn update_link(_u: Staff, State(st): State<AppState>, Path(id): Path<String>, Json(b): Json<LinkBody>) -> AppResult<Json<Value>> {
    let mut l: PublicLink = sqlx::query_as("SELECT * FROM public_links WHERE id = ?").bind(&id).fetch_one(&st.pool).await?;
    if let Some(label) = b.label {
        l.label = label.trim().to_string();
    }
    if let Some(a) = b.active {
        l.active = a;
    }
    if let Some(e) = b.expires_at {
        l.expires_at = parse_expiry(&e)?;
    }
    sqlx::query("UPDATE public_links SET label = ?, active = ?, expires_at = ? WHERE id = ?")
        .bind(&l.label)
        .bind(l.active)
        .bind(&l.expires_at)
        .bind(&id)
        .execute(&st.pool)
        .await?;
    Ok(Json(link_json(&st, &l)))
}

async fn delete_link(_u: Staff, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    sqlx::query("DELETE FROM public_links WHERE id = ?").bind(&id).execute(&st.pool).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn link_qr(_u: Staff, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Response> {
    let l: PublicLink = sqlx::query_as("SELECT * FROM public_links WHERE id = ?").bind(&id).fetch_one(&st.pool).await?;
    let code = qrcode::QrCode::with_error_correction_level(st.public_link_url(&l.token), qrcode::EcLevel::M)
        .map_err(|e| anyhow::anyhow!("QR code: {e:?}"))?;
    let svg = code.render::<qrcode::render::svg::Color>().min_dimensions(480, 480).quiet_zone(true).build();
    Ok(([(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "private, max-age=300")], svg).into_response())
}

// ---------------------------------------------------------------------------------------
// Submissions

#[derive(Deserialize)]
struct SubQuery {
    template_id: Option<String>,
    person_id: Option<String>,
    status: Option<String>,
    q: Option<String>,
}

async fn submissions(_u: Staff, State(st): State<AppState>, Query(q): Query<SubQuery>) -> AppResult<Json<Value>> {
    let like = q.q.as_ref().map(|s| format!("%{}%", s.trim()));
    let subs: Vec<Submission> = sqlx::query_as(
        "SELECT s.* FROM submissions s JOIN persons p ON p.id = s.person_id
         WHERE (?1 IS NULL OR s.template_id = ?1) AND (?2 IS NULL OR s.person_id = ?2)
           AND (?3 IS NULL OR s.status = ?3)
           AND (?4 IS NULL OR p.name LIKE ?4 OR EXISTS (SELECT 1 FROM discord_accounts d WHERE d.person_id = p.id AND (d.discord_id LIKE ?4 OR d.username LIKE ?4)))
         ORDER BY s.updated_at DESC LIMIT 500",
    )
    .bind(&q.template_id)
    .bind(&q.person_id)
    .bind(q.status.as_deref().filter(|s| *s != "expired" && !s.is_empty()))
    .bind(like)
    .fetch_all(&st.pool)
    .await?;
    let mut cache: std::collections::HashMap<String, Template> = Default::default();
    let mut out = Vec::new();
    for s in subs {
        if !cache.contains_key(&s.template_id) {
            cache.insert(s.template_id.clone(), get_template(&st.pool, &s.template_id).await?);
        }
        let v = submission_json(&st, &s, &cache[&s.template_id]).await?;
        if q.status.as_deref() == Some("expired") && v["status"] != "expired" {
            continue;
        }
        out.push(v);
    }
    Ok(Json(json!(out)))
}

#[derive(Deserialize)]
struct CreateSubReq {
    template_id: String,
    person_id: String,
    #[serde(default)]
    force_new: bool,
}

async fn create_submission_admin(_u: Staff, State(st): State<AppState>, Json(req): Json<CreateSubReq>) -> AppResult<Json<Value>> {
    let t = get_template(&st.pool, &req.template_id).await?;
    let p = get_person(&st.pool, &req.person_id).await?;
    let (s, created) = find_or_create_submission(&st.pool, &t, &p, req.force_new, None).await?;
    let mut v = submission_json(&st, &s, &t).await?;
    v["created"] = json!(created);
    Ok(Json(v))
}

async fn submission(_u: Staff, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let s = get_submission(&st.pool, &id).await?;
    let t = get_template(&st.pool, &s.template_id).await?;
    let mut v = submission_json(&st, &s, &t).await?;
    let values = load_values(&st.pool, &s.id).await?;
    let b64 = base64::engine::general_purpose::STANDARD;
    v["values"] = json!(values
        .iter()
        .map(|fv| {
            let f = t.fields.iter().find(|f| f.id == fv.field_id);
            json!({
                "field_id": fv.field_id,
                "field_name": f.map(|f| f.name.clone()),
                "type": f.map(|f| f.kind),
                "section": f.map(|f| f.section),
                "value": fv.value,
                "image": fv.image.as_ref().map(|i| format!("data:image/png;base64,{}", b64.encode(i))),
                "round_id": fv.round_id,
            })
        })
        .collect::<Vec<_>>());
    v["rounds"] = json!(load_rounds(&st.pool, &s.id).await?);
    v["sections"] = json!(t.sections);
    v["discord_accounts"] = json!(discord_accounts(&st.pool, &s.person_id).await?);
    Ok(Json(v))
}

async fn submission_pdf(_u: Staff, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Response> {
    let s = get_submission(&st.pool, &id).await?;
    let (name, bytes) = render_submission(&st, &s).await?;
    Ok(pdf_response(bytes, &name, false))
}

async fn revoke(_u: Staff, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    revoke_submission(&st.pool, &id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn reset_lock(_u: Staff, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    sqlx::query("UPDATE submissions SET failed_attempts = 0 WHERE id = ?").bind(&id).execute(&st.pool).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn rotate_token(_u: Staff, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let s = rotate_submission_token(&st.pool, &id).await?;
    let t = get_template(&st.pool, &s.template_id).await?;
    Ok(Json(submission_json(&st, &s, &t).await?))
}

async fn delete_submission(_u: AdminUser, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    sqlx::query("DELETE FROM submissions WHERE id = ?").bind(&id).execute(&st.pool).await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------------------
// Persons

#[derive(Deserialize)]
struct PersonQuery {
    q: Option<String>,
}

async fn persons(_u: Staff, State(st): State<AppState>, Query(q): Query<PersonQuery>) -> AppResult<Json<Value>> {
    let like = q.q.as_ref().filter(|s| !s.trim().is_empty()).map(|s| format!("%{}%", s.trim()));
    let list: Vec<Person> = sqlx::query_as(
        "SELECT p.* FROM persons p WHERE ?1 IS NULL OR p.name LIKE ?1 OR p.email LIKE ?1
           OR EXISTS (SELECT 1 FROM discord_accounts d WHERE d.person_id = p.id AND (d.discord_id LIKE ?1 OR d.username LIKE ?1))
         ORDER BY p.name COLLATE NOCASE LIMIT 1000",
    )
    .bind(like)
    .fetch_all(&st.pool)
    .await?;
    let mut out = Vec::new();
    for p in list {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM submissions WHERE person_id = ?").bind(&p.id).fetch_one(&st.pool).await?;
        let accounts = discord_accounts(&st.pool, &p.id).await?;
        let mut v = serde_json::to_value(&p).unwrap();
        v["discord_accounts"] = json!(accounts);
        v["submission_count"] = json!(n);
        out.push(v);
    }
    Ok(Json(json!(out)))
}

async fn person(_u: Staff, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let p = get_person(&st.pool, &id).await?;
    let mut v = serde_json::to_value(&p).unwrap();
    v["discord_accounts"] = json!(discord_accounts(&st.pool, &p.id).await?);
    Ok(Json(v))
}

#[derive(Deserialize)]
struct PersonReq {
    name: String,
    email: Option<String>,
    zip: Option<String>,
    discord_id: Option<String>,
    notes: Option<String>,
}

async fn create_person_admin(_u: Staff, State(st): State<AppState>, Json(req): Json<PersonReq>) -> AppResult<Json<Person>> {
    let p = create_person(&st.pool, &req.name, req.email.as_deref().filter(|e| !e.is_empty()), req.zip.as_deref()).await?;
    if let Some(d) = req.discord_id.as_deref().filter(|d| !d.is_empty()) {
        if let Err(e) = attach_discord(&st.pool, &p.id, d, None).await {
            sqlx::query("DELETE FROM persons WHERE id = ?").bind(&p.id).execute(&st.pool).await?;
            return Err(e);
        }
    }
    Ok(Json(p))
}

async fn update_person(_u: Staff, State(st): State<AppState>, Path(id): Path<String>, Json(req): Json<PersonReq>) -> AppResult<Json<Person>> {
    let name = validate_person_name(&req.name)?;
    let zip = normalize_zip(req.zip.as_deref())?;
    sqlx::query("UPDATE persons SET name = ?, email = ?, zip = ?, notes = COALESCE(?, notes) WHERE id = ?")
        .bind(name)
        .bind(req.email.filter(|e| !e.is_empty()))
        .bind(zip)
        .bind(req.notes)
        .bind(&id)
        .execute(&st.pool)
        .await?;
    Ok(Json(get_person(&st.pool, &id).await?))
}

async fn delete_person(_u: AdminUser, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    sqlx::query("DELETE FROM persons WHERE id = ?").bind(&id).execute(&st.pool).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct DiscordReq {
    discord_id: String,
    username: Option<String>,
}

async fn add_discord(_u: Staff, State(st): State<AppState>, Path(id): Path<String>, Json(req): Json<DiscordReq>) -> AppResult<Json<Value>> {
    get_person(&st.pool, &id).await?;
    // if the Discord account already belongs to someone else, merge that person into this one
    if let Some(other) = person_by_discord(&st.pool, &req.discord_id).await? {
        if other.id != id {
            merge_persons(&st.pool, &other.id, &id).await?;
        }
    }
    attach_discord(&st.pool, &id, req.discord_id.trim(), req.username.as_deref()).await?;
    Ok(Json(json!({ "discord_accounts": discord_accounts(&st.pool, &id).await? })))
}

async fn remove_discord(_u: Staff, State(st): State<AppState>, Path(did): Path<String>) -> AppResult<Json<Value>> {
    sqlx::query("DELETE FROM discord_accounts WHERE discord_id = ?").bind(&did).execute(&st.pool).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct MergeReq {
    from: String,
    into: String,
}

async fn merge(_u: Staff, State(st): State<AppState>, Json(req): Json<MergeReq>) -> AppResult<Json<Person>> {
    Ok(Json(merge_persons(&st.pool, &req.from, &req.into).await?))
}

#[derive(Deserialize)]
struct LinkReq {
    old_discord_id: String,
    new_discord_id: String,
}

async fn link_discord(_u: Staff, State(st): State<AppState>, Json(req): Json<LinkReq>) -> AppResult<Json<Person>> {
    Ok(Json(link_discord_accounts(&st.pool, req.old_discord_id.trim(), req.new_discord_id.trim(), None).await?))
}

// ---------------------------------------------------------------------------------------
// API keys

async fn api_keys(_u: AdminUser, State(st): State<AppState>) -> AppResult<Json<Value>> {
    let rows: Vec<(String, String, Option<String>, String, Option<String>)> = sqlx::query_as(
        "SELECT k.id, k.name, COALESCE(u.username, k.created_by), k.created_at, k.last_used_at FROM api_keys k LEFT JOIN users u ON u.id = k.created_by ORDER BY k.created_at",
    )
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(id, name, by, created_at, last_used_at)| json!({ "id": id, "name": name, "created_by": by, "created_at": created_at, "last_used_at": last_used_at }))
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct KeyReq {
    name: String,
}

async fn new_api_key(AdminUser(u): AdminUser, State(st): State<AppState>, Json(req): Json<KeyReq>) -> AppResult<Json<Value>> {
    let (id, key) = create_api_key(&st, req.name.trim(), Some(&u.id)).await?;
    Ok(Json(json!({ "id": id, "key": key })))
}

async fn delete_api_key(_u: AdminUser, State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    sqlx::query("DELETE FROM api_keys WHERE id = ?").bind(&id).execute(&st.pool).await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------------------
// DocuSeal import

async fn docuseal_templates(_u: AdminUser, Json(cfg): Json<docuseal::Connection>) -> AppResult<Json<Value>> {
    let client = docuseal::Client::new(cfg)?;
    Ok(Json(json!(client.list_templates().await?)))
}

async fn docuseal_import(_u: AdminUser, State(st): State<AppState>, Json(req): Json<docuseal::ImportRequest>) -> AppResult<Json<Value>> {
    let report = docuseal::import(&st, req).await?;
    Ok(Json(json!(report)))
}

// ---------------------------------------------------------------------------------------
// Export

#[derive(Deserialize, Default)]
struct ExportReq {
    dir: Option<String>,
}

async fn export_disk(_u: AdminUser, State(st): State<AppState>, body: Option<Json<ExportReq>>) -> AppResult<Json<Value>> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let dir = req.dir.filter(|d| !d.trim().is_empty()).map(std::path::PathBuf::from).unwrap_or_else(|| st.cfg.export_dir.clone());
    Ok(Json(json!(export_all(&st, &dir).await?)))
}

async fn export_zip(_u: Staff, State(st): State<AppState>) -> AppResult<Response> {
    let (bytes, report) = service::export_zip(&st).await?;
    let name = format!("signatures-{}.zip", chrono::Utc::now().format("%Y-%m-%d"));
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
            // failed document IDs are listed in FAILED.txt inside the archive
            (header::HeaderName::from_static("x-export-failed"), report.failed.len().to_string()),
        ],
        bytes,
    )
        .into_response())
}

// ---------------------------------------------------------------------------------------
// Backups (admin only)

async fn backup_overview(_u: AdminUser, State(st): State<AppState>) -> AppResult<Json<Value>> {
    let cfg = backup::load_config(&st).await?;
    let status = backup::load_status(&st).await?;
    let files = backup::list_backups(std::path::Path::new(&cfg.dir));
    Ok(Json(json!({
        "config": cfg, "status": status, "files": files,
        "default_dir": st.cfg.backup_dir, "timezone": chrono::Local::now().format("server time, UTC%:z").to_string(),
        "running": st.backup_running.try_lock().is_err(),
    })))
}

async fn backup_config(_u: AdminUser, State(st): State<AppState>, Json(cfg): Json<backup::BackupConfig>) -> AppResult<Json<Value>> {
    let (cfg, status) = backup::save_config(&st, cfg).await?;
    Ok(Json(json!({ "config": cfg, "status": status })))
}

async fn backup_run(AdminUser(u): AdminUser, State(st): State<AppState>) -> AppResult<Json<Value>> {
    let status = backup::run_backup(&st, &format!("manual ({})", u.username)).await?;
    Ok(Json(json!(status)))
}

async fn backup_download(_u: AdminUser, State(st): State<AppState>, Path(name): Path<String>) -> AppResult<Response> {
    let cfg = backup::load_config(&st).await?;
    let path = backup::backup_path(&cfg.dir, &name)?;
    let bytes = tokio::fs::read(&path).await.map_err(|_| AppError::not_found("backup not found"))?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        bytes,
    )
        .into_response())
}

async fn backup_delete(_u: AdminUser, State(st): State<AppState>, Path(name): Path<String>) -> AppResult<Json<Value>> {
    let cfg = backup::load_config(&st).await?;
    tokio::fs::remove_file(backup::backup_path(&cfg.dir, &name)?).await.map_err(|_| AppError::not_found("backup not found"))?;
    Ok(Json(json!({ "ok": true })))
}
