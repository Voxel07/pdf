//! Authentication:
//! * Admin UI users log in via OpenID Connect (authentik). Their role (`admin` / `staff`) is
//!   derived from group membership on every login. Sessions are HttpOnly cookies.
//! * Bots / integrations use API keys generated in the app (`Authorization: Bearer ...`).
//! * Personal signing links (`/s/<token>`, token + name check) and general links
//!   (`/p/<token>`, name + zip identification) are the only unauthenticated pages.

use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use base64::Engine;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use tokio::sync::OnceCell;

use crate::state::AppState;
use crate::util::*;

pub const SESSION_COOKIE: &str = "pdfsign_session";
/// Binds a login's `state` to the browser that started it (login CSRF protection).
const STATE_COOKIE: &str = "pdfsign_oidc_state";
const SESSION_HOURS: i64 = 12;

#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub scopes: String,
    pub admin_group: String,
    pub staff_group: String,
    pub discovery: OnceCell<Discovery>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    #[serde(default)]
    pub issuer: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub userinfo_endpoint: String,
    pub end_session_endpoint: Option<String>,
}

impl OidcConfig {
    async fn discovery(&self) -> AppResult<&Discovery> {
        self.discovery
            .get_or_try_init(|| async {
                let url = format!("{}/.well-known/openid-configuration", self.issuer.trim_end_matches('/'));
                let d: Discovery = reqwest::get(&url)
                    .await
                    .and_then(|r| r.error_for_status())
                    .map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, "oidc", format!("OIDC discovery failed ({url}): {e}")))?
                    .json()
                    .await
                    .map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, "oidc", format!("invalid OIDC discovery: {e}")))?;
                Ok::<_, AppError>(d)
            })
            .await
    }
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct User {
    pub id: String,
    pub sub: String,
    pub username: String,
    pub name: String,
    pub email: Option<String>,
    pub role: String,
    pub created_at: String,
    pub last_login_at: String,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

pub async fn session_user(state: &AppState, headers: &HeaderMap) -> AppResult<Option<User>> {
    let Some(token) = cookie_value(headers, SESSION_COOKIE) else { return Ok(None) };
    let user: Option<User> = sqlx::query_as(
        "SELECT u.* FROM user_sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ? AND s.expires_at > ?",
    )
    .bind(sha256_hex(&token))
    .bind(Utc::now().to_rfc3339())
    .fetch_optional(&state.pool)
    .await?;
    Ok(user)
}

/// Any logged-in user with the `staff` or `admin` role.
pub struct Staff(pub User);

impl FromRequestParts<AppState> for Staff {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        match session_user(state, &parts.headers).await? {
            Some(u) => Ok(Staff(u)),
            None => Err(AppError::unauthorized()),
        }
    }
}

/// Logged-in user with the `admin` role.
pub struct AdminUser(pub User);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let Staff(u) = Staff::from_request_parts(parts, state).await?;
        if !u.is_admin() {
            return Err(AppError::new(StatusCode::FORBIDDEN, "forbidden", "admin role required"));
        }
        Ok(AdminUser(u))
    }
}

/// Integration API (bots): an API key created in the admin UI.
pub struct ApiClient {
    #[allow(dead_code)]
    pub key_name: String,
}

impl FromRequestParts<AppState> for ApiClient {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer ")))
            .map(|s| s.trim().to_string())
            .ok_or_else(AppError::unauthorized)?;
        let row: Option<(String, String)> = sqlx::query_as("SELECT id, name FROM api_keys WHERE key_hash = ?")
            .bind(sha256_hex(token.as_bytes()))
            .fetch_optional(&state.pool)
            .await?;
        let Some((id, name)) = row else { return Err(AppError::unauthorized()) };
        let _ = sqlx::query("UPDATE api_keys SET last_used_at = ? WHERE id = ?").bind(now()).bind(id).execute(&state.pool).await;
        Ok(ApiClient { key_name: name })
    }
}

