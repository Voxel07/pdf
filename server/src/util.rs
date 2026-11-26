use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rand::RngExt;
use serde_json::json;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// Error type used by all handlers: rendered as `{"error": "...", "code": "..."}`.
#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

pub type AppResult<T> = Result<T, AppError>;

pub async fn read_limited(mut response: reqwest::Response, limit: usize) -> anyhow::Result<Vec<u8>> {
    if response.content_length().is_some_and(|length| length > limit as u64) {
        anyhow::bail!("upstream response exceeds the size limit");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(reqwest::Error::without_url)? {
        if chunk.len() > limit - body.len() {
            anyhow::bail!("upstream response exceeds the size limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl AppError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into() }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "missing or invalid credentials")
    }
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // Internal failures carry database, file and upstream details that clients must not
        // see; staff find them in the activity log (see `activity::record_errors`).
        let message = if self.status == StatusCode::INTERNAL_SERVER_ERROR {
            "Internal server error. Staff can find the details in the activity log."
        } else {
            self.message.as_str()
        };
        let mut res = (self.status, Json(json!({ "error": message, "code": self.code }))).into_response();
        if self.status.is_server_error() {
            // picked up by `activity::record_errors`, which also writes the server log line
            res.extensions_mut().insert(crate::activity::FailedRequest(format!("{}: {}", self.code, self.message)));
        }
        res
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => AppError::not_found("not found"),
            e => AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "database", e.to_string()),
        }
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", format!("{e:#}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "io", e.to_string())
    }
}

/// A file that is deleted when the last owner drops it (export archives).
#[derive(Debug)]
pub struct TempFile(pub std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.0) && e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("could not remove {}: {e}", self.0.display());
        }
    }
}

/// An `AsyncRead` that keeps `K` alive until the reader is dropped (e.g. a [`TempFile`]
/// that must outlive the download streaming it).
struct KeepAlive<R, K> {
    inner: R,
    _keep: K,
}

impl<R: tokio::io::AsyncRead + Unpin, K: Unpin> tokio::io::AsyncRead for KeepAlive<R, K> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

/// Streams a file as a download instead of reading it into memory.
pub async fn download_response(
    path: &std::path::Path, content_type: &str, filename: &str, keep: impl Send + Sync + Unpin + 'static,
    extra: Vec<(axum::http::HeaderName, String)>,
) -> AppResult<Response> {
    use axum::http::header;
    let file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(KeepAlive { inner: file, _keep: keep }));
    let mut res = Response::new(body);
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, content_type.parse().map_err(|_| anyhow::anyhow!("invalid content type"))?);
    h.insert(header::CONTENT_LENGTH, len.into());
    h.insert(header::CACHE_CONTROL, axum::http::HeaderValue::from_static("private, no-store"));
    let disposition = format!("attachment; filename=\"{}\"", filename.replace(|c: char| c == '"' || c == '\\' || c.is_control(), ""));
    h.insert(header::CONTENT_DISPOSITION, disposition.parse().map_err(|_| anyhow::anyhow!("invalid file name"))?);
    for (name, value) in extra {
        h.insert(name, value.parse().map_err(|_| anyhow::anyhow!("invalid header"))?);
    }
    Ok(res)
}

const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";

/// Random URL-safe token (~5.8 bits per char).
pub fn random_token(len: usize) -> String {
    let mut rng = rand::rng();
    (0..len).map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char).collect()
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn sha256_hex(data: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(data.as_ref()))
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn parse_ts(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&chrono::Utc))
}

/// Normalizes a name for the second-factor comparison: Unicode NFKD, diacritics removed,
/// lower case, whitespace collapsed and German transliterations folded, so
/// "  José  Müller " == "jose muller" == "Jose Mueller" and "Strauß" == "Strauss".
pub fn normalize_name(s: &str) -> String {
    let stripped: String = s
        .to_lowercase()
        .replace('ß', "ss")
        .nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect::<String>()
        .replace("ae", "a")
        .replace("oe", "o")
        .replace("ue", "u")
        .replace("ss", "s");
    stripped
        .split(|c: char| c.is_whitespace() || c == ',' || c == '.')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn slugify(s: &str) -> String {
    let norm: String = s
        .nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase();
    let mut out = String::new();
    for c in norm.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "document".into() } else { out.chars().take(60).collect() }
}

/// A file-name-safe version of an arbitrary string (keeps letters incl. umlauts).
pub fn safe_filename(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() { "unnamed".into() } else { out.chars().take(80).collect() }
}

/// An IP network (`10.0.0.0/8`, `::1/128`, or a bare address).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpNet {
    addr: std::net::IpAddr,
    prefix: u8,
}

