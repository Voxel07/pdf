//! Import templates and completed submissions from DocuSeal (cloud or self-hosted) using
//! its REST API. Imported submissions keep their values and signature images, so the
//! signed PDFs can be recreated (and exported) by this application.
//!
//! An import runs as a background job ([`start`]); the admin UI polls [`progress`]. Every
//! warning and error is also written to the activity log.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::activity;
use crate::models::*;
use crate::pdf;
use crate::service::{self, *};
use crate::state::AppState;
use crate::util::*;

const API_MAX_BYTES: usize = 8 * 1024 * 1024;
const SIGNATURE_MAX_BYTES: usize = 8 * 1024 * 1024;
const PDF_MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct Connection {
    /// API base, e.g. `https://api.docuseal.com`, `https://api.docuseal.eu` or
    /// `https://docuseal.example.com/api` for self-hosted installations.
    pub base_url: String,
    pub api_key: String,
}

fn upstream(e: anyhow::Error) -> AppError {
    AppError::new(axum::http::StatusCode::BAD_GATEWAY, "docuseal", format!("{e:#}"))
}

pub struct Client {
    base: reqwest::Url,
    trusted_origins: Vec<reqwest::Url>,
    key: String,
    /// `host[:port][/path]` of the API base; qualifies imported IDs so two DocuSeal
    /// instances with overlapping numeric IDs don't collide.
    instance: String,
}

#[derive(Debug, Serialize)]
pub struct TemplateSummary {
    pub id: i64,
    pub name: String,
    pub fields: usize,
    pub documents: usize,
    pub created_at: Option<String>,
    pub folder: Option<String>,
    pub imported: bool,
}

impl Client {
    pub fn new(c: Connection) -> AppResult<Self> {
        let allowed = std::env::var("DOCUSEAL_TRUSTED_ORIGINS").unwrap_or_default();
        Self::with_trusted_origins(c, &allowed)
    }

    fn with_trusted_origins(c: Connection, allowed: &str) -> AppResult<Self> {
        let base = match c.base_url.trim() {
            "" => "https://api.docuseal.com",
            value => value,
        };
        let base = outbound_url(base).map_err(|_| AppError::bad_request("enter a valid DocuSeal API URL without credentials or a fragment"))?;
        if base.query().is_some() {
            return Err(AppError::bad_request("the DocuSeal API URL must not include a query"));
        }
        if c.api_key.trim().is_empty() {
            return Err(AppError::bad_request("api_key is required"));
        }
        let trusted_origins = allowed.split(',').map(str::trim).filter(|s| !s.is_empty())
            .map(|value| {
                outbound_url(value).ok().filter(|url| url.path() == "/" && url.query().is_none())
                    .ok_or_else(|| AppError::bad_request("DOCUSEAL_TRUSTED_ORIGINS must contain HTTP(S) origins without paths or queries"))
            })
            .collect::<AppResult<Vec<_>>>()?;
        let instance = format!(
            "{}{}{}",
            base.host_str().unwrap_or_default().to_lowercase(),
            base.port().map(|p| format!(":{p}")).unwrap_or_default(),
            base.path().trim_end_matches('/')
        );
        Ok(Self { base, trusted_origins, key: c.api_key.trim().to_string(), instance })
    }

    /// The source key of an upstream object, qualified by the instance.
    fn source_key(&self, kind: &str, id: i64) -> String {
        format!("docuseal:{}:{kind}:{id}", self.instance)
    }

