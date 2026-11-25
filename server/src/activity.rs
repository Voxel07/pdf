//! Activity log: recent actions and errors, shown in the admin UI under "Activity".
//!
//! Entries are best effort: writing one never fails the action that is being logged.
//! Only the most recent [`KEEP`] entries are kept.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use serde::Serialize;
use sqlx::{FromRow, SqlitePool};

use crate::state::AppState;
use crate::util::now;

/// Number of entries kept; older ones are pruned on insert.
const KEEP: i64 = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

#[derive(Debug, Serialize, FromRow)]
pub struct Entry {
    pub id: i64,
    pub created_at: String,
    pub level: String,
    pub category: String,
    pub actor: Option<String>,
    pub message: String,
}

/// Records an entry and mirrors it to the server log.
pub async fn log(pool: &SqlitePool, level: Level, category: &str, actor: Option<&str>, message: impl Into<String>) {
    let message: String = message.into();
    match level {
        Level::Info => tracing::info!(category, actor, "{message}"),
        Level::Warn => tracing::warn!(category, actor, "{message}"),
        Level::Error => tracing::error!(category, actor, "{message}"),
    }
    let message: String = message.chars().take(4000).collect();
    let res = sqlx::query("INSERT INTO activity_log (created_at, level, category, actor, message) VALUES (?, ?, ?, ?, ?)")
        .bind(now())
        .bind(level.as_str())
        .bind(category)
        .bind(actor)
        .bind(&message)
        .execute(pool)
        .await;
    match res {
        Ok(r) => {
            let id = r.last_insert_rowid();
            if id % 100 == 0 {
                let _ = sqlx::query("DELETE FROM activity_log WHERE id <= ?").bind(id - KEEP).execute(pool).await;
            }
        }
        Err(e) => tracing::warn!("could not write activity log entry: {e}"),
    }
}

pub async fn info(pool: &SqlitePool, category: &str, actor: Option<&str>, message: impl Into<String>) {
    log(pool, Level::Info, category, actor, message).await
}

pub async fn warn(pool: &SqlitePool, category: &str, actor: Option<&str>, message: impl Into<String>) {
    log(pool, Level::Warn, category, actor, message).await
}

pub async fn error(pool: &SqlitePool, category: &str, actor: Option<&str>, message: impl Into<String>) {
    log(pool, Level::Error, category, actor, message).await
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct Filter {
    /// `info` (everything), `warn` (warnings and errors) or `error`.
    pub level: Option<String>,
    pub category: Option<String>,
    pub q: Option<String>,
    /// Only entries older than this id (paging).
    pub before: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn list(pool: &SqlitePool, f: &Filter) -> sqlx::Result<Vec<Entry>> {
    let levels: &[&str] = match f.level.as_deref() {
        Some("error") => &["error"],
        Some("warn") => &["warn", "error"],
        _ => &["info", "warn", "error"],
    };
    let like = f.q.as_deref().map(str::trim).filter(|q| !q.is_empty()).map(|q| format!("%{q}%"));
    sqlx::query_as(
        "SELECT * FROM activity_log
         WHERE level IN (?1, ?2, ?3)
           AND (?4 IS NULL OR category = ?4)
           AND (?5 IS NULL OR message LIKE ?5 OR actor LIKE ?5)
           AND (?6 IS NULL OR id < ?6)
         ORDER BY id DESC LIMIT ?7",
    )
    .bind(levels[0])
    .bind(*levels.get(1).unwrap_or(&levels[0]))
    .bind(*levels.get(2).unwrap_or(&levels[0]))
    .bind(f.category.as_deref().filter(|c| !c.is_empty()))
    .bind(like)
    .bind(f.before)
    .bind(f.limit.unwrap_or(100).clamp(1, 500))
    .fetch_all(pool)
    .await
}

/// Message of a failed request, attached to the response by `AppError::into_response`.
#[derive(Clone)]
pub struct FailedRequest(pub String);

/// Hides signing link tokens (long alphanumeric segments) in a request path. Record IDs are
/// UUIDs (with dashes) and stay readable.
fn redact_path(path: &str) -> String {
    path.split('/')
        .map(|seg| if seg.len() >= 12 && seg.chars().all(|c| c.is_ascii_alphanumeric()) { "…" } else { seg })
        .collect::<Vec<_>>()
        .join("/")
}

/// Records every request that ended in a server error (5xx) in the activity log.
pub async fn record_errors(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let what = format!("{} {}", req.method(), redact_path(req.uri().path()));
    let res = next.run(req).await;
    if res.status().is_server_error() {
        let detail = res.extensions().get::<FailedRequest>().map(|f| f.0.clone()).unwrap_or_else(|| res.status().to_string());
        error(&state.pool, "server", None, format!("{what} failed: {detail}")).await;
    }
    res
}