impl std::str::FromStr for IpNet {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let (a, p) = s.split_once('/').map(|(a, p)| (a, Some(p))).unwrap_or((s, None));
        let addr: std::net::IpAddr = a.parse().map_err(|_| format!("invalid address '{a}'"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            Some(p) => p.parse::<u8>().ok().filter(|p| *p <= max).ok_or_else(|| format!("invalid prefix in '{s}'"))?,
            None => max,
        };
        Ok(IpNet { addr, prefix })
    }
}

impl IpNet {
    pub fn contains(&self, ip: std::net::IpAddr) -> bool {
        use std::net::IpAddr::*;
        let ip = match ip {
            V6(v6) => v6.to_ipv4_mapped().map(V4).unwrap_or(ip),
            v4 => v4,
        };
        let (a, b, bits) = match (self.addr, ip) {
            (V4(a), V4(b)) => (u32::from(a) as u128, u32::from(b) as u128, 32),
            (V6(a), V6(b)) => (u128::from(a), u128::from(b), 128),
            _ => return false,
        };
        if self.prefix == 0 {
            return true;
        }
        let shift = bits - self.prefix as u32;
        (a >> shift) == (b >> shift)
    }
}

/// Parses a comma-separated list of networks; an empty string means none.
pub fn parse_nets(s: &str) -> Result<Vec<IpNet>, String> {
    s.split(',').filter(|p| !p.trim().is_empty()).map(str::parse).collect()
}

/// The client address of a request. `X-Forwarded-For` is only believed when
/// the connecting peer is a trusted proxy; then the rightmost forwarded address that is not
/// itself a trusted proxy is used (entries left of it can be set by the client).
pub fn client_ip(headers: &axum::http::HeaderMap, peer: Option<std::net::IpAddr>, trusted: &[IpNet]) -> String {
    let is_trusted = |ip: std::net::IpAddr| trusted.iter().any(|n| n.contains(ip));
    let Some(peer) = peer else { return String::new() };
    if !is_trusted(peer) {
        return peer.to_canonical().to_string();
    }
    let forwarded = headers.get_all("x-forwarded-for").iter().collect::<Vec<_>>();
    for value in forwarded.into_iter().rev() {
        let Ok(value) = value.to_str() else { break };
        for entry in value.rsplit(',') {
            let Ok(ip) = entry.trim().parse::<std::net::IpAddr>() else { return peer.to_string() };
            if !is_trusted(ip) {
                return ip.to_canonical().to_string();
            }
        }
    }
    peer.to_canonical().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarded_headers_need_a_trusted_peer() {
        let trusted = parse_nets("127.0.0.0/8,10.0.0.0/8,fc00::/7").unwrap();
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "6.6.6.6, 1.2.3.4, 10.0.0.5".parse().unwrap());
        // direct client: headers ignored
        assert_eq!(client_ip(&h, Some("8.8.8.8".parse().unwrap()), &trusted), "8.8.8.8");
        // through a trusted proxy: rightmost untrusted entry, not the client-supplied first one
        assert_eq!(client_ip(&h, Some("127.0.0.1".parse().unwrap()), &trusted), "1.2.3.4");
        assert_eq!(client_ip(&h, Some("::ffff:10.1.2.3".parse().unwrap()), &trusted), "1.2.3.4");
        // nothing trusted
        assert_eq!(client_ip(&h, Some("127.0.0.1".parse().unwrap()), &[]), "127.0.0.1");
        h.insert("x-forwarded-for", "6.6.6.6, invalid".parse().unwrap());
        assert_eq!(client_ip(&h, Some("127.0.0.1".parse().unwrap()), &trusted), "127.0.0.1");
        h.insert("x-forwarded-for", "10.0.0.4, 10.0.0.5".parse().unwrap());
        assert_eq!(client_ip(&h, Some("127.0.0.1".parse().unwrap()), &trusted), "127.0.0.1");
        assert!(parse_nets("10.0.0.0/33").is_err());
        assert!(parse_nets("").unwrap().is_empty());
    }

    #[test]
    fn names() {
        assert_eq!(normalize_name("  José  Müller "), "jose muller");
        assert_eq!(normalize_name("MÜLLER, Jose"), "muller jose");
        assert_eq!(normalize_name("juergen MUELLER"), normalize_name("Jürgen Müller"));
        assert_eq!(normalize_name("Strauß"), normalize_name("strauss"));
        assert_ne!(normalize_name("Anna Schmidt"), normalize_name("Hanna Schmidt"));
    }
    #[test]
    fn slugs() {
        assert_eq!(slugify("Ausrüstung Übergabe 2025!"), "ausrustung-ubergabe-2025");
    }
}
