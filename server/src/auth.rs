//! Authentication:
//! * Admin UI users log in via OpenID Connect (authentik). Their role (`admin` / `staff`) is
//!   derived from group membership on every login. Sessions are HttpOnly cookies.
//! * Bots / integrations use API keys generated in the app (`Authorization: Bearer ...`).
//! * Personal signing links (`/s/<token>`, token + name check) and general links
//!   (`/p/<token>`, name + zip identification) are the only unauthenticated pages.

use anyhow::Context;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use base64::Engine;
use chrono::{Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::Jwk};
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
const SESSION_MINUTES: i64 = 15;

#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub scopes: String,
    pub admin_group: String,
    pub staff_group: String,
    pub event_group: Option<String>,
    pub discovery: OnceCell<Discovery>,
    /// The provider's signing keys and when they were fetched.
    pub jwks: std::sync::Arc<std::sync::Mutex<Option<(std::time::Instant, Vec<Value>)>>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
    jwks_uri: String,
    end_session_endpoint: Option<String>,
    id_token_signing_alg_values_supported: Vec<String>,
}

pub fn validate_public_url(value: &str) -> anyhow::Result<()> {
    let url = reqwest::Url::parse(value)?;
    anyhow::ensure!(
        clean_origin_url(&url) && url.path() == "/" && url.query().is_none()
            && (url.scheme() == "https" || (url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))),
        "PUBLIC_URL must be an HTTPS origin (HTTP is allowed for local development)"
    );
    Ok(())
}

fn clean_origin_url(url: &reqwest::Url) -> bool {
    url.host().is_some() && url.username().is_empty() && url.password().is_none() && url.fragment().is_none()
}

pub fn validate_issuer(value: &str) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)?;
    anyhow::ensure!(clean_origin_url(&url) && url.scheme() == "https" && url.query().is_none(), "OIDC_ISSUER must be an HTTPS URL without credentials, a query or a fragment");
    Ok(url)
}

fn provider_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()?)
}

async fn provider_json<T: serde::de::DeserializeOwned>(request: reqwest::RequestBuilder) -> anyhow::Result<T> {
    let response = request.send().await.map_err(reqwest::Error::without_url)?;
    anyhow::ensure!(response.status().is_success(), "identity provider returned {}", response.status());
    Ok(serde_json::from_slice(&read_limited(response, 128 * 1024).await?)?)
}

impl Discovery {
    fn validate(&self, issuer: &str, issuer_url: &reqwest::Url) -> anyhow::Result<()> {
        anyhow::ensure!(self.issuer == issuer, "OIDC discovery issuer does not match OIDC_ISSUER");
        let endpoints = [&self.authorization_endpoint, &self.token_endpoint, &self.userinfo_endpoint, &self.jwks_uri];
        anyhow::ensure!(
            endpoints.into_iter().chain(self.end_session_endpoint.iter())
                .all(|value| reqwest::Url::parse(value).is_ok_and(|url| clean_origin_url(&url) && url.origin() == issuer_url.origin())),
            "OIDC discovery endpoints must use the configured HTTPS origin"
        );
        Ok(())
    }
}

/// Signing keys are reused for this long; an unknown key id triggers an early refresh.
const JWKS_TTL: std::time::Duration = std::time::Duration::from_secs(600);

impl OidcConfig {
    async fn signing_keys(&self, http: &reqwest::Client, uri: &str, refresh: bool) -> anyhow::Result<Vec<Value>> {
        if !refresh && let Some((at, keys)) = &*self.jwks.lock().unwrap() && at.elapsed() < JWKS_TTL {
            return Ok(keys.clone());
        }
        #[derive(Deserialize)]
        struct KeySet {
            keys: Vec<Value>,
        }
        let keys: KeySet = provider_json(http.get(uri)).await?;
        *self.jwks.lock().unwrap() = Some((std::time::Instant::now(), keys.keys.clone()));
        Ok(keys.keys)
    }

