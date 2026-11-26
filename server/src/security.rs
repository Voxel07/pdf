//! Browser hardening for every response, and cross-site checks for cookie-authenticated
//! requests (the admin API and logout).

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};

use crate::state::AppState;
use crate::util::AppError;

/// The SPA loads only its own scripts (pdf.js decodes some images with WebAssembly) and
/// uses inline `style` attributes; signatures and previews are `data:`/`blob:` images.
const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; worker-src 'self' blob:; \
    object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'self'";

pub async fn headers(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let https = st.cfg.public_url.starts_with("https://");
    let h = res.headers_mut();
    let html = h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("text/html"));
    if html {
        h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    }
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
    // signing pages carry their link token in the path
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    if https {
        h.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
    }
    res
}

/// `true` when a browser says the request comes from another site or origin. Requests
/// without these headers (bots, scripts, tests) are not browser form posts and pass.
fn cross_site(st: &AppState, headers: &HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        return !matches!(site, "same-origin" | "none");
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let ours = reqwest::Url::parse(&st.cfg.public_url).map(|u| u.origin().ascii_serialization()).unwrap_or_default();
        return origin != ours;
    }
    false
}

/// The admin session cookie is `SameSite=Lax`, which still lets sibling subdomains post
/// forms with it; refuse state-changing admin requests that a browser marks cross-site.
pub async fn same_origin(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let writes = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if path == "/auth/logout" && cross_site(&st, req.headers()) {
        // a cross-site link or image must not end the session
        return Redirect::to("/admin").into_response();
    }
    if writes && (path == "/api/admin" || path.starts_with("/api/admin/")) && cross_site(&st, req.headers()) {
        return AppError::new(StatusCode::FORBIDDEN, "cross_site", "cross-site requests are not allowed").into_response();
    }
    next.run(req).await
}
