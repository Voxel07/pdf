//! Core business logic shared by the admin API, the bot API, the public signing pages,
//! the DocuSeal importer and the CLI.

use std::collections::{BTreeSet, HashMap};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::Context;
use base64::Engine;
use chrono::{Duration, NaiveDate, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{SqliteConnection, SqliteExecutor, SqlitePool};

use crate::models::*;
use crate::pdf::{self, AuditLine, DrawItem, DrawValue};
use crate::state::AppState;
use crate::util::*;

/// The HTTP write gate is held exclusively. Detached jobs can outlive their start request.
pub async fn reset_application_data(state: &AppState) -> AppResult<()> {
    let _backup = state.backup_running.try_lock().map_err(|_| AppError::conflict("backup_running", "wait for the backup to finish and try again"))?;
    if state.import.lock().unwrap().running || state.export_jobs.lock().unwrap().values().any(|j| j.running) {
        return Err(AppError::conflict("job_running", "wait for the import or export to finish and try again"));
    }

    let mut tx = begin_write(&state.pool).await?;
    // Also collect PDFs left behind by an interrupted upload or an older deletion.
    let mut files = tokio::fs::read_dir(state.cfg.data_dir.join("templates")).await?;
    while let Some(file) = files.next_entry().await? {
        let path = file.path();
        if path.extension().is_some_and(|ext| ext == "pdf")
            && let Some(id) = path.file_stem().and_then(|s| s.to_str())
            && uuid::Uuid::parse_str(id).is_ok()
            && !file.file_type().await?.is_dir()
        {
            sqlx::query("INSERT OR IGNORE INTO deleted_template_pdfs (id) VALUES (?)").bind(id).execute(&mut *tx).await?;
        }
    }
    // Foreign keys remove documents, values, signing rounds, links, accounts and sessions.
    sqlx::raw_sql("DELETE FROM templates; DELETE FROM persons; DELETE FROM person_groups; DELETE FROM users; DELETE FROM api_keys;
                   DELETE FROM oidc_states; DELETE FROM settings; DELETE FROM activity_log; DELETE FROM sqlite_sequence;")
        .execute(&mut *tx).await?;
    tx.commit().await?;
    state.clear_rate_limits();
    *state.import.lock().unwrap() = Default::default();
    state.export_jobs.lock().unwrap().clear();
    state.clean_deleted_pdfs().await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Templates

pub async fn get_template(pool: &SqlitePool, id_or_slug: &str) -> AppResult<Template> {
    let row: Option<TemplateRow> = sqlx::query_as("SELECT * FROM templates WHERE id = ? OR slug = ?")
        .bind(id_or_slug)
        .bind(id_or_slug)
        .fetch_optional(pool)
        .await?;
    row.map(Template::from).ok_or_else(|| AppError::not_found(format!("template '{id_or_slug}' not found")))
}

pub async fn list_templates(pool: &SqlitePool, include_archived: bool) -> AppResult<Vec<Template>> {
    let rows: Vec<TemplateRow> =
        sqlx::query_as("SELECT * FROM templates WHERE archived = 0 OR ? ORDER BY name COLLATE NOCASE")
            .bind(include_archived)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(Template::from).collect())
}

async fn unique_slug(pool: &SqlitePool, base: &str, exclude_id: Option<&str>) -> AppResult<String> {
    let base = slugify(base);
    let mut slug = base.clone();
    let mut i = 2;
    loop {
        let taken: Option<(String,)> = sqlx::query_as("SELECT id FROM templates WHERE slug = ?")
            .bind(&slug)
            .fetch_optional(pool)
            .await?;
        match taken {
            Some((id,)) if Some(id.as_str()) != exclude_id => {
                slug = format!("{base}-{i}");
                i += 1;
            }
            _ => return Ok(slug),
        }
    }
}

pub async fn create_template(
    state: &AppState,
    name: &str,
    pdf_bytes: &[u8],
    fields: Vec<Field>,
    sections: Vec<Section>,
    source: Option<String>,
) -> AppResult<Template> {
    let bytes = pdf_bytes.to_vec();
    let pages = state.blocking(move || pdf::page_count(&bytes)).await?.map_err(|e| AppError::bad_request(format!("{e:#}")))?;
    if pages == 0 {
        return Err(AppError::bad_request("PDF has no pages"));
    }
    let id = new_id();
    let slug = unique_slug(&state.pool, name, None).await?;
    tokio::fs::write(state.template_pdf_path(&id), pdf_bytes).await?;
    let sections = if sections.is_empty() {
        vec![Section { id: 0, name: "Main".into(), required: true }]
    } else {
        sections
    };
    let ts = now();
    sqlx::query(
        "INSERT INTO templates (id, slug, name, description, validity_days, page_count, pdf_sha256, fields, sections, source, created_at, updated_at)
         VALUES (?, ?, ?, '', NULL, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&slug)
    .bind(name)
    .bind(pages as i64)
    .bind(sha256_hex(pdf_bytes))
    .bind(serde_json::to_string(&fields).unwrap())
    .bind(serde_json::to_string(&sections).unwrap())
    .bind(source)
    .bind(&ts)
    .bind(&ts)
    .execute(&state.pool)
    .await?;
    get_template(&state.pool, &id).await
}

#[derive(Debug, serde::Deserialize)]
pub struct TemplateUpdate {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    /// `Some(None)` clears validity (forever valid).
    #[serde(default, with = "double_option")]
    pub validity_days: Option<Option<i64>>,
    pub fields: Option<Vec<Field>>,
    pub sections: Option<Vec<Section>>,
    pub archived: Option<bool>,
}

mod double_option {
    use serde::{Deserialize, Deserializer};
    pub fn deserialize<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Ok(Some(Option::deserialize(d)?))
    }
}

pub async fn update_template(state: &AppState, id: &str, up: TemplateUpdate) -> AppResult<Template> {
    let mut t = get_template(&state.pool, id).await?;
    if let Some(name) = up.name {
        if name.trim().is_empty() {
            return Err(AppError::bad_request("name must not be empty"));
        }
        t.name = name.trim().to_string();
    }
    if let Some(slug) = up.slug {
        t.slug = unique_slug(&state.pool, &slug, Some(&t.id)).await?;
    }
    if let Some(d) = up.description {
        t.description = d;
    }
    if let Some(v) = up.validity_days {
        t.validity_days = v.filter(|d| *d > 0);
    }
    if let Some(sections) = up.sections {
        t.sections = sections;
    }
    if let Some(fields) = up.fields {
        t.fields = fields;
    }
    if let Some(a) = up.archived {
        t.archived = a;
    }
    // the saved template is checked as a whole: changing only the sections must not leave
    // fields pointing at a removed section
    validate_template(&t)?;
    sqlx::query(
        "UPDATE templates SET name=?, slug=?, description=?, validity_days=?, fields=?, sections=?, archived=?, updated_at=? WHERE id=?",
    )
    .bind(&t.name)
    .bind(&t.slug)
    .bind(&t.description)
    .bind(t.validity_days)
    .bind(serde_json::to_string(&t.fields).unwrap())
    .bind(serde_json::to_string(&t.sections).unwrap())
    .bind(t.archived)
    .bind(now())
    .bind(&t.id)
    .execute(&state.pool)
    .await?;
    get_template(&state.pool, &t.id).await
}

pub const AUTOFILL_KINDS: [&str; 3] = ["person_name", "today", "discord_id"];

/// Checks sections, fields, autofill settings and geometry of a template as it will be saved.
fn validate_template(t: &Template) -> AppResult<()> {
    let bad = |m: String| Err(AppError::bad_request(m));
    if t.sections.is_empty() {
        return bad("a template needs at least one section".into());
    }
    let mut section_ids = BTreeSet::new();
    for s in &t.sections {
        if !section_ids.insert(s.id) {
            return bad(format!("duplicate section id {}", s.id));
        }
        if s.name.trim().is_empty() {
            return bad(format!("section {} needs a name", s.id));
        }
    }
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    const EPS: f64 = 1e-3;
    for f in &t.fields {
        if f.id.is_empty() || !ids.insert(f.id.clone()) {
            return bad(format!("duplicate or empty field id '{}'", f.id));
        }
        if f.name.trim().is_empty() || !names.insert(f.name.trim().to_lowercase()) {
            return bad(format!("field names must be unique and non-empty ('{}')", f.name));
        }
        if f.page as i64 >= t.page_count {
            return bad(format!("field '{}' is on a non-existent page", f.name));
        }
        if !section_ids.contains(&f.section) {
            return bad(format!("field '{}' references unknown section {}", f.name, f.section));
        }
        let geom = [f.x, f.y, f.w, f.h];
        if geom.iter().any(|v| !v.is_finite())
            || f.w <= 0.0
            || f.h <= 0.0
            || f.x < -EPS
            || f.y < -EPS
            || f.x + f.w > 1.0 + EPS
            || f.y + f.h > 1.0 + EPS
        {
            return bad(format!("field '{}' lies (partly) outside its page", f.name));
        }
        if f.font_size.is_some_and(|s| !s.is_finite() || s <= 0.0 || f64::from(s) > pdf::MAX_FONT_SIZE) {
            return bad(format!("field '{}' needs a font size between 1 and {}", f.name, pdf::MAX_FONT_SIZE));
        }
        if let (Some(min), Some(max)) = (f.min, f.max) {
            if min > max {
                return bad(format!("field '{}': minimum is larger than maximum", f.name));
            }
        }
        match f.autofill.as_deref() {
            None => {
                if f.kind == FieldKind::Select && f.options.is_empty() {
                    return bad(format!("select field '{}' needs options", f.name));
                }
            }
            Some(a) if !AUTOFILL_KINDS.contains(&a) => return bad(format!("field '{}': unknown autofill '{a}'", f.name)),
            Some(_) if f.kind == FieldKind::Signature => return bad(format!("signature field '{}' cannot be autofilled", f.name)),
            Some(_) => {}
        }
    }
    // a required section without anything to fill in could never be signed, so no document
    // of this template could be completed (a template without any fields is still a draft)
    if !t.fields.is_empty() {
        for s in t.sections.iter().filter(|s| s.required) {
            if !t.fields.iter().any(|f| f.section == s.id && f.autofill.is_none()) {
                return bad(format!("required section '{}' has no field the signer can fill in", s.name));
            }
        }
    }
    Ok(())
}

/// Creates a new template from an existing one: same PDF, fields, sections, description and
/// validity, but no documents, general links or import origin.
pub async fn duplicate_template(state: &AppState, id: &str, name: Option<&str>) -> AppResult<Template> {
    let src = get_template(&state.pool, id).await?;
    let name = name.map(str::trim).filter(|n| !n.is_empty()).map(String::from).unwrap_or_else(|| format!("{} (copy)", src.name));
    let pdf_bytes = template_pdf(state, &src.id).await?;
    let fields: Vec<Field> = src.fields.iter().cloned().map(|f| Field { source_ref: None, ..f }).collect();
    let t = create_template(state, &name, &pdf_bytes, fields, src.sections.clone(), None).await?;
    sqlx::query("UPDATE templates SET description = ?, validity_days = ? WHERE id = ?")
        .bind(&src.description)
        .bind(src.validity_days)
        .bind(&t.id)
        .execute(&state.pool)
        .await?;
    get_template(&state.pool, &t.id).await
}

pub async fn template_pdf(state: &AppState, template_id: &str) -> AppResult<Vec<u8>> {
    tokio::fs::read(state.template_pdf_path(template_id))
        .await
        .map_err(|e| AppError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, "io", format!("template PDF missing: {e}")))
}

// ---------------------------------------------------------------------------------------
// Persons & discord accounts

pub async fn get_person<'e>(ex: impl SqliteExecutor<'e>, id: &str) -> AppResult<Person> {
    Ok(sqlx::query_as("SELECT * FROM persons WHERE id = ?").bind(id).fetch_one(ex).await?)
}