    async fn discovery(&self) -> AppResult<&Discovery> {
        self.discovery
            .get_or_try_init(|| async {
                let fetch = async {
                    let issuer_url = validate_issuer(&self.issuer)?;
                    let url = format!("{}/.well-known/openid-configuration", self.issuer.trim_end_matches('/'));
                    let d: Discovery = provider_json(provider_client()?.get(url)).await?;
                    d.validate(&self.issuer, &issuer_url)?;
                    anyhow::Ok(d)
                };
                fetch.await.map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, "oidc", format!("OIDC discovery failed: {e}")))
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

    pub fn is_staff(&self) -> bool {
        self.is_admin() || self.role == "staff"
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
        "SELECT u.* FROM user_sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = ? AND s.expires_at > ? AND s.issuer IS ?",
    )
    .bind(sha256_hex(&token))
    .bind(Utc::now().to_rfc3339())
    .bind(state.cfg.oidc.as_ref().map(|cfg| cfg.issuer.as_str()))
    .fetch_optional(&state.pool)
    .await?;
    Ok(user)
}

pub async fn revoke_user_sessions(state: &AppState, user_id: &str) -> AppResult<u64> {
    let mut tx = crate::service::begin_write(&state.pool).await?;
    sqlx::query("UPDATE users SET sessions_revoked_at = ? WHERE id = ? RETURNING id")
        .bind(Utc::now().to_rfc3339()).bind(user_id).fetch_one(&mut *tx).await?;
    let count = sqlx::query("DELETE FROM user_sessions WHERE user_id = ?")
        .bind(user_id).execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    Ok(count)
}

/// Any logged-in user with the `staff` or `admin` role.
pub struct SessionUser(pub User);

impl FromRequestParts<AppState> for SessionUser {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        match session_user(state, &parts.headers).await? {
            Some(user) if user.is_staff() || (state.cfg.events_enabled && user.role == "attendee") => Ok(Self(user)),
            _ => Err(AppError::unauthorized()),
        }
    }
}

/// Signing staff and admins.
pub struct Staff(pub User);

impl FromRequestParts<AppState> for Staff {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let SessionUser(user) = SessionUser::from_request_parts(parts, state).await?;
        if !user.is_staff() {
            return Err(AppError::new(StatusCode::FORBIDDEN, "forbidden", "staff role required"));
        }
        Ok(Self(user))
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
        let row: Option<(String, String, Option<String>)> = sqlx::query_as("SELECT id, name, last_used_at FROM api_keys WHERE key_hash = ?")
            .bind(sha256_hex(token.as_bytes()))
            .fetch_optional(&state.pool)
            .await?;
        let Some((id, name, last_used)) = row else { return Err(AppError::unauthorized()) };
        // a write per bot request would queue behind SQLite's single writer; minutes are enough
        if last_used.as_deref().and_then(parse_ts).is_none_or(|t| Utc::now() - t > Duration::minutes(5)) {
            let _ = sqlx::query("UPDATE api_keys SET last_used_at = ? WHERE id = ?").bind(now()).bind(id).execute(&state.pool).await;
        }
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
        Some(n) if n.starts_with('/') && !n.starts_with("//") && !n.contains('\\') && !n.chars().any(char::is_control) => n.to_string(),
        _ => "/admin".into(),
    }
}

#[derive(Deserialize)]
struct LoginQuery {
    next: Option<String>,
}

async fn login(
    State(st): State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<LoginQuery>,
) -> AppResult<Response> {
    let cfg = oidc(&st)?;
    // every login start stores a pending state row
    let ip = client_ip(&headers, Some(addr.ip()), &st.cfg.trusted_proxies);
    if !st.rate_ok(&format!("login:{}", crate::state::rate_key(&ip)), 60) {
        return Err(AppError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many login attempts - please wait a few minutes"));
    }
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
    Ok(([(header::SET_COOKIE, cookie), (header::CACHE_CONTROL, "no-store".into())], Redirect::to(url.as_str())).into_response())
}

fn secure_attr(st: &AppState) -> &'static str {
    if reqwest::Url::parse(&st.cfg.public_url).is_ok_and(|url| url.scheme() == "https") { "; Secure" } else { "" }
}

#[derive(Deserialize)]
struct IdClaims {
    sub: String,
    iat: i64,
    nonce: String,
    azp: Option<String>,
}

fn check_id_token(
    token: &str, alg: Algorithm, key: &DecodingKey, issuer: &str, client_id: &str, nonce: &str,
) -> anyhow::Result<IdClaims> {
    let mut validation = Validation::new(alg);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[client_id]);
    validation.set_required_spec_claims(&["iss", "sub", "aud", "exp"]);
    validation.leeway = 0;
    validation.validate_nbf = true;
    let claims = decode::<Value>(token, key, &validation)?.claims;
    let aud = &claims["aud"];
    anyhow::ensure!(
        aud.as_str() == Some(client_id)
            || aud.as_array().is_some_and(|values| values.len() == 1 && values[0].as_str() == Some(client_id)),
        "ID token has an untrusted audience"
    );
    let claims: IdClaims = serde_json::from_value(claims)?;
    anyhow::ensure!(!claims.sub.is_empty(), "ID token subject is empty");
    anyhow::ensure!(claims.iat <= Utc::now().timestamp() + 30, "ID token was issued in the future");
    anyhow::ensure!(claims.nonce == nonce, "ID token nonce does not match this login");
    anyhow::ensure!(claims.azp.as_deref().is_none_or(|azp| azp == client_id), "ID token was issued to another authorized party");
    Ok(claims)
}

