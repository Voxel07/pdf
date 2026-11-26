use std::time::Duration;
use anyhow::Context;

use axum::body::Body;
use axum::extract::{Extension, Path, Request};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, put};
use axum::{Json, Router};
use serde_json::Value;

use crate::auth::{AdminUser, SessionUser};
use crate::state::AppState;
use crate::util::{AppError, AppResult, sha256_hex};

#[derive(Clone)]
struct Calendar {
    client: reqwest::Client,
    origin: Option<reqwest::Url>,
    token: Option<String>,
    public_url: String,
}

fn unavailable() -> AppError {
    AppError::new(StatusCode::SERVICE_UNAVAILABLE, "events_unavailable", "The calendar service is unavailable")
}

impl Calendar {
    fn from_state(state: &AppState) -> Self {
        Self {
            client: reqwest::Client::builder().timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none()).build().expect("build calendar HTTP client"),
            origin: state.cfg.calendar_url.clone(),
            token: state.cfg.calendar_pdfsign_token.clone(),
            public_url: state.cfg.public_url.trim_end_matches('/').to_owned(),
        }
    }

    fn request(&self, method: Method, path: &str) -> AppResult<reqwest::RequestBuilder> {
        let token = self.token.as_deref().filter(|token| !token.is_empty()).ok_or_else(unavailable)?;
        let url = self.origin.as_ref().ok_or_else(unavailable)?
            .join(&format!("/internal/pdfsign/{path}")).map_err(|_| unavailable())?;
        Ok(self.client.request(method, url).bearer_auth(token)
            .header("X-Pdfsign-Public-Url", &self.public_url))
    }
}

pub async fn backup_snapshot(state: &AppState) -> anyhow::Result<Vec<u8>> {
    let response = Calendar::from_state(state).request(Method::GET, "backup")
        .map_err(|e| anyhow::anyhow!(e.message))?
        .timeout(Duration::from_secs(60))
        .send().await.map_err(reqwest::Error::without_url).context("requesting the calendar snapshot")?;
    anyhow::ensure!(response.status().is_success(), "calendar snapshot failed with HTTP {}", response.status());
    anyhow::ensure!(
        response.headers().get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok()) == Some("application/vnd.sqlite3"),
        "calendar service did not return a SQLite snapshot"
    );
    crate::util::read_limited(response, crate::backup::CALENDAR_MAX_BYTES).await
}

async fn forward(request: reqwest::RequestBuilder) -> AppResult<Response> {
    let response = request.send().await.map_err(|_| unavailable())?;
    let status = response.status();
    if status.is_server_error() {
        return Err(unavailable());
    }
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let body = response.bytes().await.map_err(|_| unavailable())?;
    let mut response = (status, Body::from(body)).into_response();
    if let Some(content_type) = content_type {
        response.headers_mut().insert(header::CONTENT_TYPE, content_type);
    }
    Ok(response)
}

pub fn disabled_router() -> Router<AppState> {
    let disabled = any(|| async { AppError::not_found("Event scheduling is disabled") });
    Router::new()
        .route("/api/admin/events", disabled.clone())
        .route("/api/admin/events/{*rest}", disabled.clone())
        .route("/api/events/{*rest}", disabled)
}

pub fn router(state: &AppState) -> Router<AppState> {
    let calendar = Calendar::from_state(state);
    Router::new()
        .route("/api/admin/events", get(session_list).post(session_create))
        .route("/api/admin/events/{id}", get(session_detail).put(session_update))
        .route("/api/admin/events/{id}/rsvp", put(session_rsvp))
        .route("/api/admin/events/credentials", get(list_credentials).post(issue_credential))
        .route("/api/admin/events/credentials/{id}", axum::routing::delete(revoke_credential))
        .route("/api/events/v1/events", get(integration_list).post(integration_create))
        .route("/api/events/v1/events/{id}", get(integration_detail).put(integration_update))
        .route("/api/events/calendar/{feed}", get(calendar_feed))
        .route("/api/events/health", get(health))
        .layer(Extension(calendar))
        .layer(middleware::from_fn(private_cache))
}

async fn private_cache(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    response
}

async fn health(Extension(calendar): Extension<Calendar>) -> AppResult<StatusCode> {
    let response = calendar.request(Method::GET, "health")?.send().await.map_err(|_| unavailable())?;
    if response.status() != StatusCode::NO_CONTENT {
        return Err(unavailable());
    }
    Ok(StatusCode::NO_CONTENT)
}

fn event_token(headers: &HeaderMap) -> AppResult<&str> {
    headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("bearer") && token_shape(token, "pdfe_"))
        .map(|(_, token)| token).ok_or_else(AppError::unauthorized)
}