pub async fn person_by_discord(pool: &SqlitePool, discord_id: &str) -> AppResult<Option<Person>> {
    Ok(sqlx::query_as(
        "SELECT p.* FROM persons p JOIN discord_accounts d ON d.person_id = p.id WHERE d.discord_id = ?",
    )
    .bind(discord_id)
    .fetch_optional(pool)
    .await?)
}

pub async fn discord_accounts(pool: &SqlitePool, person_id: &str) -> AppResult<Vec<DiscordAccount>> {
    Ok(sqlx::query_as("SELECT * FROM discord_accounts WHERE person_id = ? ORDER BY linked_at")
        .bind(person_id)
        .fetch_all(pool)
        .await?)
}

pub fn validate_discord_id(id: &str) -> AppResult<()> {
    if id.len() < 5 || id.len() > 25 || !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::bad_request(format!("'{id}' is not a valid Discord id")));
    }
    Ok(())
}

pub fn validate_person_name(name: &str) -> AppResult<String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalize_name(&name).len() < 3 || name.chars().count() > 120 {
        return Err(AppError::bad_request("please provide a full name (3-120 characters)"));
    }
    Ok(name)
}

/// Normalizes a zip / postal code ("12 345" -> "12345"); `None` if empty.
pub fn normalize_zip(zip: Option<&str>) -> AppResult<Option<String>> {
    let Some(z) = zip else { return Ok(None) };
    let z: String = z.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase();
    if z.is_empty() {
        return Ok(None);
    }
    if z.len() < 3 || z.len() > 10 || !z.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(AppError::bad_request("please enter a valid zip code"));
    }
    Ok(Some(z))
}