async fn verify_id_token(
    http: &reqwest::Client, cfg: &OidcConfig, discovery: &Discovery, token: &str, nonce: &str,
) -> anyhow::Result<IdClaims> {
    let header = decode_header(token)?;
    let algorithm = serde_json::to_value(header.alg)?;
    let name = algorithm.as_str().context("invalid signing algorithm")?;
    anyhow::ensure!(discovery.id_token_signing_alg_values_supported.iter().any(|alg| alg == name), "OIDC provider did not advertise the ID token algorithm");
    let key = match header.alg {
        Algorithm::HS256 => DecodingKey::from_secret(cfg.client_secret.as_bytes())?,
        Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 | Algorithm::PS256 | Algorithm::PS384 | Algorithm::PS512
        | Algorithm::ES256 | Algorithm::ES384 | Algorithm::EdDSA => {
            let kty = match header.alg {
                Algorithm::ES256 | Algorithm::ES384 => "EC",
                Algorithm::EdDSA => "OKP",
                _ => "RSA",
            };
            let pick = |keys: Vec<Value>| -> anyhow::Result<Option<Value>> {
                let mut candidates = keys.into_iter().filter(|key| {
                    key["kty"].as_str() == Some(kty)
                        && header.kid.as_deref().is_none_or(|kid| key["kid"].as_str() == Some(kid))
                        && key.get("alg").is_none_or(|alg| alg.as_str() == Some(name))
                        && key.get("use").is_none_or(|usage| usage.as_str() == Some("sig"))
                        && key.get("key_ops").is_none_or(|ops| ops.as_array().is_some_and(|ops| ops.iter().any(|op| op.as_str() == Some("verify"))))
                });
                let key = candidates.next();
                anyhow::ensure!(candidates.next().is_none(), "OIDC signing key is ambiguous");
                Ok(key)
            };
            // cached keys first; a key the cache does not know means the provider rotated
            let key = match pick(cfg.signing_keys(http, &discovery.jwks_uri, false).await?)? {
                Some(key) => key,
                None => pick(cfg.signing_keys(http, &discovery.jwks_uri, true).await?)?.context("OIDC signing key was not found")?,
            };
            let key: Jwk = serde_json::from_value(key)?;
            DecodingKey::try_from(&key)?
        }
        _ => anyhow::bail!("unsupported OIDC ID token signing algorithm"),
    };
    check_id_token(token, header.alg, &key, &cfg.issuer, &cfg.client_id, nonce)
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
        r.headers_mut().insert(header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-store"));
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
    let row: Option<(String, String, String, String)> =
        sqlx::query_as("DELETE FROM oidc_states WHERE state = ? AND created_at > ? RETURNING verifier, nonce, next, created_at")
            .bind(&state_token)
            .bind((Utc::now() - Duration::minutes(15)).to_rfc3339())
            .fetch_optional(&st.pool)
            .await?;
    let Some((verifier, nonce, next, started_at)) = row else {
        return Ok(fail(StatusCode::BAD_REQUEST, "Login expired", "The login request expired or was already used."));
    };

    let http = provider_client()?;
    let token: Value = provider_json(http
        .post(&disc.token_endpoint)
        .basic_auth(&cfg.client_id, Some(&cfg.client_secret))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri(&st).as_str()),
            ("client_id", cfg.client_id.as_str()),
            ("code_verifier", verifier.as_str()),
        ])).await.map_err(|e| anyhow::anyhow!("OIDC token request failed: {e}"))?;
    let Some(access_token) = token["access_token"].as_str() else {
        tracing::warn!("OIDC token error: {}", token["error"]);
        return Ok(fail(StatusCode::UNAUTHORIZED, "Login failed", token["error_description"].as_str().unwrap_or("token exchange failed")));
    };
    let Some(id_token) = token["id_token"].as_str() else {
        return Ok(fail(StatusCode::UNAUTHORIZED, "Login failed", "The identity provider returned no ID token."));
    };
    let id_claims = match verify_id_token(&http, cfg, disc, id_token, &nonce).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("OIDC id_token rejected: {e}");
            return Ok(fail(StatusCode::UNAUTHORIZED, "Login failed", "The identity provider's ID token could not be verified."));
        }
    };
    // Profile and groups come from the userinfo endpoint (queried over TLS with the fresh
    // access token); it must describe the same subject as the checked ID token.
    let info: Value = provider_json(http.get(&disc.userinfo_endpoint).bearer_auth(access_token))
        .await.map_err(|e| anyhow::anyhow!("OIDC userinfo request failed: {e}"))?;

    let sub = info["sub"].as_str().unwrap_or_default().to_string();
    if id_claims.sub != sub {
        return Ok(fail(StatusCode::UNAUTHORIZED, "Login failed", "the userinfo subject does not match the ID token"));
    }
    let groups: Vec<String> = info["groups"].as_array().map(|a| a.iter().filter_map(|g| g.as_str().map(String::from)).collect()).unwrap_or_default();
    let role = if groups.iter().any(|g| g == &cfg.admin_group) {
        "admin"
    } else if groups.iter().any(|g| g == &cfg.staff_group) {
        "staff"
    } else if st.cfg.events_enabled && cfg.event_group.as_ref().is_some_and(|group| groups.contains(group)) {
        "attendee"
    } else {
        if let Some(id) = sqlx::query_scalar::<_, String>("SELECT id FROM users WHERE sub = ?")
            .bind(&sub).fetch_optional(&st.pool).await? {
            revoke_user_sessions(&st, &id).await?;
        }
        let mut allowed = vec![cfg.admin_group.as_str(), cfg.staff_group.as_str()];
        if st.cfg.events_enabled {
            allowed.extend(cfg.event_group.as_deref());
        }
        return Ok(fail(
            StatusCode::FORBIDDEN,
            "No access",
            &format!("Your account requires membership in one of these groups: {}.", allowed.join(", ")),
        ));
    };
    let username = info["preferred_username"].as_str().or(info["nickname"].as_str()).or(info["email"].as_str()).unwrap_or(&sub).to_string();
    let name = info["name"].as_str().unwrap_or(&username).to_string();
    let email = info["email"].as_str().map(String::from);
    let ts = now();
    let mut tx = crate::service::begin_write(&st.pool).await?;
    let existing: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, role, sessions_revoked_at FROM users WHERE sub = ?"
    ).bind(&sub).fetch_optional(&mut *tx).await?;
    if let Some((id, old_role, revoked_at)) = existing {
        if revoked_at.as_deref().and_then(parse_ts).zip(parse_ts(&started_at)).is_some_and(|(at, started)| at >= started) {
            return Ok(fail(StatusCode::UNAUTHORIZED, "Login expired", "Your sessions were revoked. Start a new login."));
        }
        if old_role != role {
            sqlx::query("DELETE FROM user_sessions WHERE user_id = ?").bind(&id).execute(&mut *tx).await?;
            sqlx::query("UPDATE users SET sessions_revoked_at = ? WHERE id = ?")
                .bind(Utc::now().to_rfc3339()).bind(id).execute(&mut *tx).await?;
        }
    }
    let user_id: String = sqlx::query_scalar(
        "INSERT INTO users (id, sub, username, name, email, role, created_at, last_login_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(sub) DO UPDATE SET username = excluded.username, name = excluded.name, email = excluded.email,
           role = excluded.role, last_login_at = excluded.last_login_at
         RETURNING id",
    )
    .bind(new_id())
    .bind(&sub)
    .bind(&username)
    .bind(&name)
    .bind(&email)
    .bind(role)
    .bind(&ts)
    .bind(&ts)
    .fetch_one(&mut *tx)
    .await?;

    let session = random_token(40);
    sqlx::query("DELETE FROM user_sessions WHERE expires_at < ? OR issuer IS NOT ?")
        .bind(Utc::now().to_rfc3339()).bind(&cfg.issuer).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO user_sessions (token_hash, user_id, expires_at, issuer) VALUES (?, ?, ?, ?)")
        .bind(sha256_hex(&session))
        .bind(&user_id)
        .bind((Utc::now() + Duration::minutes(SESSION_MINUTES)).to_rfc3339())
        .bind(&cfg.issuer)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!("user {username} logged in as {role}");
    let cookie = format!("{SESSION_COOKIE}={session}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}", SESSION_MINUTES * 60, secure_attr(&st));
    let mut res = ([(header::SET_COOKIE, cookie), (header::CACHE_CONTROL, "no-store".into())], Redirect::to(&next)).into_response();
    res.headers_mut().append(header::SET_COOKIE, clear_state.parse().unwrap());
    Ok(res)
}