    async fn client_for(&self, url: &reqwest::Url) -> anyhow::Result<reqwest::Client> {
        let host = url.host_str().context("missing DocuSeal destination host")?;
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("pdfsign-importer")
            .timeout(Duration::from_secs(60));
        if !self.trusted_origins.iter().any(|trusted| trusted.origin() == url.origin()) {
            if url.scheme() != "https" {
                bail!("DocuSeal destinations must use HTTPS unless explicitly configured");
            }
            if let Ok(ip) = host.trim_matches(['[', ']']).parse() {
                if !public_address(ip) {
                    bail!("DocuSeal destination is not a public address");
                }
            } else {
                let port = url.port_or_known_default().context("missing DocuSeal destination port")?;
                let addresses: Vec<_> = tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host((host, port)))
                    .await.context("DocuSeal DNS lookup timed out")?
                    .context("could not resolve the DocuSeal destination")?
                    .collect();
                if addresses.is_empty() || addresses.iter().any(|address| !public_address(address.ip())) {
                    bail!("DocuSeal destination does not resolve exclusively to public addresses");
                }
                builder = builder.resolve_to_addrs(host, &addresses);
            }
        }
        Ok(builder.build()?)
    }

    async fn fetch(&self, mut url: reqwest::Url, use_key: bool, api_request: bool) -> anyhow::Result<reqwest::Response> {
        for _ in 0..=5 {
            let same_origin = url.origin() == self.base.origin();
            if api_request && !same_origin {
                bail!("DocuSeal API redirects must stay on the configured origin");
            }
            let client = self.client_for(&url).await?;
            let result = self.send_retrying(|| {
                let request = client.get(url.clone());
                if use_key && same_origin { request.header("X-Auth-Token", &self.key) } else { request }
            }).await?;
            if !matches!(result.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                return Ok(result);
            }
            let location = result.headers().get(reqwest::header::LOCATION).and_then(|value| value.to_str().ok())
                .context("DocuSeal redirect has no valid location")?;
            let next = url.join(location).map_err(|_| anyhow!("invalid DocuSeal redirect destination"))?;
            url = outbound_url(next.as_str())?;
            if result.url().scheme() == "https" && url.scheme() != "https" {
                bail!("DocuSeal redirects cannot downgrade HTTPS");
            }
        }
        bail!("too many DocuSeal redirects");
    }

    /// Sends a request, retrying rate limits (429), server errors and network failures with
    /// a growing delay (honouring `Retry-After`), so a long import survives upstream hiccups.
    async fn send_retrying(&self, make: impl Fn() -> reqwest::RequestBuilder) -> anyhow::Result<reqwest::Response> {
        const ATTEMPTS: u32 = 6;
        let mut delay = Duration::from_secs(1);
        let mut attempt = 1;
        loop {
            let last = attempt == ATTEMPTS;
            match make().send().await {
                Ok(res) if !last && (res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS || res.status().is_server_error()) => {
                    let wait = res
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .map(|s| Duration::from_secs(s.min(120)))
                        .unwrap_or(delay);
                    tracing::warn!("DocuSeal returned {} - retrying in {}s ({attempt}/{ATTEMPTS})", res.status(), wait.as_secs());
                    tokio::time::sleep(wait).await;
                }
                Ok(res) => return Ok(res),
                Err(_) if !last => {
                    tracing::warn!("DocuSeal request failed - retrying in {}s ({attempt}/{ATTEMPTS})", delay.as_secs());
                    tokio::time::sleep(delay).await;
                }
                Err(e) => return Err(anyhow!(e.without_url()).context("DocuSeal request failed")),
            }
            delay = (delay * 2).min(Duration::from_secs(30));
            attempt += 1;
        }
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> anyhow::Result<Value> {
        let mut url = self.base.clone();
        url.set_path(&format!("{}{}", self.base.path().trim_end_matches('/'), path));
        url.query_pairs_mut().extend_pairs(query);
        let res = self.fetch(url, true, true).await?;
        let status = res.status();
        if !status.is_success() {
            bail!("DocuSeal {} returned {}", path, status);
        }
        let body = read_limited(res, API_MAX_BYTES).await?;
        serde_json::from_slice(&body).context("invalid DocuSeal API response")
    }

    /// Fetches all pages of a list endpoint (`{data, pagination: {next}}`). DocuSeal lists
    /// newest first; `after=<pagination.next>` continues with the older entries.
    async fn list_all(&self, path: &str, extra: &[(&str, String)]) -> anyhow::Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut after: Option<i64> = None;
        loop {
            let mut q: Vec<(&str, String)> = extra.to_vec();
            q.push(("limit", "100".into()));
            if let Some(a) = after {
                q.push(("after", a.to_string()));
            }
            let page = self.get(path, &q).await?;
            let data = page.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
            let oldest = data.iter().filter_map(|d| d["id"].as_i64()).min();
            let mut new = 0;
            for d in data {
                if seen.insert(d["id"].as_i64()) {
                    out.push(d);
                    new += 1;
                }
            }
            let next = page.pointer("/pagination/next").and_then(Value::as_i64).or(oldest);
            // stop on an empty page or one with nothing new (guards against a server that
            // ignores `after`), not on a short page
            if new == 0 || next.is_none() || next == after {
                break;
            }
            after = next;
        }
        Ok(out)
    }

    /// Downloads a file URL. Tries without credentials first (file URLs are signed), then
    /// with the API token if the URL belongs to the configured instance.
    async fn download(&self, url: &str, limit: usize) -> anyhow::Result<Vec<u8>> {
        let url = outbound_url(url)?;
        let mut res = self.fetch(url.clone(), false, false).await?;
        if matches!(res.status(), reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::NOT_FOUND)
            && url.origin() == self.base.origin() && res.url().origin() == self.base.origin() {
            res = self.fetch(url, true, false).await?;
        }
        if !res.status().is_success() {
            bail!("DocuSeal file download failed: {}", res.status());
        }
        read_limited(res, limit).await
    }

    pub async fn list_templates(&self) -> AppResult<Vec<TemplateSummary>> {
        let list = self.list_all("/templates", &[]).await.map_err(upstream)?;
        Ok(list
            .iter()
            .map(|t| TemplateSummary {
                id: t["id"].as_i64().unwrap_or_default(),
                name: t["name"].as_str().unwrap_or("Untitled").to_string(),
                fields: t["fields"].as_array().map(|a| a.len()).unwrap_or(0),
                documents: t["documents"].as_array().map(|a| a.len()).unwrap_or(0),
                created_at: t["created_at"].as_str().map(String::from),
                folder: t["folder_name"].as_str().map(String::from),
                imported: false,
            })
            .collect())
    }
}