pub async fn create_person(pool: &SqlitePool, name: &str, email: Option<&str>, zip: Option<&str>) -> AppResult<Person> {
    let name = validate_person_name(name)?;
    let zip = normalize_zip(zip)?;
    let id = new_id();
    sqlx::query("INSERT INTO persons (id, name, email, zip, created_at, name_key) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(&name)
        .bind(email)
        .bind(zip)
        .bind(now())
        .bind(normalize_name(&name))
        .execute(pool)
        .await?;
    get_person(pool, &id).await
}

pub async fn attach_discord(pool: &SqlitePool, person_id: &str, discord_id: &str, username: Option<&str>) -> AppResult<()> {
    validate_discord_id(discord_id)?;
    if let Some(existing) = person_by_discord(pool, discord_id).await? {
        if existing.id != person_id {
            return Err(AppError::conflict("discord_taken", "this Discord account belongs to another person; merge them instead"));
        }
        if let Some(u) = username {
            sqlx::query("UPDATE discord_accounts SET username = ? WHERE discord_id = ?")
                .bind(u)
                .bind(discord_id)
                .execute(pool)
                .await?;
        }
        return Ok(());
    }
    sqlx::query("INSERT INTO discord_accounts (discord_id, person_id, username, linked_at) VALUES (?, ?, ?, ?)")
        .bind(discord_id)
        .bind(person_id)
        .bind(username)
        .bind(now())
        .execute(pool)
        .await?;
    Ok(())
}

/// Creates the person for a Discord account if needed. A registered person keeps their
/// name: it identifies them on signed documents and general links, so only staff change it
/// (repeating the registered name in another spelling is accepted). A ZIP code is only
/// added when none is on file, and the Discord username is refreshed.
pub async fn upsert_discord_person(
    pool: &SqlitePool,
    discord_id: &str,
    name: Option<&str>,
    username: Option<&str>,
    zip: Option<&str>,
) -> AppResult<Person> {
    validate_discord_id(discord_id)?;
    let zip = normalize_zip(zip)?;
    match person_by_discord(pool, discord_id).await? {
        Some(p) => {
            if let Some(n) = name {
                let n = validate_person_name(n)?;
                if normalize_name(&n) != normalize_name(&p.name) {
                    return Err(AppError::conflict(
                        "already_registered",
                        format!("You are already registered as {}. Ask the staff to change your registered name.", p.name),
                    ));
                }
            }
            if let (Some(z), None) = (&zip, &p.zip) {
                sqlx::query("UPDATE persons SET zip = ? WHERE id = ? AND zip IS NULL").bind(z).bind(&p.id).execute(pool).await?;
            }
            if let Some(u) = username {
                sqlx::query("UPDATE discord_accounts SET username = ? WHERE discord_id = ?")
                    .bind(u)
                    .bind(discord_id)
                    .execute(pool)
                    .await?;
            }
            get_person(pool, &p.id).await
        }
        None => {
            let Some(n) = name else {
                return Err(AppError::conflict("name_required", "this Discord account is not registered yet; a name is required"));
            };
            let p = create_person(pool, n, None, zip.as_deref()).await?;
            attach_discord(pool, &p.id, discord_id, username).await?;
            Ok(p)
        }
    }
}

/// Moves everything from `from` into `into` and deletes `from`.
pub async fn merge_persons(pool: &SqlitePool, from: &str, into: &str) -> AppResult<Person> {
    if from == into {
        return get_person(pool, into).await;
    }
    get_person(pool, from).await?;
    get_person(pool, into).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE submissions SET person_id = ? WHERE person_id = ?").bind(into).bind(from).execute(&mut *tx).await?;
    sqlx::query("UPDATE discord_accounts SET person_id = ? WHERE person_id = ?").bind(into).bind(from).execute(&mut *tx).await?;
    sqlx::query("INSERT OR IGNORE INTO group_members (person_id, group_id) SELECT ?, group_id FROM group_members WHERE person_id = ?")
        .bind(into).bind(from).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM persons WHERE id = ?").bind(from).execute(&mut *tx).await?;
    tx.commit().await?;
    get_person(pool, into).await
}

/// Links a new Discord account to the person owning an old (lost) one. If the new account
/// already has its own person (e.g. it registered again), that person is merged into the old.
pub async fn link_discord_accounts(
    pool: &SqlitePool,
    old_discord_id: &str,
    new_discord_id: &str,
    new_username: Option<&str>,
) -> AppResult<Person> {
    validate_discord_id(old_discord_id)?;
    validate_discord_id(new_discord_id)?;
    let old = person_by_discord(pool, old_discord_id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("no person is linked to Discord id {old_discord_id}")))?;
    if let Some(newp) = person_by_discord(pool, new_discord_id).await? {
        if newp.id != old.id {
            merge_persons(pool, &newp.id, &old.id).await?;
        }
    } else {
        attach_discord(pool, &old.id, new_discord_id, new_username).await?;
    }
    if let Some(u) = new_username {
        sqlx::query("UPDATE discord_accounts SET username = ? WHERE discord_id = ?").bind(u).bind(new_discord_id).execute(pool).await?;
    }
    get_person(pool, &old.id).await
}