async fn logout(State(st): State<AppState>, headers: HeaderMap) -> AppResult<Response> {
    if let Some(token) = cookie_value(&headers, SESSION_COOKIE) {
        sqlx::query("DELETE FROM user_sessions WHERE token_hash = ?").bind(sha256_hex(&token)).execute(&st.pool).await?;
    }
    let clear = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}", secure_attr(&st));
    let target = match st.cfg.oidc.as_ref() {
        Some(cfg) => cfg.discovery().await.ok().and_then(|d| d.end_session_endpoint.as_deref()).unwrap_or("/"),
        None => "/",
    };
    Ok(([(header::SET_COOKIE, clear)], Redirect::to(target)).into_response())
}

/// Serves the SPA for `/admin/*` only to logged-in users; everyone else is sent to the login.
pub async fn guard_admin_page(State(st): State<AppState>, headers: HeaderMap, uri: axum::http::Uri) -> AppResult<Response> {
    let Some(user) = session_user(&st, &headers).await? else {
        let next = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/admin");
        let mut url = reqwest::Url::parse("http://x/auth/login").unwrap();
        url.query_pairs_mut().append_pair("next", next);
        return Ok(Redirect::to(&format!("/auth/login?{}", url.query().unwrap_or(""))).into_response());
    };
    if !user.is_staff() {
        let on_events_page = uri.path() == "/admin/events" || uri.path().starts_with("/admin/events/");
        if !st.cfg.events_enabled || user.role != "attendee" || !on_events_page {
            if st.cfg.events_enabled && user.role == "attendee" && uri.path().trim_end_matches('/') == "/admin" {
                return Ok(Redirect::to("/admin/events").into_response());
            }
            return Ok(error_page(StatusCode::FORBIDDEN, "No access", "This page requires signing staff access."));
        }
    }
    let index = tokio::fs::read(st.cfg.web_dir.join("index.html")).await?;
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], index).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};

    fn token(claims: Value) -> String {
        encode(&Header::new(Algorithm::HS256), &claims, &EncodingKey::from_secret(b"test signing key").unwrap()).unwrap()
    }

    #[test]
    fn id_token_claims_are_checked() {
        let iss = "https://auth.example.com/application/o/pdfsign/";
        let now = Utc::now().timestamp();
        let ok = serde_json::json!({ "iss": iss, "aud": "cid", "exp": now + 120, "iat": now, "nonce": "n1", "sub": "u" });
        let key = DecodingKey::from_secret(b"test signing key").unwrap();
        let check = |claims: Value, issuer, nonce| check_id_token(&token(claims), Algorithm::HS256, &key, issuer, "cid", nonce);
        assert!(check(ok.clone(), iss, "n1").is_ok());
        assert!(check(ok.clone(), iss.trim_end_matches('/'), "n1").is_err());
        assert!(check(ok.clone(), iss, "another nonce").is_err());
        let mut aud = ok.clone();
        aud["aud"] = serde_json::json!(["other", "cid"]);
        assert!(check(aud, iss, "n1").is_err());
        let different_key = DecodingKey::from_secret(b"another signing key").unwrap();
        assert!(check_id_token(&token(ok.clone()), Algorithm::HS256, &different_key, iss, "cid", "n1").is_err());
        let mut expired = ok;
        expired["exp"] = serde_json::json!(now - 1);
        assert!(check(expired, iss, "n1").is_err());
    }

    #[test]
    fn discovery_requires_the_exact_https_issuer_and_origin() {
        let iss = "https://auth.example.com/provider/";
        let issuer = validate_issuer(iss).unwrap();
        assert!(validate_issuer("http://auth.example.com/provider/").is_err());
        let mut discovery: Discovery = serde_json::from_value(serde_json::json!({
            "issuer": iss,
            "authorization_endpoint": "https://auth.example.com/authorize",
            "token_endpoint": "https://auth.example.com/token",
            "userinfo_endpoint": "https://auth.example.com/userinfo",
            "jwks_uri": "https://auth.example.com/jwks",
            "id_token_signing_alg_values_supported": ["RS256"]
        })).unwrap();
        assert!(discovery.validate(iss, &issuer).is_ok());
        assert!(discovery.validate(iss.trim_end_matches('/'), &issuer).is_err());
        discovery.token_endpoint = "https://different.example.com/token".into();
        assert!(discovery.validate(iss, &issuer).is_err());
    }
}