fn outbound_url(value: &str) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).map_err(|_| anyhow!("invalid DocuSeal destination URL"))?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none()
        || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        bail!("DocuSeal destinations must be HTTP(S) URLs without credentials or fragments");
    }
    Ok(url)
}

fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !matches!(ip.octets(),
            [0 | 10 | 127 | 224..=255, ..] | [100, 64..=127, ..] | [169, 254, ..] |
            [172, 16..=31, ..] | [192, 0, 0 | 2, _] | [192, 88, 99, _] |
            [192, 168, ..] | [198, 18 | 19, ..] | [198, 51, 100, _] | [203, 0, 113, _]),
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            // Allow global unicast, excluding transition, special-purpose, and documentation ranges.
            segments[0] & 0xe000 == 0x2000
                && !matches!(segments, [0x2001, 0..=0x1ff | 0x0db8, ..] | [0x2002 | 0x3fff, ..])
        }
    }
}

#[cfg(test)]
mod outbound_tests {
    use super::*;

    #[test]
    fn outbound_policy_excludes_non_public_addresses() {
        for address in ["127.0.0.1", "10.0.0.1", "100.64.0.1", "169.254.1.1", "::1", "::ffff:192.168.1.1", "fd00::1", "2001:db8::1"] {
            assert!(!public_address(address.parse().unwrap()), "{address}");
        }
        for address in ["8.8.8.8", "2606:4700::1"] {
            assert!(public_address(address.parse().unwrap()), "{address}");
        }
    }

    #[tokio::test]
    async fn a_private_instance_requires_an_exact_configured_origin() {
        let base_url = "http://127.0.0.1:3210/api";
        let config = Connection { base_url: base_url.into(), api_key: "test".into() };
        let url = outbound_url(base_url).unwrap();
        assert!(Client::with_trusted_origins(config.clone(), "").unwrap().client_for(&url).await.is_err());
        let client = Client::with_trusted_origins(config, "http://127.0.0.1:3210").unwrap();
        assert!(client.client_for(&url).await.is_ok());
        let other_port = outbound_url("http://127.0.0.1:3211/api").unwrap();
        assert!(client.client_for(&other_port).await.is_err());
    }
}

#[derive(Debug, Deserialize)]
pub struct ImportRequest {
    #[serde(flatten)]
    pub connection: Connection,
    pub template_ids: Vec<i64>,
    #[serde(default = "yes")]
    pub include_submissions: bool,
    /// Also store the original signed PDFs from DocuSeal in `<export_dir>/docuseal-originals`.
    #[serde(default)]
    pub save_originals: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ImportReport {
    pub templates_created: usize,
    pub templates_reused: usize,
    pub submissions_imported: usize,
    /// All submissions not imported in this run (the sum of the three counters below).
    pub submissions_skipped: usize,
    /// Imported by an earlier run.
    pub submissions_existing: usize,
    /// Not completed by every submitter yet; a later import picks them up.
    pub submissions_incomplete: usize,
    /// Failed with an error (see `warnings` and the activity log); retried by a later import.
    pub submissions_failed: usize,
    pub originals_saved: usize,
    pub persons_created: usize,
    /// Submitters without a known Discord id / e-mail who were matched to a person by name.
    pub persons_matched_by_name: usize,
    pub warnings: Vec<String>,
}

/// State of the current (or last) import, polled by the admin UI for its progress bar.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ImportProgress {
    pub running: bool,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub started_by: Option<String>,
    /// What is happening right now, e.g. "downloading the PDF of 'Form 2026'".
    pub phase: String,
    /// 1-based index of the template being imported, and how many were selected.
    pub template_index: usize,
    pub template_total: usize,
    pub template_name: Option<String>,
    /// Submissions of the current template handled so far, and how many it has.
    pub done: usize,
    pub total: usize,
    pub report: ImportReport,
    /// Set when the import as a whole stopped with an error.
    pub error: Option<String>,
}

/// A running import: updates the shared progress and sends warnings and errors both to the
/// report (shown when the import is done) and to the activity log.
struct Run<'a> {
    state: &'a AppState,
    actor: String,
}

impl Run<'_> {
    fn update(&self, f: impl FnOnce(&mut ImportProgress)) {
        f(&mut self.state.import.lock().unwrap());
    }

    fn phase(&self, phase: impl Into<String>) {
        let phase = phase.into();
        self.update(|p| p.phase = phase);
    }

    fn count(&self, f: impl FnOnce(&mut ImportReport)) {
        self.update(|p| f(&mut p.report));
    }

    async fn warn(&self, msg: String) {
        activity::warn(&self.state.pool, "import", Some(self.actor.as_str()), msg.clone()).await;
        self.update(|p| p.report.warnings.push(msg));
    }

    async fn fail(&self, msg: String) {
        activity::error(&self.state.pool, "import", Some(self.actor.as_str()), msg.clone()).await;
        self.update(|p| p.report.warnings.push(msg));
    }
}

