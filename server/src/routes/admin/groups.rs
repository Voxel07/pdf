use axum::extract::{Path, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;

use crate::auth::Staff;
use crate::state::AppState;
use crate::util::{new_id, AppError, AppResult};

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/groups", get(groups).post(create))
        .route("/groups/{id}/requirements", get(requirements))
        .route("/groups/{group}/requirements/{template}", put(require).delete(unrequire))
        .route("/groups/{group}/members/{person}", put(add_member).delete(remove_member))
}

#[derive(FromRow, Serialize)]
struct Group {
    id: String,
    name: String,
}

async fn groups(_: Staff, State(st): State<AppState>) -> AppResult<Json<Vec<Group>>> {
    Ok(Json(sqlx::query_as("SELECT id, name FROM person_groups ORDER BY name").fetch_all(&st.pool).await?))
}

#[derive(Deserialize)]
struct GroupName {
    name: String,
}

async fn create(_: Staff, State(st): State<AppState>, Json(req): Json<GroupName>) -> AppResult<Json<Group>> {
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(AppError::bad_request("Enter a group name of 1 to 100 characters"));
    }
    let group = sqlx::query_as("INSERT INTO person_groups (id, name) VALUES (?, ?) ON CONFLICT(name) DO NOTHING RETURNING id, name")
        .bind(new_id()).bind(name).fetch_optional(&st.pool).await?
        .ok_or_else(|| AppError::conflict("duplicate_group", "A group with this name already exists"))?;
    Ok(Json(group))
}

async fn add_member(_: Staff, State(st): State<AppState>, Path((group, person)): Path<(String, String)>) -> AppResult<Json<Value>> {
    sqlx::query("INSERT INTO group_members (group_id, person_id) VALUES (?, ?) ON CONFLICT DO NOTHING")
        .bind(group).bind(person).execute(&st.pool).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn remove_member(_: Staff, State(st): State<AppState>, Path((group, person)): Path<(String, String)>) -> AppResult<Json<Value>> {
    sqlx::query("DELETE FROM group_members WHERE group_id = ? AND person_id = ?")
        .bind(group).bind(person).execute(&st.pool).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn require(_: Staff, State(st): State<AppState>, Path((group, template)): Path<(String, String)>) -> AppResult<Json<Value>> {
    sqlx::query("INSERT INTO group_requirements (group_id, template_id) VALUES (?, ?) ON CONFLICT DO NOTHING")
        .bind(group).bind(template).execute(&st.pool).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn unrequire(_: Staff, State(st): State<AppState>, Path((group, template)): Path<(String, String)>) -> AppResult<Json<Value>> {
    sqlx::query("DELETE FROM group_requirements WHERE group_id = ? AND template_id = ?")
        .bind(group).bind(template).execute(&st.pool).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(FromRow)]
struct Requirement {
    id: String,
    name: String,
    archived: bool,
    missing: String,
}

async fn requirements(_: Staff, State(st): State<AppState>, Path(group): Path<String>) -> AppResult<Json<Value>> {
    let rows: Vec<Requirement> = sqlx::query_as(
        "SELECT t.id, t.name, t.archived,
           (SELECT json_group_array(json_object('id', p.id, 'name', p.name, 'zip', p.zip,
               'status', COALESCE((SELECT s.effective_status FROM document_status s
                       WHERE s.person_id = p.id AND s.template_id = t.id
                       ORDER BY s.updated_at DESC, s.id DESC LIMIT 1), 'unissued'))
               ORDER BY p.name COLLATE NOCASE, p.id)
            FROM group_members m JOIN persons p ON p.id = m.person_id WHERE m.group_id = r.group_id
            AND NOT EXISTS (SELECT 1 FROM document_status s WHERE s.person_id = p.id
                            AND s.template_id = t.id AND s.effective_status = 'completed')) AS missing
         FROM group_requirements r JOIN templates t ON t.id = r.template_id
         WHERE r.group_id = ? ORDER BY t.name COLLATE NOCASE, t.id"
    ).bind(group).fetch_all(&st.pool).await?;
    let mut out = Vec::new();
    for row in rows {
        let missing: Value = serde_json::from_str(&row.missing).map_err(anyhow::Error::from)?;
        out.push(json!({ "id": row.id, "name": row.name, "archived": row.archived, "missing": missing }));
    }
    Ok(Json(json!(out)))
}