pub async fn create_api_key(state: &AppState, name: &str, created_by: Option<&str>) -> AppResult<(String, String)> {
    if name.trim().is_empty() {
        return Err(AppError::bad_request("name required"));
    }
    let key = format!("pdfs_{}", random_token(40));
    let id = new_id();
    sqlx::query("INSERT INTO api_keys (id, name, key_hash, created_by, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(name.trim())
        .bind(sha256_hex(key.as_bytes()))
        .bind(created_by)
        .bind(now())
        .execute(&state.pool)
        .await?;
    Ok((id, key))
}

// ---------------------------------------------------------------------------------------
// OIDC login flow

pub fn router() -> Router<AppState> {
    Router::new().route("/login", get(login)).route("/callback", get(callback)).route("/logout", get(logout))
}

fn oidc(state: &AppState) -> AppResult<&OidcConfig> {
    state.cfg.oidc.as_ref().ok_or_else(|| {
        AppError::new(StatusCode::SERVICE_UNAVAILABLE, "oidc_not_configured", "OIDC is not configured (set OIDC_ISSUER, OIDC_CLIENT_ID, OIDC_CLIENT_SECRET)")
    })
}

fn redirect_uri(state: &AppState) -> String {
    format!("{}/auth/callback", state.cfg.public_url.trim_end_matches('/'))
}

fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.starts_with("//") && !n.contains('\\') => n.to_string(),
        _ => "/admin".into(),
    }
}

#[derive(Deserialize)]
struct LoginQuery {
    next: Option<String>,
}

async fn login(State(st): State<AppState>, Query(q): Query<LoginQuery>) -> AppResult<Response> {
    let cfg = oidc(&st)?;
    let disc = cfg.discovery().await?;
    let state_token = random_token(32);
    let verifier = random_token(64);
    let nonce = random_token(24);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let cutoff = (Utc::now() - Duration::minutes(15)).to_rfc3339();
    sqlx::query("DELETE FROM oidc_states WHERE created_at < ?").bind(cutoff).execute(&st.pool).await?;
    sqlx::query("INSERT INTO oidc_states (state, verifier, nonce, next, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&state_token)
        .bind(&verifier)
        .bind(&nonce)
        .bind(safe_next(q.next.as_deref()))
        .bind(Utc::now().to_rfc3339())
        .execute(&st.pool)
        .await?;
    let mut url = reqwest::Url::parse(&disc.authorization_endpoint).map_err(|e| anyhow::anyhow!(e))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &cfg.client_id)
        .append_pair("redirect_uri", &redirect_uri(&st))
        .append_pair("scope", &cfg.scopes)
        .append_pair("state", &state_token)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    let cookie = format!("{STATE_COOKIE}={state_token}; Path=/auth; HttpOnly; SameSite=Lax; Max-Age=900{}", secure_attr(&st));
    Ok(([(header::SET_COOKIE, cookie)], Redirect::to(url.as_str())).into_response())
}

fn secure_attr(st: &AppState) -> &'static str {
    if st.cfg.public_url.starts_with("https://") { "; Secure" } else { "" }
}

/// Checks the ID token returned by the token endpoint. It was received directly from the
/// provider over TLS, so per OIDC Core 3.1.3.7 the TLS server check stands in for the
/// signature; issuer, audience, expiry and the login's nonce are verified here.
fn check_id_token(id_token: &str, issuer: &str, client_id: &str, nonce: &str, now: i64) -> Result<Value, String> {
    let payload = id_token.split('.').nth(1).ok_or("malformed id_token")?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_| "malformed id_token payload")?;
    let claims: Value = serde_json::from_slice(&raw).map_err(|_| "malformed id_token claims")?;
    if claims["iss"].as_str().map(|i| i.trim_end_matches('/')) != Some(issuer.trim_end_matches('/')) {
        return Err(format!("id_token issuer {} does not match {issuer}", claims["iss"]));
    }
    let aud_ok = match &claims["aud"] {
        Value::String(a) => a == client_id,
        Value::Array(a) => a.iter().any(|x| x.as_str() == Some(client_id)),
        _ => false,
    };
    if !aud_ok {
        return Err("id_token was not issued for this client".into());
    }
    if !claims["exp"].as_i64().is_some_and(|e| e > now - 60) {
        return Err("id_token expired".into());
    }
    if claims["nonce"].as_str() != Some(nonce) {
        return Err("id_token nonce does not match this login".into());
    }
    Ok(claims)
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn error_page(status: StatusCode, title: &str, msg: &str) -> Response {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    (
        status,
        Html(format!(
            "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'><title>{t}</title>\
             <body style='font-family:system-ui;max-width:520px;margin:80px auto;padding:0 16px'>\
             <h2>{t}</h2><p>{m}</p><p><a href='/auth/login'>Try again</a> · <a href='/auth/logout'>Log out</a></p></body>",
            t = esc(title),
            m = esc(msg)
        )),
    )
        .into_response()
}