/// Starts an import in the background and returns immediately; [`progress`] reports how far
/// it got. The job does not depend on the HTTP request, so a proxy timeout or a closed browser
/// tab can no longer cut it off halfway.
pub fn start(state: &AppState, req: ImportRequest, actor: &str) -> AppResult<()> {
    let client = Client::new(req.connection.clone())?;
    if req.template_ids.is_empty() {
        return Err(AppError::bad_request("select at least one template"));
    }
    {
        let mut p = state.import.lock().unwrap();
        if p.running {
            return Err(AppError::conflict("import_running", "an import is already running"));
        }
        *p = ImportProgress {
            running: true,
            started_at: Some(now()),
            started_by: Some(actor.to_string()),
            template_total: req.template_ids.len(),
            phase: "starting".into(),
            ..Default::default()
        };
    }
    let state = state.clone();
    let actor = actor.to_string();
    tokio::spawn(async move {
        let job = tokio::spawn({
            let state = state.clone();
            let actor = actor.clone();
            async move { run(&state, &client, &req, &actor).await }
        });
        // a panic inside the job must not leave the import marked as running forever
        let error = job.await.err().map(|e| format!("the import stopped unexpectedly: {e}"));
        if let Some(e) = &error {
            activity::error(&state.pool, "import", Some(actor.as_str()), e.clone()).await;
        }
        let mut p = state.import.lock().unwrap();
        p.running = false;
        p.finished_at = Some(now());
        p.phase = if error.is_some() { "failed".into() } else { "finished".into() };
        p.error = error;
    });
    Ok(())
}

pub fn progress(state: &AppState) -> ImportProgress {
    state.import.lock().unwrap().clone()
}

async fn run(state: &AppState, client: &Client, req: &ImportRequest, actor: &str) {
    let job = Run { state, actor: actor.to_string() };
    activity::info(
        &state.pool,
        "import",
        Some(actor),
        format!("DocuSeal import of {} template(s) from {} started", req.template_ids.len(), client.instance),
    )
    .await;
    for (i, id) in req.template_ids.iter().enumerate() {
        job.update(|p| {
            p.template_index = i + 1;
            p.template_name = None;
            p.done = 0;
            p.total = 0;
        });
        if let Err(e) = import_template(&job, client, *id, req).await {
            job.fail(format!("template {id}: {}", e.message)).await;
        }
    }
    let r = progress(state).report;
    let level = if r.submissions_failed > 0 { activity::Level::Warn } else { activity::Level::Info };
    activity::log(
        &state.pool,
        level,
        "import",
        Some(actor),
        format!(
            "DocuSeal import finished: {} template(s) created, {} reused; {} submission(s) imported, {} already imported, \
             {} not completed yet, {} failed; {} people created, {} matched by name; {} original PDF(s) saved",
            r.templates_created,
            r.templates_reused,
            r.submissions_imported,
            r.submissions_existing,
            r.submissions_incomplete,
            r.submissions_failed,
            r.persons_created,
            r.persons_matched_by_name,
            r.originals_saved
        ),
    )
    .await;
}

enum Outcome {
    Imported,
    Existing,
    Incomplete,
}

/// Maps a DocuSeal field type to ours. `None` = not importable.
fn map_kind(t: &str, has_options: bool) -> Option<FieldKind> {
    Some(match t {
        "text" | "cells" | "phone" | "multiple" | "checkbox" => FieldKind::Text,
        "number" => FieldKind::Number,
        "date" => FieldKind::Date,
        "signature" | "initials" | "image" | "stamp" => FieldKind::Signature,
        "select" | "radio" if has_options => FieldKind::Select,
        "select" | "radio" => FieldKind::Text,
        _ => return None,
    })
}

