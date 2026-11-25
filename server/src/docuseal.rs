//! Import templates and completed submissions from DocuSeal (cloud or self-hosted) using
//! its REST API. Imported submissions keep their values and signature images, so the
//! signed PDFs can be recreated (and exported) by this application.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::*;
use crate::pdf;
use crate::service::{self, *};
use crate::state::AppState;
use crate::util::*;

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
    http: reqwest::Client,
    base: String,
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
        let mut base = c.base_url.trim().trim_end_matches('/').to_string();
        if base.is_empty() {
            base = "https://api.docuseal.com".into();
        }
        if !base.starts_with("http://") && !base.starts_with("https://") {
            return Err(AppError::bad_request("base_url must start with http(s)://"));
        }
        if c.api_key.trim().is_empty() {
            return Err(AppError::bad_request("api_key is required"));
        }
        let http = reqwest::Client::builder()
            .user_agent("pdfsign-importer")
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| anyhow!(e))?;
        let url = reqwest::Url::parse(&base).map_err(|e| AppError::bad_request(format!("invalid base_url: {e}")))?;
        let instance = format!(
            "{}{}{}",
            url.host_str().unwrap_or_default().to_lowercase(),
            url.port().map(|p| format!(":{p}")).unwrap_or_default(),
            url.path().trim_end_matches('/')
        );
        Ok(Self { http, base, key: c.api_key.trim().to_string(), instance })
    }

    /// `(qualified, legacy)` source keys for an upstream object. Imports before instance
    /// qualification stored the bare `docuseal:<kind>:<id>`; those are still recognized.
    fn source_keys(&self, kind: &str, id: i64) -> (String, String) {
        (format!("docuseal:{}:{kind}:{id}", self.instance), format!("docuseal:{kind}:{id}"))
    }

    /// The API token is only sent to the configured instance, never to other hosts that
    /// file URLs may point to (e.g. a storage bucket).
    fn same_origin(&self, url: &str) -> bool {
        match (reqwest::Url::parse(&self.base), reqwest::Url::parse(url)) {
            (Ok(a), Ok(b)) => a.scheme() == b.scheme() && a.host_str() == b.host_str() && a.port_or_known_default() == b.port_or_known_default(),
            _ => false,
        }
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> anyhow::Result<Value> {
        let url = format!("{}{}", self.base, path);
        let res = self.http.get(&url).header("X-Auth-Token", &self.key).query(query).send().await.with_context(|| format!("GET {url}"))?;
        let status = res.status();
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            bail!("DocuSeal {} returned {}: {}", path, status, body.chars().take(300).collect::<String>());
        }
        res.json().await.with_context(|| format!("decoding {url}"))
    }

    /// Fetches all pages of a list endpoint (`{data, pagination: {next}}`).
    async fn list_all(&self, path: &str, extra: &[(&str, String)]) -> anyhow::Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut after: Option<i64> = None;
        loop {
            let mut q: Vec<(&str, String)> = extra.to_vec();
            q.push(("limit", "100".into()));
            if let Some(a) = after {
                q.push(("after", a.to_string()));
            }
            let page = self.get(path, &q).await?;
            let data = page.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
            let n = data.len();
            let last_id = data.iter().filter_map(|d| d["id"].as_i64()).max();
            out.extend(data);
            let next = page.pointer("/pagination/next").and_then(Value::as_i64).or(last_id);
            if n < 100 || next.is_none() || next == after {
                break;
            }
            after = next;
        }
        Ok(out)
    }

    /// Downloads a file URL. Tries without credentials first (file URLs are signed), then
    /// with the API token if the URL belongs to the configured instance.
    async fn download(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        let res = self.http.get(url).send().await?;
        let res = if res.status().is_success() || !self.same_origin(url) {
            res
        } else {
            self.http.get(url).header("X-Auth-Token", &self.key).send().await?
        };
        if !res.status().is_success() {
            bail!("download {url} failed: {}", res.status());
        }
        Ok(res.bytes().await?.to_vec())
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

#[derive(Debug, Serialize, Default)]
pub struct ImportReport {
    pub templates_created: usize,
    pub templates_reused: usize,
    pub submissions_imported: usize,
    pub submissions_skipped: usize,
    pub originals_saved: usize,
    pub persons_created: usize,
    pub warnings: Vec<String>,
}

pub async fn import(state: &AppState, req: ImportRequest) -> AppResult<ImportReport> {
    let client = Client::new(req.connection.clone())?;
    let mut report = ImportReport::default();
    for id in &req.template_ids {
        if let Err(e) = import_template(state, &client, *id, &req, &mut report).await {
            report.warnings.push(format!("template {id}: {}", e.message));
        }
    }
    Ok(report)
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

async fn import_template(
    state: &AppState,
    client: &Client,
    ds_id: i64,
    req: &ImportRequest,
    report: &mut ImportReport,
) -> AppResult<()> {
    let ds = client.get(&format!("/templates/{ds_id}"), &[]).await.map_err(upstream)?;
    let (source, legacy_source) = client.source_keys("template", ds_id);
    let name = ds["name"].as_str().unwrap_or("Imported template").to_string();

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

    let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM templates WHERE source IN (?, ?) ORDER BY source = ? DESC LIMIT 1")
        .bind(&source)
        .bind(&legacy_source)
        .bind(&source)
        .fetch_optional(&state.pool)
        .await?;
    let template = if let Some((id,)) = existing {
        report.templates_reused += 1;
        get_template(&state.pool, &id).await?
    } else {
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
            pdfs.push(client.download(url).await.map_err(upstream)?);
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
                report.warnings.push(format!("{name}: skipped unsupported field type '{ftype}'"));
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
        report.templates_created += 1;
        service::create_template(state, &name, &merged, fields, sections, Some(source.clone())).await?
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

    let list = client.list_all("/submissions", &[("template_id", ds_id.to_string())]).await.map_err(upstream)?;
    for item in list {
        let sid = item["id"].as_i64().unwrap_or_default();
        match import_submission(state, client, &template, &roles, &ds_fields, sid, req, report).await {
            Ok(true) => report.submissions_imported += 1,
            Ok(false) => report.submissions_skipped += 1,
            Err(e) => {
                report.submissions_skipped += 1;
                report.warnings.push(format!("submission {sid}: {}", e.message));
            }
        }
    }
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

#[allow(clippy::too_many_arguments)]
async fn import_submission(
    state: &AppState,
    client: &Client,
    t: &Template,
    roles: &[(String, String)],
    ds_fields: &HashMap<(String, String), Vec<String>>,
    sid: i64,
    req: &ImportRequest,
    report: &mut ImportReport,
) -> AppResult<bool> {
    let (source, legacy_source) = client.source_keys("submission", sid);
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM submissions WHERE source IN (?, ?)")
        .bind(&source)
        .bind(&legacy_source)
        .fetch_optional(&state.pool)
        .await?;
    if exists.is_some() {
        return Ok(false);
    }
    let ds = client.get(&format!("/submissions/{sid}"), &[]).await.map_err(upstream)?;
    let mut submitters: Vec<Value> = ds["submitters"].as_array().cloned().unwrap_or_default();
    if submitters.is_empty() {
        return Ok(false);
    }
    // every role is a required section here, so only fully completed submissions are imported;
    // partial ones are skipped (not recorded) and picked up by a later import
    let open = submitters.iter().filter(|s| s["completed_at"].as_str().is_none()).count();
    if open > 0 {
        if submitters.len() > open {
            report.warnings.push(format!("submission {sid}: {open} of {} submitters have not completed yet - skipped for now", submitters.len()));
        }
        return Ok(false);
    }
    submitters.sort_by_key(|s| roles.iter().position(|(u, _)| Some(u.as_str()) == s["uuid"].as_str()).unwrap_or(usize::MAX));
    let primary = &submitters[0];

    // resolve or create the person
    let pname = primary["name"].as_str().filter(|n| !n.trim().is_empty()).map(String::from);
    let email = primary["email"].as_str().filter(|e| !e.is_empty()).map(String::from);
    let discord = discord_id_of(primary);
    let mut person = None;
    if let Some(d) = &discord {
        person = person_by_discord(&state.pool, d).await?;
    }
    if person.is_none() {
        if let Some(e) = &email {
            person = sqlx::query_as("SELECT * FROM persons WHERE lower(email) = lower(?) LIMIT 1").bind(e).fetch_optional(&state.pool).await?;
        }
    }
    // a new person is only written together with the submission (see below)
    let new_person = match &person {
        Some(_) => None,
        None => {
            let name = pname.clone().or_else(|| email.clone()).unwrap_or_else(|| format!("DocuSeal submitter {sid}"));
            Some(validate_person_name(&name)?)
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
                        client.download(url).await.ok()
                    };
                    match bytes.and_then(|b| normalize_image(&b).ok()) {
                        Some(png) => (String::new(), Some(png)),
                        None => {
                            report.warnings.push(format!("submission {sid}: could not load image for '{fname}'"));
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
            match client.download(&u).await {
                Ok(bytes) => {
                    let file = format!("{}_{}_{}_{}.pdf", &completed_at[..10.min(completed_at.len())], safe_filename(&person_name), sid, safe_filename(&n));
                    originals.push((file, bytes));
                }
                Err(e) => report.warnings.push(format!("submission {sid}: could not download '{n}': {e}")),
            }
        }
    }

    // person, submission, rounds and values commit as one unit: an interrupted import leaves
    // nothing behind, and the next run imports the submission again
    let mut tx = service::begin_write(&state.pool).await?;
    if let Some(name) = &new_person {
        sqlx::query("INSERT INTO persons (id, name, email, created_at) VALUES (?, ?, ?, ?)")
            .bind(&person_id)
            .bind(name)
            .bind(&email)
            .bind(now())
            .execute(&mut *tx)
            .await?;
        if let Some(d) = &discord {
            sqlx::query("INSERT INTO discord_accounts (discord_id, person_id, username, linked_at) VALUES (?, ?, NULL, ?)")
                .bind(d)
                .bind(&person_id)
                .bind(now())
                .execute(&mut *tx)
                .await?;
        }
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
        report.persons_created += 1;
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
            Ok(n) => report.originals_saved += n,
            Err(e) => report.warnings.push(format!("submission {sid}: imported, but writing originals to {} failed: {e}", dir.display())),
        }
    }
    Ok(true)
}