async fn callback(State(st): State<AppState>, headers: HeaderMap, Query(q): Query<CallbackQuery>) -> AppResult<Response> {
    let clear_state = format!("{STATE_COOKIE}=; Path=/auth; HttpOnly; SameSite=Lax; Max-Age=0{}", secure_attr(&st));
    let fail = |status, title: &str, msg: &str| {
        let mut r = error_page(status, title, msg);
        r.headers_mut().append(header::SET_COOKIE, clear_state.parse().unwrap());
        r
    };
    if let Some(e) = q.error {
        return Ok(fail(StatusCode::UNAUTHORIZED, "Login failed", &format!("{e}: {}", q.error_description.unwrap_or_default())));
    }
    let (Some(code), Some(state_token)) = (q.code, q.state) else {
        return Ok(fail(StatusCode::BAD_REQUEST, "Login failed", "missing code or state"));
    };
    // the callback must come back to the browser that started the login
    if cookie_value(&headers, STATE_COOKIE).as_deref() != Some(state_token.as_str()) {
        return Ok(fail(StatusCode::BAD_REQUEST, "Login expired", "This login was started in another browser or has expired. Please try again."));
    }
    let cfg = oidc(&st)?;
    let disc = cfg.discovery().await?;
    let row: Option<(String, String, String)> =
        sqlx::query_as("DELETE FROM oidc_states WHERE state = ? AND created_at > ? RETURNING verifier, nonce, next")
            .bind(&state_token)
            .bind((Utc::now() - Duration::minutes(15)).to_rfc3339())
            .fetch_optional(&st.pool)
            .await?;
    let Some((verifier, nonce, next)) = row else {
        return Ok(fail(StatusCode::BAD_REQUEST, "Login expired", "The login request expired or was already used."));
    };

    let http = reqwest::Client::new();
    let token: Value = http
        .post(&disc.token_endpoint)
        .basic_auth(&cfg.client_id, Some(&cfg.client_secret))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri(&st).as_str()),
            ("client_id", cfg.client_id.as_str()),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("token request failed: {e}"))?
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("invalid token response: {e}"))?;
    let Some(access_token) = token["access_token"].as_str() else {
        tracing::warn!("OIDC token error: {}", token["error"]);
        return Ok(fail(StatusCode::UNAUTHORIZED, "Login failed", token["error_description"].as_str().unwrap_or("token exchange failed")));
    };
    let issuer = disc.issuer.clone().unwrap_or_else(|| cfg.issuer.clone());
    let id_claims = match token["id_token"].as_str().ok_or_else(|| "no id_token in the token response".to_string()).and_then(|t| {
        check_id_token(t, &issuer, &cfg.client_id, &nonce, Utc::now().timestamp())
    }) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("OIDC id_token rejected: {e}");
            return Ok(fail(StatusCode::UNAUTHORIZED, "Login failed", &e));
        }
    };
    // Profile and groups come from the userinfo endpoint (queried over TLS with the fresh
    // access token); it must describe the same subject as the checked ID token.
    let info: Value = http
        .get(&disc.userinfo_endpoint)
        .bearer_auth(access_token)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| anyhow::anyhow!("userinfo request failed: {e}"))?
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("invalid userinfo: {e}"))?;

    let sub = info["sub"].as_str().unwrap_or_default().to_string();
    if sub.is_empty() || id_claims["sub"].as_str() != Some(sub.as_str()) {
        return Ok(fail(StatusCode::UNAUTHORIZED, "Login failed", "the userinfo subject does not match the ID token"));
    }
    let groups: Vec<String> = info["groups"].as_array().map(|a| a.iter().filter_map(|g| g.as_str().map(String::from)).collect()).unwrap_or_default();
    let role = if groups.iter().any(|g| g == &cfg.admin_group) {
        "admin"
    } else if groups.iter().any(|g| g == &cfg.staff_group) {
        "staff"
    } else {
        return Ok(fail(
            StatusCode::FORBIDDEN,
            "No access",
            &format!("Your account is not in the '{}' or '{}' group.", cfg.admin_group, cfg.staff_group),
        ));
    };
    let username = info["preferred_username"].as_str().or(info["nickname"].as_str()).or(info["email"].as_str()).unwrap_or(&sub).to_string();
    let name = info["name"].as_str().unwrap_or(&username).to_string();
    let email = info["email"].as_str().map(String::from);
    let ts = now();
    sqlx::query(
        "INSERT INTO users (id, sub, username, name, email, role, created_at, last_login_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(sub) DO UPDATE SET username = excluded.username, name = excluded.name, email = excluded.email,
           role = excluded.role, last_login_at = excluded.last_login_at",
    )
    .bind(new_id())
    .bind(&sub)
    .bind(&username)
    .bind(&name)
    .bind(&email)
    .bind(role)
    .bind(&ts)
    .bind(&ts)
    .execute(&st.pool)
    .await?;
    let (user_id,): (String,) = sqlx::query_as("SELECT id FROM users WHERE sub = ?").bind(&sub).fetch_one(&st.pool).await?;

    let session = random_token(40);
    sqlx::query("DELETE FROM user_sessions WHERE expires_at < ?").bind(Utc::now().to_rfc3339()).execute(&st.pool).await?;
    sqlx::query("INSERT INTO user_sessions (token_hash, user_id, expires_at) VALUES (?, ?, ?)")
        .bind(sha256_hex(&session))
        .bind(&user_id)
        .bind((Utc::now() + Duration::hours(SESSION_HOURS)).to_rfc3339())
        .execute(&st.pool)
        .await?;
    tracing::info!("user {username} logged in as {role}");
    let cookie = format!("{SESSION_COOKIE}={session}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}", SESSION_HOURS * 3600, secure_attr(&st));
    let mut res = ([(header::SET_COOKIE, cookie)], Redirect::to(&next)).into_response();
    res.headers_mut().append(header::SET_COOKIE, clear_state.parse().unwrap());
    Ok(res)
}