/// All persons whose name matches (normalized) the given name, through the indexed `name_key`.
pub async fn persons_by_name(pool: &SqlitePool, name: &str) -> AppResult<Vec<Person>> {
    Ok(sqlx::query_as("SELECT * FROM persons WHERE name_key = ? ORDER BY created_at, id")
        .bind(normalize_name(name))
        .fetch_all(pool)
        .await?)
}

pub enum Identified {
    Person(Person),
    /// Every general-link signer confirms their name with their ZIP code.
    NeedZip,
    /// Nobody with this name and ZIP code is registered: a new person (name, ZIP).
    New(String, String),
}

/// Identifies a signer of a general (QR) link by name and ZIP code. The ZIP code is always
/// required and must match the one on file, so knowing a member's name is not enough to sign
/// as them, and the answer to a request without one does not reveal whether the name exists.
/// A registered namesake without a ZIP code on file cannot be told apart, so staff help.
pub async fn identify_by_name(pool: &SqlitePool, name: &str, zip: Option<&str>) -> AppResult<Identified> {
    let name = validate_person_name(name)?;
    if name.split_whitespace().count() < 2 {
        return Err(AppError::bad_request("please enter your first and last name"));
    }
    let Some(zip) = normalize_zip(zip)? else { return Ok(Identified::NeedZip) };
    let candidates = persons_by_name(pool, &name).await?;
    let any_without_zip = candidates.iter().any(|p| p.zip.is_none());
    let mut hits: Vec<Person> = candidates.into_iter().filter(|p| p.zip.as_deref() == Some(zip.as_str())).collect();
    match hits.len() {
        1 => Ok(Identified::Person(hits.remove(0))),
        0 if any_without_zip => Err(AppError::conflict(
            "zip_unknown",
            "we cannot confirm who you are with this name and zip code - please ask the staff to help you",
        )),
        0 => Ok(Identified::New(name, zip)),
        _ => Err(AppError::conflict("ambiguous", "we could not identify you unambiguously - please contact the staff")),
    }
}

// ---------------------------------------------------------------------------------------
// Submissions

pub async fn get_submission<'e>(ex: impl SqliteExecutor<'e>, id: &str) -> AppResult<Submission> {
    Ok(sqlx::query_as("SELECT * FROM submissions WHERE id = ?").bind(id).fetch_one(ex).await?)
}

/// Starts a transaction that takes SQLite's write lock up front, so everything read inside it
/// stays current until commit. Concurrent writers wait (busy timeout) instead of interleaving.
pub async fn begin_write(pool: &SqlitePool) -> AppResult<sqlx::Transaction<'static, sqlx::Sqlite>> {
    Ok(pool.begin_with("BEGIN IMMEDIATE").await?)
}

pub async fn submission_by_token<'e>(ex: impl SqliteExecutor<'e>, token: &str) -> AppResult<Submission> {
    sqlx::query_as("SELECT * FROM submissions WHERE token = ?")
        .bind(token)
        .fetch_optional(ex)
        .await?
        .ok_or_else(|| AppError::not_found("this link is invalid"))
}

#[derive(Debug, Clone, Serialize)]
pub struct Validity {
    pub valid: bool,
    pub valid_until: Option<String>,
    pub expired: bool,
}

pub fn validity(t: &Template, s: &Submission) -> Validity {
    submission_validity(t.validity_days, s)
}

fn submission_validity(validity_days: Option<i64>, s: &Submission) -> Validity {
    if s.status != "completed" {
        return Validity { valid: false, valid_until: None, expired: false };
    }
    let completed = s.completed_at.as_deref().and_then(parse_ts);
    match (validity_days, completed) {
        (Some(days), Some(c)) => {
            let until = c + Duration::days(days);
            let expired = until <= Utc::now();
            Validity { valid: !expired, valid_until: Some(until.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)), expired }
        }
        _ => Validity { valid: true, valid_until: None, expired: false },
    }
}

pub async fn create_submission(
    conn: &mut SqliteConnection,
    template_id: &str,
    person_id: &str,
    source: Option<&str>,
) -> AppResult<Submission> {
    let id = new_id();
    let ts = now();
    sqlx::query(
        "INSERT INTO submissions (id, template_id, person_id, token, status, source, created_at, updated_at) VALUES (?, ?, ?, ?, 'pending', ?, ?, ?)",
    )
    .bind(&id)
    .bind(template_id)
    .bind(person_id)
    .bind(random_token(26))
    .bind(source)
    .bind(&ts)
    .bind(&ts)
    .execute(&mut *conn)
    .await?;
    get_submission(&mut *conn, &id).await
}

/// Returns the submission a person should use for a template: an open (pending) one, or a
/// still-valid completed one (so it can be continued), otherwise a newly created one.
/// The lookup and the insert share one write transaction, so concurrent bot / QR requests
/// for the same person and template converge on one submission. `force_new` always creates.
pub async fn find_or_create_submission(
    pool: &SqlitePool,
    t: &Template,
    person: &Person,
    force_new: bool,
    source: Option<&str>,
) -> AppResult<(Submission, bool)> {
    let mut tx = begin_write(pool).await?;
    if !force_new {
        let subs: Vec<Submission> = sqlx::query_as(
            "SELECT * FROM submissions WHERE person_id = ? AND template_id = ? AND status != 'revoked' ORDER BY created_at DESC",
        )
        .bind(&person.id)
        .bind(&t.id)
        .fetch_all(&mut *tx)
        .await?;
        if let Some(s) = subs.iter().find(|s| s.status == "pending").or_else(|| subs.iter().find(|s| validity(t, s).valid)) {
            return Ok((s.clone(), false));
        }
    }
    if t.archived {
        return Err(AppError::bad_request("template is archived"));
    }
    let sub = create_submission(&mut tx, &t.id, &person.id, source).await?;
    tx.commit().await?;
    Ok((sub, true))
}

pub async fn load_values<'e>(ex: impl SqliteExecutor<'e>, submission_id: &str) -> AppResult<Vec<FieldValue>> {
    Ok(sqlx::query_as("SELECT field_id, value, image, round_id FROM field_values WHERE submission_id = ?")
        .bind(submission_id)
        .fetch_all(ex)
        .await?)
}