async fn import_template(job: &Run<'_>, client: &Client, ds_id: i64, req: &ImportRequest) -> AppResult<()> {
    let state = job.state;
    job.phase(format!("loading DocuSeal template {ds_id}"));
    let ds = client.get(&format!("/templates/{ds_id}"), &[]).await.map_err(upstream)?;
    let source = client.source_key("template", ds_id);
    let name = ds["name"].as_str().unwrap_or("Imported template").to_string();
    job.update(|p| p.template_name = Some(name.clone()));

    // submitter roles -> sections
    let roles: Vec<(String, String)> = ds["submitters"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| (s["uuid"].as_str().unwrap_or("").to_string(), s["name"].as_str().unwrap_or("Signer").to_string()))
                .collect()
        })
        .unwrap_or_default();
    let section_of = |submitter_uuid: &str| -> u32 { roles.iter().position(|(u, _)| u == submitter_uuid).unwrap_or(0) as u32 };

    let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM templates WHERE source = ? LIMIT 1")
        .bind(&source)
        .fetch_optional(&state.pool)
        .await?;
    let template = if let Some((id,)) = existing {
        job.count(|r| r.templates_reused += 1);
        get_template(&state.pool, &id).await?
    } else {
        job.phase(format!("downloading the PDF of '{name}'"));
        // documents in schema order
        let docs = ds["documents"].as_array().cloned().unwrap_or_default();
        let order: Vec<String> = ds["schema"]
            .as_array()
            .map(|a| a.iter().filter_map(|s| s["attachment_uuid"].as_str().map(String::from)).collect())
            .unwrap_or_default();
        let mut sorted = docs.clone();
        sorted.sort_by_key(|d| order.iter().position(|u| Some(u.as_str()) == d["uuid"].as_str()).unwrap_or(usize::MAX));
        if sorted.is_empty() {
            return Err(AppError::bad_request("template has no documents"));
        }
        let mut pdfs = Vec::new();
        for d in &sorted {
            let url = d["url"].as_str().ok_or_else(|| anyhow!("document without url"))?;
            pdfs.push(client.download(url, PDF_MAX_BYTES).await.map_err(upstream)?);
        }
        let (merged, offsets) = tokio::task::spawn_blocking(move || pdf::merge(&pdfs)).await.map_err(|e| anyhow!(e))??;
        let page_offset: HashMap<String, u32> = sorted
            .iter()
            .zip(offsets)
            .filter_map(|(d, o)| d["uuid"].as_str().map(|u| (u.to_string(), o)))
            .collect();

        let mut fields: Vec<Field> = Vec::new();
        let mut used_names: HashMap<String, usize> = HashMap::new();
        for f in ds["fields"].as_array().cloned().unwrap_or_default() {
            let ftype = f["type"].as_str().unwrap_or("text");
            let options: Vec<String> = f["options"]
                .as_array()
                .map(|a| a.iter().filter_map(|o| o["value"].as_str().or(o.as_str()).map(String::from)).collect())
                .unwrap_or_default();
            let Some(kind) = map_kind(ftype, !options.is_empty()) else {
                job.warn(format!("'{name}': skipped unsupported field type '{ftype}'")).await;
                continue;
            };
            let base_name = f["name"].as_str().filter(|n| !n.trim().is_empty()).map(String::from).unwrap_or_else(|| {
                let mut c = ftype.chars();
                c.next().map(|x| x.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            });
            let prefs = &f["preferences"];
            for area in f["areas"].as_array().cloned().unwrap_or_default() {
                let count = used_names.entry(base_name.to_lowercase()).or_insert(0);
                *count += 1;
                let fname = if *count == 1 { base_name.clone() } else { format!("{base_name} {count}") };
                let offset = area["attachment_uuid"].as_str().and_then(|u| page_offset.get(u)).copied().unwrap_or(0);
                fields.push(Field {
                    id: random_token(10),
                    name: fname,
                    kind,
                    required: f["required"].as_bool().unwrap_or(false),
                    section: section_of(f["submitter_uuid"].as_str().unwrap_or("")),
                    page: offset + area["page"].as_u64().unwrap_or(0) as u32,
                    x: area["x"].as_f64().unwrap_or(0.0),
                    y: area["y"].as_f64().unwrap_or(0.0),
                    w: area["w"].as_f64().unwrap_or(0.1),
                    h: area["h"].as_f64().unwrap_or(0.03),
                    options: if kind == FieldKind::Select { options.clone() } else { vec![] },
                    font_size: prefs["font_size"].as_f64().map(|s| s as f32),
                    format: prefs["format"].as_str().filter(|_| kind == FieldKind::Date).map(String::from),
                    autofill: None,
                    min: None,
                    max: None,
                    placeholder: None,
                    source_ref: f["uuid"].as_str().map(|u| format!("{u}|{ftype}")),
                });
            }
        }
        let sections: Vec<Section> = if roles.is_empty() {
            vec![Section { id: 0, name: "Main".into(), required: true }]
        } else {
            roles.iter().enumerate().map(|(i, (_, n))| Section { id: i as u32, name: n.clone(), required: true }).collect()
        };
        let t = {
            let _gate = state.gate.read().await;
            service::create_template(state, &name, &merged, fields, sections, Some(source.clone())).await?
        };
        job.count(|r| r.templates_created += 1);
        activity::info(&state.pool, "import", Some(job.actor.as_str()), format!("template '{name}' created from DocuSeal template {ds_id}")).await;
        t
    };

    if !req.include_submissions {
        return Ok(());
    }

    // DocuSeal field (uuid) per (submitter uuid, field name), in template order
    let mut ds_fields: HashMap<(String, String), Vec<String>> = HashMap::new();
    for f in ds["fields"].as_array().cloned().unwrap_or_default() {
        let key = (f["submitter_uuid"].as_str().unwrap_or("").to_string(), f["name"].as_str().unwrap_or("").to_string());
        ds_fields.entry(key).or_default().push(f["uuid"].as_str().unwrap_or("").to_string());
    }

    job.phase(format!("listing the submissions of '{name}'"));
    // archived submissions are listed separately; instances that ignore the filter return
    // the same entries twice, which are dropped here
    let mut list = Vec::new();
    for archived in [false, true] {
        let mut q = vec![("template_id", ds_id.to_string())];
        if archived {
            q.push(("archived", "true".into()));
        }
        list.extend(client.list_all("/submissions", &q).await.map_err(upstream)?);
    }
    let mut seen = HashSet::new();
    list.retain(|s| seen.insert(s["id"].as_i64()));
    let total = list.len();
    let before = progress(state).report;
    job.update(|p| p.total = total);
    job.phase(format!("importing the submissions of '{name}'"));
    for item in list {
        let sid = item["id"].as_i64().unwrap_or_default();
        match import_submission(job, client, &template, &roles, &ds_fields, sid, req).await {
            Ok(Outcome::Imported) => job.count(|r| r.submissions_imported += 1),
            Ok(Outcome::Existing) => job.count(|r| {
                r.submissions_existing += 1;
                r.submissions_skipped += 1;
            }),
            Ok(Outcome::Incomplete) => job.count(|r| {
                r.submissions_incomplete += 1;
                r.submissions_skipped += 1;
            }),
            Err(e) => {
                job.count(|r| {
                    r.submissions_failed += 1;
                    r.submissions_skipped += 1;
                });
                job.fail(format!("'{name}', submission {sid}: {}", e.message)).await;
            }
        }
        job.update(|p| p.done += 1);
    }
    let after = progress(state).report;
    let failed = after.submissions_failed - before.submissions_failed;
    activity::log(
        &state.pool,
        if failed > 0 { activity::Level::Warn } else { activity::Level::Info },
        "import",
        Some(job.actor.as_str()),
        format!(
            "'{name}': {total} submission(s) in DocuSeal; {} imported, {} already imported, {} not completed yet, {failed} failed",
            after.submissions_imported - before.submissions_imported,
            after.submissions_existing - before.submissions_existing,
            after.submissions_incomplete - before.submissions_incomplete,
        ),
    )
    .await;
    Ok(())
}