async fn logout(State(st): State<AppState>, headers: HeaderMap) -> AppResult<Response> {
    if let Some(token) = cookie_value(&headers, SESSION_COOKIE) {
        sqlx::query("DELETE FROM user_sessions WHERE token_hash = ?").bind(sha256_hex(&token)).execute(&st.pool).await?;
    }
    let clear = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    let target = match st.cfg.oidc.as_ref() {
        Some(cfg) => cfg.discovery().await.ok().and_then(|d| d.end_session_endpoint.clone()).unwrap_or_else(|| "/".into()),
        None => "/".into(),
    };
    Ok(([(header::SET_COOKIE, clear)], Redirect::to(&target)).into_response())
}

/// Serves the SPA for `/admin/*` only to logged-in users; everyone else is sent to the login.
pub async fn guard_admin_page(State(st): State<AppState>, headers: HeaderMap, uri: axum::http::Uri) -> AppResult<Response> {
    if session_user(&st, &headers).await?.is_none() {
        let next = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/admin");
        let mut url = reqwest::Url::parse("http://x/auth/login").unwrap();
        url.query_pairs_mut().append_pair("next", next);
        return Ok(Redirect::to(&format!("/auth/login?{}", url.query().unwrap_or(""))).into_response());
    }
    let index = tokio::fs::read(st.cfg.web_dir.join("index.html")).await?;
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], index).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(claims: Value) -> String {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!("{}.{}.sig", b64.encode(br#"{"alg":"RS256"}"#), b64.encode(claims.to_string()))
    }

    #[test]
    fn id_token_claims_are_checked() {
        let iss = "https://auth.example.com/application/o/pdfsign/";
        let ok = serde_json::json!({ "iss": iss, "aud": "cid", "exp": 2000, "nonce": "n1", "sub": "u" });
        assert!(check_id_token(&token(ok.clone()), iss.trim_end_matches('/'), "cid", "n1", 1000).is_ok());
        let mut aud = ok.clone();
        aud["aud"] = serde_json::json!(["other", "cid"]);
        assert!(check_id_token(&token(aud), iss, "cid", "n1", 1000).is_ok());
        assert!(check_id_token(&token(ok.clone()), iss, "cid", "replayed", 1000).is_err());
        assert!(check_id_token(&token(ok.clone()), iss, "someone-else", "n1", 1000).is_err());
        assert!(check_id_token(&token(ok.clone()), "https://evil.example/", "cid", "n1", 1000).is_err());
        assert!(check_id_token(&token(ok), iss, "cid", "n1", 5000).is_err());
        assert!(check_id_token("garbage", iss, "cid", "n1", 1000).is_err());
    }
}