pub async fn load_rounds<'e>(ex: impl SqliteExecutor<'e>, submission_id: &str) -> AppResult<Vec<Round>> {
    Ok(sqlx::query_as("SELECT * FROM rounds WHERE submission_id = ? ORDER BY id")
        .bind(submission_id)
        .fetch_all(ex)
        .await?)
}

/// Sections that were already submitted (and are therefore locked).
pub fn done_sections(t: &Template, values: &[FieldValue], rounds: &[Round]) -> BTreeSet<u32> {
    let mut done = BTreeSet::new();
    for r in rounds {
        if let Ok(ids) = serde_json::from_str::<Vec<u32>>(&r.sections) {
            done.extend(ids);
        }
    }
    for v in values {
        if let Some(f) = t.fields.iter().find(|f| f.id == v.field_id) {
            done.insert(f.section);
        }
    }
    done
}

/// Signature images from signers: PNG data URLs only (what the signature pad produces).
fn decode_image_data_url(s: &str) -> AppResult<Vec<u8>> {
    let b64 = s
        .strip_prefix("data:image/png;base64,")
        .ok_or_else(|| AppError::bad_request("signature must be a base64 PNG data URL"))?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|_| AppError::bad_request("invalid base64 in signature"))?;
    normalize_image_as(&raw, Some(image::ImageFormat::Png)).map_err(|e| AppError::bad_request(format!("invalid signature image: {e}")))
}

/// Re-encodes an image (PNG, JPEG, WebP or GIF) as a compact PNG of at most 1000x1000 px.
pub fn normalize_image(raw: &[u8]) -> anyhow::Result<Vec<u8>> {
    normalize_image_as(raw, None)
}

fn normalize_image_as(raw: &[u8], format: Option<image::ImageFormat>) -> anyhow::Result<Vec<u8>> {
    if raw.len() > 8 * 1024 * 1024 {
        anyhow::bail!("image too large");
    }
    let mut img = pdf::decode_image(raw, format)?;
    if img.width() > 1000 || img.height() > 1000 {
        img = img.resize(1000, 1000, image::imageops::FilterType::Triangle);
    }
    let mut out = Vec::new();
    img.to_rgba8().write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)?;
    Ok(out)
}

fn validate_value(f: &Field, raw: &str) -> AppResult<String> {
    let v = raw.trim();
    let err = |m: &str| AppError::bad_request(format!("{}: {m}", f.name));
    match f.kind {
        FieldKind::Text => {
            if v.chars().count() > 2000 {
                return Err(err("text is too long"));
            }
            Ok(v.to_string())
        }
        FieldKind::Number => {
            let n: f64 = v.replace(',', ".").parse().map_err(|_| err("not a number"))?;
            if !n.is_finite() {
                return Err(err("not a number"));
            }
            if f.min.is_some_and(|m| n < m) || f.max.is_some_and(|m| n > m) {
                return Err(err("number out of range"));
            }
            Ok(v.replace(',', "."))
        }
        FieldKind::Date => {
            NaiveDate::parse_from_str(v, "%Y-%m-%d").map_err(|_| err("date must be YYYY-MM-DD"))?;
            Ok(v.to_string())
        }
        FieldKind::Select => {
            if !f.options.iter().any(|o| o == v) {
                return Err(err("value is not one of the allowed options"));
            }
            Ok(v.to_string())
        }
        FieldKind::Signature => Ok(String::new()),
    }
}

pub struct SubmitContext<'a> {
    pub signer_name: &'a str,
    pub ip: &'a str,
    pub user_agent: &'a str,
    /// SHA-256 of the signer session the request came with. It is checked again inside the
    /// write transaction, so a revocation or token rotation that committed first wins.
    pub session_hash: Option<&'a str>,
}

/// Sections of a round that must be complete for the document to count as signed.
fn missing_required<'t>(t: &'t Template, done: &BTreeSet<u32>, touched: &BTreeSet<u32>) -> Vec<&'t Section> {
    t.sections.iter().filter(|s| s.required && !done.contains(&s.id) && !touched.contains(&s.id)).collect()
}

fn check_round(t: &Template, sub: &Submission, done: &BTreeSet<u32>, touched: &BTreeSet<u32>) -> AppResult<()> {
    if sub.status == "revoked" {
        return Err(AppError::new(axum::http::StatusCode::GONE, "revoked", "this document has been revoked"));
    }
    if validity(t, sub).expired {
        return Err(AppError::bad_request("this document has expired; please request a new link"));
    }
    if let Some(sec) = touched.iter().find(|s| done.contains(s)) {
        let name = t.sections.iter().find(|s| s.id == *sec).map(|s| s.name.as_str()).unwrap_or("this section");
        return Err(AppError::conflict("already_signed", format!("'{name}' was already signed and cannot be changed")));
    }
    let missing = missing_required(t, done, touched);
    if sub.status == "pending" && !missing.is_empty() {
        return Err(AppError::bad_request(format!(
            "please also complete: {}",
            missing.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
        )));
    }
    Ok(())
}