fn discord_id_of(sub: &Value) -> Option<String> {
    let is_snowflake = |s: &str| (15..=21).contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit());
    let candidates = [
        sub["external_id"].as_str().map(String::from),
        sub.pointer("/metadata/discord_id").and_then(|v| v.as_str().map(String::from).or(v.as_i64().map(|n| n.to_string()))),
        sub.pointer("/metadata/discord").and_then(|v| v.as_str().map(String::from)),
    ];
    candidates.into_iter().flatten().find(|c| is_snowflake(c))
}

async fn import_submission(
    job: &Run<'_>,
    client: &Client,
    t: &Template,
    roles: &[(String, String)],
    ds_fields: &HashMap<(String, String), Vec<String>>,
    sid: i64,
    req: &ImportRequest,
) -> AppResult<Outcome> {
    let state = job.state;
    let source = client.source_key("submission", sid);
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM submissions WHERE source = ?")
        .bind(&source)
        .fetch_optional(&state.pool)
        .await?;
    if exists.is_some() {
        return Ok(Outcome::Existing);
    }
    let ds = client.get(&format!("/submissions/{sid}"), &[]).await.map_err(upstream)?;
    let mut submitters: Vec<Value> = ds["submitters"].as_array().cloned().unwrap_or_default();
    if submitters.is_empty() {
        return Ok(Outcome::Incomplete);
    }
    // every role is a required section here, so only fully completed submissions are imported;
    // partial ones are skipped (not recorded) and picked up by a later import
    let open = submitters.iter().filter(|s| s["completed_at"].as_str().is_none()).count();
    if open > 0 {
        if submitters.len() > open {
            job.warn(format!("'{}', submission {sid}: {open} of {} submitters have not completed yet - skipped for now", t.name, submitters.len()))
                .await;
        }
        return Ok(Outcome::Incomplete);
    }
    submitters.sort_by_key(|s| roles.iter().position(|(u, _)| Some(u.as_str()) == s["uuid"].as_str()).unwrap_or(usize::MAX));
    let primary = &submitters[0];

    // resolve or create the person: Discord id, then e-mail, then a unique name
    let pname = primary["name"].as_str().filter(|n| !n.trim().is_empty()).map(String::from);
    let email = primary["email"].as_str().filter(|e| !e.is_empty()).map(String::from);
    let discord = discord_id_of(primary);
    let mut person = None;
    if let Some(d) = &discord {
        person = person_by_discord(&state.pool, d).await?;
    }
    let discord_known = person.is_some();
    if person.is_none() {
        if let Some(e) = &email {
            person = sqlx::query_as("SELECT * FROM persons WHERE lower(email) = lower(?) LIMIT 1").bind(e).fetch_optional(&state.pool).await?;
        }
    }
    // Repeat signers often use another e-mail address (or none) the next time. Take the one
    // person with this name, unless their e-mail or Discord account says it is someone else.
    let mut by_name = false;
    if person.is_none() {
        if let Some(n) = &pname {
            let mut hits = persons_by_name(&state.pool, n).await?;
            if hits.len() == 1 {
                let p = hits.remove(0);
                let other_email = matches!((&p.email, &email), (Some(a), Some(b)) if !a.eq_ignore_ascii_case(b));
                let other_discord = discord.is_some() && !discord_accounts(&state.pool, &p.id).await?.is_empty();
                if !other_email && !other_discord {
                    person = Some(p);
                    by_name = true;
                }
            }
        }
    }
    // a new person is only written together with the submission (see below)
    let new_person = match &person {
        Some(_) => None,
        None => {
            let name = pname.clone().or_else(|| email.clone()).unwrap_or_else(|| format!("DocuSeal submitter {sid}"));
            Some(validate_person_name(&name).map_err(|e| AppError::bad_request(format!("submitter name '{name}': {}", e.message)))?)
        }
    };
    let person_id = person.as_ref().map(|p| p.id.clone()).unwrap_or_else(new_id);
    let person_name = person.as_ref().map(|p| p.name.clone()).or(new_person.clone()).unwrap_or_default();

    // collect values per our field id
    let mut values: Vec<(String, String, Option<Vec<u8>>, usize)> = Vec::new(); // (field_id, value, image, submitter idx)
    for (si, s) in submitters.iter().enumerate() {
        let su = s["uuid"].as_str().unwrap_or("");
        let mut consumed: HashMap<String, usize> = HashMap::new();
        for v in s["values"].as_array().cloned().unwrap_or_default() {
            let fname = v["field"].as_str().unwrap_or("").to_string();
            let idx = consumed.entry(fname.clone()).or_insert(0);
            let Some(ds_uuid) = ds_fields.get(&(su.to_string(), fname.clone())).and_then(|l| l.get(*idx)).cloned() else {
                continue;
            };
            *idx += 1;
            let ours: Vec<&Field> = t
                .fields
                .iter()
                .filter(|f| f.source_ref.as_deref().is_some_and(|r| r.split('|').next() == Some(ds_uuid.as_str())))
                .collect();
            if ours.is_empty() {
                continue;
            }
            let ds_type = ours[0].source_ref.as_deref().and_then(|r| r.split('|').nth(1)).unwrap_or("text").to_string();
            let raw = &v["value"];
            if raw.is_null() {
                continue;
            }
            let (text, image) = match ours[0].kind {
                FieldKind::Signature => {
                    let Some(url) = raw.as_str().filter(|u| !u.is_empty()) else { continue };
                    let bytes = if url.starts_with("data:") {
                        url.split_once(";base64,").and_then(|(_, d)| {
                            use base64::Engine;
                            base64::engine::general_purpose::STANDARD.decode(d).ok()
                        })
                    } else {
                        client.download(url, SIGNATURE_MAX_BYTES).await.ok()
                    };
                    match bytes.and_then(|b| normalize_image(&b).ok()) {
                        Some(png) => (String::new(), Some(png)),
                        None => {
                            job.warn(format!("'{}', submission {sid}: could not load the image for '{fname}'", t.name)).await;
                            continue;
                        }
                    }
                }
                _ => {
                    let s = match raw {
                        Value::String(s) => s.clone(),
                        Value::Bool(true) => "X".into(),
                        Value::Bool(false) => continue,
                        Value::Number(n) => n.to_string(),
                        Value::Array(a) => a.iter().map(|x| x.as_str().map(String::from).unwrap_or(x.to_string())).collect::<Vec<_>>().join(", "),
                        other => other.to_string(),
                    };
                    let s = if ds_type == "checkbox" && (s == "true" || s == "X") { "X".to_string() } else { s };
                    let s = if ours[0].kind == FieldKind::Date { s.get(..10).unwrap_or(&s).to_string() } else { s };
                    if s.is_empty() {
                        continue;
                    }
                    (s, None)
                }
            };
            for f in ours {
                values.push((f.id.clone(), text.clone(), image.clone(), si));
            }
        }
    }

    let completed_at = submitters
        .iter()
        .filter_map(|s| s["completed_at"].as_str())
        .max()
        .map(|s| parse_ts(s).map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)).unwrap_or_else(|| s.to_string()))
        .unwrap_or_else(now);
    let created_at = ds["created_at"].as_str().and_then(parse_ts).map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)).unwrap_or_else(now);

    // stage the originals before writing anything, so the record and its files describe the
    // same upstream state; files are written after the commit
    let mut originals: Vec<(String, Vec<u8>)> = Vec::new();
    if req.save_originals {
        let mut docs: Vec<(String, String)> = Vec::new();
        for d in ds["documents"].as_array().cloned().unwrap_or_default() {
            if let (Some(n), Some(u)) = (d["name"].as_str(), d["url"].as_str()) {
                docs.push((n.to_string(), u.to_string()));
            }
        }
        if docs.is_empty() {
            for s in &submitters {
                for d in s["documents"].as_array().cloned().unwrap_or_default() {
                    if let (Some(n), Some(u)) = (d["name"].as_str(), d["url"].as_str()) {
                        docs.push((n.to_string(), u.to_string()));
                    }
                }
            }
        }
        if let Some(audit) = ds["audit_log_url"].as_str() {
            docs.push(("audit-log".into(), audit.to_string()));
        }
        for (n, u) in docs {
            match client.download(&u, PDF_MAX_BYTES).await {
                Ok(bytes) => {
                    let file = format!("{}_{}_{}_{}.pdf", &completed_at[..10.min(completed_at.len())], safe_filename(&person_name), sid, safe_filename(&n));
                    originals.push((file, bytes));
                }
                Err(e) => job.warn(format!("'{}', submission {sid}: could not download '{n}': {e:#}", t.name)).await,
            }
        }
    }

    // person, submission, rounds and values commit as one unit: an interrupted import leaves
    // nothing behind, and the next run imports the submission again. A backup waits for it.
    let _gate = state.gate.read().await;
    let mut tx = service::begin_write(&state.pool).await?;
    if let Some(name) = &new_person {
        sqlx::query("INSERT INTO persons (id, name, email, created_at, name_key) VALUES (?, ?, ?, ?, ?)")
            .bind(&person_id)
            .bind(name)
            .bind(&email)
            .bind(now())
            .bind(normalize_name(name))
            .execute(&mut *tx)
            .await?;
    }
    if let (Some(d), false) = (&discord, discord_known) {
        // links the Discord id to the new or matched person (unless it is linked meanwhile)
        sqlx::query("INSERT OR IGNORE INTO discord_accounts (discord_id, person_id, username, linked_at) VALUES (?, ?, NULL, ?)")
            .bind(d)
            .bind(&person_id)
            .bind(now())
            .execute(&mut *tx)
            .await?;
    }
    let sub = service::create_submission(&mut tx, &t.id, &person_id, Some(&source)).await?;
    sqlx::query("UPDATE submissions SET status='completed', created_at=?, completed_at=?, updated_at=? WHERE id=?")
        .bind(&created_at)
        .bind(&completed_at)
        .bind(&completed_at)
        .bind(&sub.id)
        .execute(&mut *tx)
        .await?;
    for (si, s) in submitters.iter().enumerate() {
        let section = roles.iter().position(|(u, _)| Some(u.as_str()) == s["uuid"].as_str()).unwrap_or(0) as u32;
        let ts = s["completed_at"].as_str().and_then(parse_ts).map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)).unwrap_or_else(|| completed_at.clone());
        let signer = s["name"].as_str().filter(|n| !n.is_empty()).or(s["email"].as_str()).unwrap_or(&person_name).to_string();
        let round_id = sqlx::query("INSERT INTO rounds (submission_id, sections, signer_name, ip, user_agent, created_at) VALUES (?, ?, ?, '', ?, ?)")
            .bind(&sub.id)
            .bind(format!("[{section}]"))
            .bind(&signer)
            .bind(format!("imported from DocuSeal submission {sid}"))
            .bind(&ts)
            .execute(&mut *tx)
            .await?
            .last_insert_rowid();
        for (fid, text, image, _) in values.iter().filter(|v| v.3 == si) {
            // a DocuSeal value can map to several of our fields, and one of ours to one value
            sqlx::query("INSERT OR REPLACE INTO field_values (submission_id, field_id, value, image, round_id) VALUES (?, ?, ?, ?, ?)")
                .bind(&sub.id)
                .bind(fid)
                .bind(text)
                .bind(image)
                .bind(round_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    if new_person.is_some() {
        job.count(|r| r.persons_created += 1);
    }
    if by_name {
        job.count(|r| r.persons_matched_by_name += 1);
    }

    if !originals.is_empty() {
        let dir: PathBuf = state.cfg.export_dir.join("docuseal-originals").join(&t.slug);
        // the submission is committed at this point; a file error must not report it as skipped
        let written = async {
            tokio::fs::create_dir_all(&dir).await?;
            let mut n = 0;
            for (file, bytes) in &originals {
                tokio::fs::write(dir.join(file), bytes).await?;
                n += 1;
            }
            Ok::<usize, std::io::Error>(n)
        }
        .await;
        match written {
            Ok(n) => job.count(|r| r.originals_saved += n),
            Err(e) => job.warn(format!("'{}', submission {sid}: imported, but writing originals to {} failed: {e}", t.name, dir.display())).await,
        }
    }
    Ok(Outcome::Imported)
}