fn token_shape(token: &str, prefix: &str) -> bool {
    token.strip_prefix(prefix).is_some_and(|value| value.len() == 48 && value.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn valid_id(id: &str) -> AppResult<String> {
    uuid::Uuid::parse_str(id).map(|id| id.to_string()).map_err(|_| AppError::not_found("Event not found"))
}

fn require_staff(user: &crate::auth::User) -> AppResult<()> {
    if !user.is_staff() {
        return Err(AppError::new(StatusCode::FORBIDDEN, "forbidden", "Signing staff access is required to change events"));
    }
    Ok(())
}

fn with_query(request: reqwest::RequestBuilder, uri: &axum::http::Uri) -> AppResult<reqwest::RequestBuilder> {
    let (client, request) = request.build_split();
    let mut request = request.map_err(|_| unavailable())?;
    request.url_mut().set_query(uri.query());
    Ok(reqwest::RequestBuilder::from_parts(client, request))
}

fn create_request(mut request: reqwest::RequestBuilder, headers: &HeaderMap, body: Value) -> reqwest::RequestBuilder {
    if let Some(key) = headers.get("idempotency-key") {
        request = request.header("Idempotency-Key", key);
    }
    request.json(&body)
}

async fn session_list(SessionUser(user): SessionUser, Extension(calendar): Extension<Calendar>, request: Request) -> AppResult<Response> {
    let upstream = calendar.request(Method::GET, "session/events")?.header("X-Pdfsign-User", sha256_hex(&user.sub));
    forward(with_query(upstream, request.uri())?).await
}

async fn session_detail(SessionUser(user): SessionUser, Extension(calendar): Extension<Calendar>, Path(id): Path<String>) -> AppResult<Response> {
    forward(calendar.request(Method::GET, &format!("session/events/{}", valid_id(&id)?))?
        .header("X-Pdfsign-User", sha256_hex(&user.sub))).await
}

async fn session_create(SessionUser(user): SessionUser, Extension(calendar): Extension<Calendar>, headers: HeaderMap, Json(body): Json<Value>) -> AppResult<Response> {
    require_staff(&user)?;
    let request = calendar.request(Method::POST, "session/events")?.header("X-Pdfsign-User", sha256_hex(&user.sub));
    forward(create_request(request, &headers, body)).await
}

async fn session_update(SessionUser(user): SessionUser, Extension(calendar): Extension<Calendar>, Path(id): Path<String>, Json(body): Json<Value>) -> AppResult<Response> {
    require_staff(&user)?;
    forward(calendar.request(Method::PUT, &format!("session/events/{}", valid_id(&id)?))?
        .header("X-Pdfsign-User", sha256_hex(&user.sub)).json(&body)).await
}

async fn session_rsvp(SessionUser(user): SessionUser, Extension(calendar): Extension<Calendar>, Path(id): Path<String>, Json(body): Json<Value>) -> AppResult<Response> {
    forward(calendar.request(Method::PUT, &format!("session/events/{}/rsvp", valid_id(&id)?))?
        .header("X-Pdfsign-User", sha256_hex(&user.sub)).json(&body)).await
}

async fn integration_list(Extension(calendar): Extension<Calendar>, request: Request) -> AppResult<Response> {
    let token = event_token(request.headers())?;
    let upstream = calendar.request(Method::GET, "integration/events")?.header("X-Pdfsign-Event-Token", token);
    forward(with_query(upstream, request.uri())?).await
}

async fn integration_detail(Extension(calendar): Extension<Calendar>, Path(id): Path<String>, headers: HeaderMap) -> AppResult<Response> {
    let token = event_token(&headers)?;
    forward(calendar.request(Method::GET, &format!("integration/events/{}", valid_id(&id)?))?
        .header("X-Pdfsign-Event-Token", token)).await
}

async fn integration_create(Extension(calendar): Extension<Calendar>, headers: HeaderMap, Json(body): Json<Value>) -> AppResult<Response> {
    let token = event_token(&headers)?;
    let request = calendar.request(Method::POST, "integration/events")?.header("X-Pdfsign-Event-Token", token);
    forward(create_request(request, &headers, body)).await
}

async fn integration_update(Extension(calendar): Extension<Calendar>, Path(id): Path<String>, headers: HeaderMap, Json(body): Json<Value>) -> AppResult<Response> {
    let token = event_token(&headers)?;
    forward(calendar.request(Method::PUT, &format!("integration/events/{}", valid_id(&id)?))?
        .header("X-Pdfsign-Event-Token", token).json(&body)).await
}

async fn list_credentials(_: AdminUser, Extension(calendar): Extension<Calendar>) -> AppResult<Response> {
    forward(calendar.request(Method::GET, "credentials")?).await
}

async fn issue_credential(AdminUser(user): AdminUser, Extension(calendar): Extension<Calendar>, Json(body): Json<Value>) -> AppResult<Response> {
    forward(calendar.request(Method::POST, "credentials")?.header("X-Pdfsign-User", user.id).json(&body)).await
}

async fn revoke_credential(_: AdminUser, Extension(calendar): Extension<Calendar>, Path(id): Path<String>) -> AppResult<Response> {
    forward(calendar.request(Method::DELETE, &format!("credentials/{}", valid_id(&id)?))?).await
}

async fn calendar_feed(Extension(calendar): Extension<Calendar>, Path(feed): Path<String>) -> AppResult<Response> {
    if !feed.strip_suffix(".ics").is_some_and(|token| token_shape(token, "pdfc_")) {
        return Err(AppError::not_found("Calendar feed not found"));
    }
    forward(calendar.request(Method::GET, &format!("feeds/{feed}"))?).await
}