/// Validates and stores a round of values from the signer.
///
/// Input is validated against a snapshot first (so images are decoded outside the write
/// lock), then submission status, session, and section completion are read again inside one
/// `BEGIN IMMEDIATE` transaction that also writes the round, its values and the status.
/// Of two competing rounds for the same section exactly one commits.
pub async fn submit_round(
    state: &AppState,
    sub: &Submission,
    t: &Template,
    person: &Person,
    input: &HashMap<String, String>,
    ctx: SubmitContext<'_>,
) -> AppResult<Submission> {
    let pool = &state.pool;
    let values = load_values(pool, &sub.id).await?;
    let rounds = load_rounds(pool, &sub.id).await?;
    let done = done_sections(t, &values, &rounds);

    let mut new_values: Vec<(String, String, Option<Vec<u8>>)> = Vec::new();
    let mut touched = BTreeSet::new();
    for (fid, raw) in input {
        if raw.trim().is_empty() {
            continue;
        }
        let f = t.fields.iter().find(|f| &f.id == fid).ok_or_else(|| AppError::bad_request(format!("unknown field {fid}")))?;
        if f.autofill.is_some() {
            continue;
        }
        if done.contains(&f.section) {
            return Err(AppError::conflict("already_signed", format!("'{}' was already signed and cannot be changed", f.name)));
        }
        let value = validate_value(f, raw)?;
        let image = if f.kind == FieldKind::Signature {
            let raw = raw.clone();
            Some(state.blocking(move || decode_image_data_url(&raw)).await??)
        } else {
            None
        };
        touched.insert(f.section);
        new_values.push((f.id.clone(), value, image));
    }
    if touched.is_empty() {
        return Err(AppError::bad_request("nothing to submit"));
    }
    // all required fields of each touched section must be present
    for sec in &touched {
        for f in t.fields.iter().filter(|f| f.section == *sec && f.required && f.autofill.is_none()) {
            if !new_values.iter().any(|(id, _, _)| id == &f.id) {
                return Err(AppError::bad_request(format!("'{}' is required", f.name)));
            }
        }
    }
    // the document is only complete once all required sections are done
    check_round(t, sub, &done, &touched)?;
    // autofill fields of touched sections
    let discord_id: Option<(String,)> = sqlx::query_as("SELECT discord_id FROM discord_accounts WHERE person_id = ? ORDER BY linked_at DESC LIMIT 1")
        .bind(&person.id)
        .fetch_optional(pool)
        .await?;
    for f in t.fields.iter().filter(|f| touched.contains(&f.section)) {
        if let Some(kind) = &f.autofill {
            let v = match kind.as_str() {
                "person_name" => person.name.clone(),
                "today" => Utc::now().format("%Y-%m-%d").to_string(),
                "discord_id" => discord_id.as_ref().map(|d| d.0.clone()).unwrap_or_default(),
                _ => continue,
            };
            new_values.push((f.id.clone(), v, None));
        }
    }

    let ts = now();
    let mut tx = begin_write(pool).await?;
    // re-read everything the decision depends on; another round, a revocation or a token
    // rotation may have committed since the snapshot above
    let cur: Submission = sqlx::query_as("SELECT * FROM submissions WHERE id = ?")
        .bind(&sub.id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::not_found("this document no longer exists"))?;
    if cur.token != sub.token {
        return Err(AppError::new(axum::http::StatusCode::UNAUTHORIZED, "session_expired", "this link was replaced - please use the new link"));
    }
    if let Some(hash) = ctx.session_hash {
        let exp: Option<(String,)> = sqlx::query_as("SELECT expires_at FROM signer_sessions WHERE token_hash = ? AND submission_id = ?")
            .bind(hash)
            .bind(&cur.id)
            .fetch_optional(&mut *tx)
            .await?;
        if !exp.and_then(|(e,)| parse_ts(&e)).is_some_and(|e| e > Utc::now()) {
            return Err(AppError::new(axum::http::StatusCode::UNAUTHORIZED, "session_expired", "session expired - please enter your name again"));
        }
    }
    let values = load_values(&mut *tx, &cur.id).await?;
    let rounds = load_rounds(&mut *tx, &cur.id).await?;
    check_round(t, &cur, &done_sections(t, &values, &rounds), &touched)?;

    let round_id = sqlx::query(
        "INSERT INTO rounds (submission_id, sections, signer_name, ip, user_agent, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&cur.id)
    .bind(serde_json::to_string(&touched.iter().collect::<Vec<_>>()).unwrap())
    .bind(ctx.signer_name)
    .bind(ctx.ip)
    .bind(ctx.user_agent.chars().take(300).collect::<String>())
    .bind(&ts)
    .execute(&mut *tx)
    .await?
    .last_insert_rowid();
    for (fid, value, image) in new_values {
        // plain INSERT: a signed value is never replaced
        sqlx::query("INSERT INTO field_values (submission_id, field_id, value, image, round_id) VALUES (?, ?, ?, ?, ?)")
            .bind(&cur.id)
            .bind(fid)
            .bind(value)
            .bind(image)
            .bind(round_id)
            .execute(&mut *tx)
            .await?;
    }
    if cur.status == "pending" {
        sqlx::query("UPDATE submissions SET status='completed', completed_at=?, updated_at=? WHERE id=? AND status='pending'")
            .bind(&ts)
            .bind(&ts)
            .bind(&cur.id)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query("UPDATE submissions SET updated_at=? WHERE id=?").bind(&ts).bind(&cur.id).execute(&mut *tx).await?;
    }
    let out = get_submission(&mut *tx, &cur.id).await?;
    tx.commit().await?;
    Ok(out)
}

/// Revokes a submission and ends its signer sessions in one write transaction, so no round
/// can commit after the revocation.
pub async fn revoke_submission(pool: &SqlitePool, id: &str) -> AppResult<()> {
    let mut tx = begin_write(pool).await?;
    let n = sqlx::query("UPDATE submissions SET status = 'revoked', updated_at = ? WHERE id = ?").bind(now()).bind(id).execute(&mut *tx).await?;
    if n.rows_affected() == 0 {
        return Err(AppError::not_found("submission not found"));
    }
    sqlx::query("DELETE FROM signer_sessions WHERE submission_id = ?").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Replaces the link token and ends all signer sessions of a submission atomically.
pub async fn rotate_submission_token(pool: &SqlitePool, id: &str) -> AppResult<Submission> {
    let mut tx = begin_write(pool).await?;
    let n = sqlx::query("UPDATE submissions SET token = ?, failed_attempts = 0, updated_at = ? WHERE id = ?")
        .bind(random_token(26))
        .bind(now())
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if n.rows_affected() == 0 {
        return Err(AppError::not_found("submission not found"));
    }
    sqlx::query("DELETE FROM signer_sessions WHERE submission_id = ?").bind(id).execute(&mut *tx).await?;
    let s = get_submission(&mut *tx, id).await?;
    tx.commit().await?;
    Ok(s)
}

// ---------------------------------------------------------------------------------------
// Rendering & export

pub fn format_date(iso: &str, fmt: Option<&str>) -> String {
    let Ok(d) = NaiveDate::parse_from_str(iso, "%Y-%m-%d") else { return iso.to_string() };
    let fmt = fmt.unwrap_or("DD.MM.YYYY");
    fmt.replace("YYYY", &d.format("%Y").to_string())
        .replace("MM", &d.format("%m").to_string())
        .replace("DD", &d.format("%d").to_string())
}

pub fn display_value(f: &Field, v: &FieldValue) -> Option<DrawValue> {
    match f.kind {
        FieldKind::Signature => v.image.clone().map(DrawValue::Image),
        FieldKind::Date => Some(DrawValue::Text(format_date(&v.value, f.format.as_deref()))),
        _ if v.value.is_empty() => None,
        _ => Some(DrawValue::Text(v.value.clone())),
    }
}

pub async fn render_submission(state: &AppState, sub: &Submission) -> AppResult<(String, Vec<u8>)> {
    let t = get_template(&state.pool, &sub.template_id).await?;
    let r = render_submission_for(state, sub, &t).await?;
    Ok((r.filename, r.bytes))
}

pub struct Rendered {
    pub filename: String,
    pub bytes: Vec<u8>,
    pub person: Person,
    pub accounts: Vec<DiscordAccount>,
}

/// Renders a submission of the (already loaded) template `t`; also returns the person and
/// Discord accounts it loaded, so exports need no second lookup.
pub async fn render_submission_for(state: &AppState, sub: &Submission, t: &Template) -> AppResult<Rendered> {
    let person = get_person(&state.pool, &sub.person_id).await?;
    let values = load_values(&state.pool, &sub.id).await?;
    let rounds = load_rounds(&state.pool, &sub.id).await?;
    let accounts = discord_accounts(&state.pool, &person.id).await?;
    let template_doc = state.template_document(&t.id, &t.pdf_sha256).await?;

    let mut items = Vec::new();
    for v in &values {
        if let Some(f) = t.fields.iter().find(|f| f.id == v.field_id) {
            if let Some(dv) = display_value(f, v) {
                items.push(DrawItem { field: f.clone(), value: dv });
            }
        }
    }
    let audit = if state.cfg.audit_page {
        let mut lines = vec![
            AuditLine::Title("Signature certificate".into()),
            AuditLine::Kv("Document".into(), t.name.clone()),
            AuditLine::Kv("Submission".into(), sub.id.clone()),
            AuditLine::Kv("Signer".into(), person.name.clone()),
        ];
        if !accounts.is_empty() {
            lines.push(AuditLine::Kv(
                "Discord account(s)".into(),
                accounts
                    .iter()
                    .map(|a| match &a.username {
                        Some(u) => format!("{} ({u})", a.discord_id),
                        None => a.discord_id.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        lines.push(AuditLine::Kv("Status".into(), sub.status.clone()));
        if let Some(c) = &sub.completed_at {
            lines.push(AuditLine::Kv("Completed".into(), c.clone()));
        }
        let v = validity(&t, sub);
        if let Some(u) = &v.valid_until {
            lines.push(AuditLine::Kv("Valid until".into(), u.clone()));
        }
        lines.push(AuditLine::Kv("Original SHA-256".into(), t.pdf_sha256.clone()));
        if let Some(src) = &sub.source {
            lines.push(AuditLine::Kv("Imported from".into(), src.clone()));
        }
        for (i, r) in rounds.iter().enumerate() {
            lines.push(AuditLine::Heading(format!("Round {} - {}", i + 1, r.created_at)));
            let secs: Vec<u32> = serde_json::from_str(&r.sections).unwrap_or_default();
            let names: Vec<String> = secs
                .iter()
                .map(|id| t.sections.iter().find(|s| s.id == *id).map(|s| s.name.clone()).unwrap_or(format!("#{id}")))
                .collect();
            lines.push(AuditLine::Kv("Sections".into(), names.join(", ")));
            lines.push(AuditLine::Kv("Name verified as".into(), r.signer_name.clone()));
            if !r.ip.is_empty() {
                lines.push(AuditLine::Kv("IP address".into(), r.ip.clone()));
            }
            if !r.user_agent.is_empty() {
                lines.push(AuditLine::Kv("User agent".into(), r.user_agent.clone()));
            }
            let filled: Vec<String> = values
                .iter()
                .filter(|v| v.round_id == r.id)
                .filter_map(|v| t.fields.iter().find(|f| f.id == v.field_id))
                .map(|f| f.name.clone())
                .collect();
            lines.push(AuditLine::Kv("Fields".into(), filled.join(", ")));
        }
        lines.push(AuditLine::Spacer);
        lines.push(AuditLine::Text(format!("Generated {} by pdfsign.", now())));
        Some(lines)
    } else {
        None
    };
    let bytes = state.blocking(move || pdf::render(&template_doc, &items, audit.as_deref())).await??;
    let date = sub.completed_at.as_deref().unwrap_or(&sub.created_at).get(..10).unwrap_or("").to_string();
    let filename = format!("{}_{}_{}.pdf", date, safe_filename(&person.name), &sub.id[..8]);
    Ok(Rendered { filename, bytes, person, accounts })
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ExportReport {
    pub dir: String,
    pub written: usize,
    pub failed: Vec<String>,
}

/// Writes every completed submission (of one template, if given) as PDF to
/// `<dir>/<template-slug>/<file>.pdf` plus an `index.csv`. Files have deterministic names, so
/// re-exporting updates continued forms.
pub async fn export_all(state: &AppState, dir: &Path, template: Option<&str>) -> AppResult<ExportReport> {
    export_all_with(state, dir, template, &|_, _| {}).await
}

/// Called with `(documents done, documents total)` while an export runs.
pub type ExportProgress<'a> = &'a (dyn Fn(usize, usize) + Send + Sync);

/// [`export_all`] reporting its progress after every document.
pub async fn export_all_with(state: &AppState, dir: &Path, template: Option<&str>, progress: ExportProgress<'_>) -> AppResult<ExportReport> {
    let only = match template.filter(|t| !t.trim().is_empty()) {
        Some(t) => Some(get_template(&state.pool, t.trim()).await?.id),
        None => None,
    };
    let subs: Vec<Submission> =
        sqlx::query_as("SELECT * FROM submissions WHERE status = 'completed' AND (?1 IS NULL OR template_id = ?1) ORDER BY completed_at")
            .bind(&only)
            .fetch_all(&state.pool)
            .await?;
    tokio::fs::create_dir_all(dir).await?;
    let total = subs.len();
    progress(0, total);
    let mut report = ExportReport { dir: dir.display().to_string(), ..Default::default() };
    let mut csv = String::from("file,template,person,discord_ids,status,completed_at,valid_until,submission_id\n");
    let mut templates: HashMap<String, Template> = HashMap::new();
    for (i, s) in subs.into_iter().enumerate() {
        if i > 0 {
            progress(i, total);
        }
        if !templates.contains_key(&s.template_id) {
            templates.insert(s.template_id.clone(), get_template(&state.pool, &s.template_id).await?);
        }
        let t = &templates[&s.template_id];
        match render_submission_for(state, &s, t).await {
            Ok(Rendered { filename: name, bytes, person, accounts }) => {
                let sub_dir: PathBuf = dir.join(&t.slug);
                tokio::fs::create_dir_all(&sub_dir).await?;
                tokio::fs::write(sub_dir.join(&name), bytes).await?;
                report.written += 1;
                let ids: Vec<String> = accounts.into_iter().map(|a| a.discord_id).collect();
                let v = validity(t, &s);
                csv.push_str(&format!(
                    "{},{},{},{},{},{},{},{}\n",
                    csv_escape(&format!("{}/{}", t.slug, name)),
                    csv_escape(&t.name),
                    csv_escape(&person.name),
                    ids.join(" "),
                    if v.expired { "expired" } else { "completed" },
                    s.completed_at.clone().unwrap_or_default(),
                    v.valid_until.unwrap_or_default(),
                    s.id
                ));
            }
            Err(e) => report.failed.push(format!("{}: {}", s.id, e.message)),
        }
    }
    tokio::fs::write(dir.join("index.csv"), csv).await?;
    progress(total, total);
    Ok(report)
}

/// Quotes a CSV cell. Cells that a spreadsheet would run as a formula (`=`, `+`, `-`, `@`,
/// tab or carriage return first) get a leading apostrophe, since names are self-registered.
fn csv_escape(s: &str) -> String {
    let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{s}") } else { s.to_string() };
    if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s }
}

/// Same as [`export_all`] but as a zip archive in a temporary file (deleted when the
/// returned handle is dropped), reporting its progress; after the last document the
/// archive is packed. Documents that could not be rendered are listed in `FAILED.txt`
/// inside the archive and in the returned report.
pub async fn export_zip_with(state: &AppState, template: Option<&str>, progress: ExportProgress<'_>) -> AppResult<(TempFile, ExportReport)> {
    let tmp = std::env::temp_dir().join(format!("pdfsign-export-{}", random_token(10)));
    let report = match export_all_with(state, &tmp, template, progress).await {
        Ok(r) => r,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&tmp).await;
            return Err(e);
        }
    };
    if !report.failed.is_empty() {
        let text = format!("{} document(s) could not be rendered and are missing from this archive:\n{}\n", report.failed.len(), report.failed.join("\n"));
        tokio::fs::write(tmp.join("FAILED.txt"), text).await?;
    }
    let archive = TempFile(tmp.with_extension("zip"));
    let (src, dst) = (tmp.clone(), archive.0.clone());
    let packed = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut zw = zip::ZipWriter::new(std::io::BufWriter::new(std::fs::File::create(&dst)?));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true);
        for entry in walk(&src)? {
            let rel = entry.strip_prefix(&src)?.to_string_lossy().replace('\\', "/");
            zw.start_file(rel, opts)?;
            std::io::copy(&mut std::fs::File::open(&entry)?, &mut zw)?;
        }
        std::io::Write::flush(&mut zw.finish()?)?;
        Ok(())
    })
    .await
    .context("zip task");
    let _ = tokio::fs::remove_dir_all(&tmp).await;
    packed??;
    Ok((archive, report))
}

fn walk(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() { out.extend(walk(&p)?) } else { out.push(p) }
    }
    out.sort();
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// JSON views

pub async fn submission_json(state: &AppState, s: &Submission, t: &Template) -> AppResult<Value> {
    let person = get_person(&state.pool, &s.person_id).await?;
    Ok(submission_summary(state, s, &person.name, &t.name, &t.slug, t.validity_days))
}

pub fn submission_summary(state: &AppState, s: &Submission, person_name: &str, template_name: &str, slug: &str, days: Option<i64>) -> Value {
    let v = submission_validity(days, s);
    json!({
        "id": s.id,
        "template_id": s.template_id,
        "template_slug": slug,
        "template_name": template_name,
        "person_id": s.person_id,
        "person_name": person_name,
        "status": if v.expired { "expired" } else { s.status.as_str() },
        "valid": v.valid,
        "valid_until": v.valid_until,
        "created_at": s.created_at,
        "updated_at": s.updated_at,
        "completed_at": s.completed_at,
        "locked": s.failed_attempts >= state.cfg.max_name_attempts,
        "source": s.source,
        "url": state.sign_url(&s.token),
    })
}

// ---------------------------------------------------------------------------------------
// Output folders

/// `path` with its longest existing prefix resolved (symlinks, `..`); the missing rest is
/// appended as given.
fn resolve_partly(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    while !existing.exists() {
        match existing.file_name() {
            Some(name) => {
                rest.push(name.to_owned());
                existing.pop();
            }
            None => break,
        }
    }
    let mut out = existing.canonicalize().unwrap_or(existing);
    for name in rest.iter().rev() {
        out.push(name);
    }
    out
}

/// Export and backup folders chosen in the admin UI must be absolute and must not be the
/// published web folder (anything written there is downloadable without login) or hold the
/// live data (the database folder itself or its template PDFs).
pub fn check_output_dir(state: &AppState, dir: &Path) -> AppResult<()> {
    if !dir.is_absolute() || dir.components().any(|c| c == std::path::Component::ParentDir) {
        return Err(AppError::bad_request("the folder must be an absolute path without '..'"));
    }
    let target = resolve_partly(dir);
    let web = resolve_partly(&state.cfg.web_dir);
    let data = resolve_partly(&state.cfg.data_dir);
    if target.starts_with(&web) {
        return Err(AppError::bad_request("the folder must not be inside the published web folder"));
    }
    if target == data || target.starts_with(data.join("templates")) {
        return Err(AppError::bad_request("the folder must not be the data folder or its templates folder"));
    }
    Ok(())
}
