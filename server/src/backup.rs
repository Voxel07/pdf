//! Scheduled backups.
//!
//! A backup briefly pauses the app: every state-changing request holds a read guard on
//! `AppState::gate`; the backup takes the write guard, which waits for in-flight requests to
//! finish and holds new ones until the snapshot is taken (they are delayed, not rejected).
//! While paused, the WAL is checkpointed and `VACUUM INTO` writes a consistent copy of the
//! database; the template PDFs it references are copied after the pause (they never change
//! once written). Event deployments also snapshot the separate
//! calendar database. Staging directories and published archives are private to the owner.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context};
use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{ConnectOptions, Connection, SqliteConnection, sqlite::SqliteConnectOptions};

use crate::state::AppState;
use crate::util::*;

pub const CALENDAR_MAX_BYTES: usize = 128 * 1024 * 1024;
/// Archive layout written to `manifest.json`; restores accept only this format.
const BACKUP_FORMAT: u64 = 2;
const CALENDAR_DB: &str = "calendar.sqlite";

struct PrivateDir(PathBuf);

impl PrivateDir {
    fn create(parent: &Path, purpose: &str) -> anyhow::Result<Self> {
        let path = parent.join(format!(".{purpose}-{}", new_id()));
        let mut builder = DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).with_context(|| format!("creating {}", path.display()))?;
        Ok(Self(path))
    }

    fn keep(mut self) -> PathBuf {
        std::mem::take(&mut self.0)
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() && let Err(error) = std::fs::remove_dir_all(&self.0) {
            tracing::warn!("could not clean up backup staging {}: {error}", self.0.display());
        }
    }
}

fn private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupConfig {
    pub enabled: bool,
    /// Folder inside the container (mount a host folder there).
    pub dir: String,
    /// Local time of day of the first run, `HH:MM`.
    pub start_time: String,
    pub interval_hours: u32,
    /// Number of backups to keep (0 = keep all).
    pub keep: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackupStatus {
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_ok: Option<bool>,
    pub last_file: Option<String>,
    pub last_error: Option<String>,
    pub last_size: Option<u64>,
    pub last_pause_ms: Option<u64>,
    pub last_duration_ms: Option<u64>,
    pub last_trigger: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BackupFile {
    pub name: String,
    pub size: u64,
    pub modified: String,
}

pub fn default_config(state: &AppState) -> BackupConfig {
    BackupConfig {
        enabled: false,
        dir: state.cfg.backup_dir.display().to_string(),
        start_time: "03:00".into(),
        interval_hours: 24,
        keep: 14,
    }
}

async fn get_setting<T: for<'de> Deserialize<'de>>(state: &AppState, key: &str) -> AppResult<Option<T>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?").bind(key).fetch_optional(&state.pool).await?;
    Ok(row.and_then(|(v,)| serde_json::from_str(&v).ok()))
}

async fn set_setting<T: Serialize>(state: &AppState, key: &str, value: &T) -> AppResult<()> {
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(key)
        .bind(serde_json::to_string(value).unwrap())
        .execute(&state.pool)
        .await?;
    Ok(())
}

pub async fn load_config(state: &AppState) -> AppResult<BackupConfig> {
    Ok(get_setting(state, "backup_config").await?.unwrap_or_else(|| default_config(state)))
}

pub async fn load_status(state: &AppState) -> AppResult<BackupStatus> {
    Ok(get_setting(state, "backup_status").await?.unwrap_or_default())
}

fn validate(cfg: &BackupConfig) -> AppResult<NaiveTime> {
    if !Path::new(&cfg.dir).is_absolute() {
        return Err(AppError::bad_request("the backup folder must be an absolute path"));
    }
    if cfg.interval_hours == 0 || cfg.interval_hours > 24 * 31 {
        return Err(AppError::bad_request("interval must be between 1 hour and 31 days"));
    }
    NaiveTime::parse_from_str(&cfg.start_time, "%H:%M").map_err(|_| AppError::bad_request("start time must be HH:MM"))
}

fn local_to_utc(t: NaiveDateTime) -> DateTime<Utc> {
    // in a DST gap the wall-clock time does not exist; run at the first time after it
    Local
        .from_local_datetime(&t)
        .earliest()
        .or_else(|| Local.from_local_datetime(&(t + Duration::hours(1))).earliest())
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|| t.and_utc())
}

/// First run time strictly after `after` on the schedule that starts at `start` (local time)
/// on `anchor_day`. Whole-day intervals step in local calendar days, so runs stay at `start`
/// across DST changes. Other intervals step in hours from that day's `start`.
pub fn next_run(start: NaiveTime, interval_hours: u32, anchor_day: NaiveDate, after: DateTime<Utc>) -> DateTime<Utc> {
    if interval_hours % 24 == 0 {
        let days = (interval_hours / 24) as i64;
        let today = after.with_timezone(&Local).date_naive();
        let k = (today - anchor_day).num_days().div_euclid(days);
        let mut day = anchor_day + Duration::days(k * days);
        loop {
            let t = local_to_utc(day.and_time(start));
            if t > after {
                return t;
            }
            day += Duration::days(days);
        }
    }
    let anchor = local_to_utc(anchor_day.and_time(start));
    let step = Duration::hours(interval_hours as i64);
    let k = (after - anchor).num_seconds().div_euclid(step.num_seconds());
    let mut t = anchor + step * k as i32;
    while t <= after {
        t += step;
    }
    t
}

/// The next due time after a backup or a config change. `keep_pending` keeps a due time
/// that is still in the future (manual runs, saving unchanged timing). Otherwise the
/// schedule continues from the previous due time, or starts today without one.
pub fn reschedule(start: NaiveTime, interval_hours: u32, prev: Option<DateTime<Utc>>, keep_pending: bool, now: DateTime<Utc>) -> DateTime<Utc> {
    if let Some(p) = prev.filter(|p| keep_pending && *p > now) {
        return p;
    }
    let anchor_day = prev.unwrap_or(now).with_timezone(&Local).date_naive();
    next_run(start, interval_hours, anchor_day, now)
}

pub async fn save_config(state: &AppState, cfg: BackupConfig) -> AppResult<(BackupConfig, BackupStatus)> {
    let start = validate(&cfg)?;
    crate::service::check_output_dir(state, Path::new(&cfg.dir))?;
    ensure_writable(Path::new(&cfg.dir)).map_err(|e| AppError::bad_request(format!("{e:#}")))?;
    let old = load_config(state).await?;
    set_setting(state, "backup_config", &cfg).await?;
    let mut status = load_status(state).await?;
    // changing only the folder or retention keeps the schedule; new timing starts today
    let same_timing = old.enabled && old.start_time == cfg.start_time && old.interval_hours == cfg.interval_hours;
    let prev = status.next_run_at.as_deref().and_then(parse_ts).filter(|_| same_timing);
    status.next_run_at = cfg.enabled.then(|| reschedule(start, cfg.interval_hours, prev, true, Utc::now()).to_rfc3339());
    set_setting(state, "backup_status", &status).await?;
    Ok((cfg, status))
}

fn ensure_writable(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let _probe = PrivateDir::create(dir, "pdfsign-write-test")?;
    Ok(())
}

/// Middleware: state-changing requests wait while a backup snapshot is being taken.
pub async fn write_gate(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if req.method() == Method::DELETE && path == "/api/admin/data" {
        let _guard = state.gate.write().await;
        return next.run(req).await;
    }
    // Auth callbacks can write records. Manual backups take their own guard.
    let read_only = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS)
        && !path.starts_with("/auth/");
    if read_only || path == "/api/admin/backup/run" {
        return next.run(req).await;
    }
    let _guard = state.gate.clone().read_owned().await;
    next.run(req).await
}

pub async fn run_backup(state: &AppState, trigger: &str) -> AppResult<BackupStatus> {
    let _running = state
        .backup_running
        .try_lock()
        .map_err(|_| AppError::conflict("backup_running", "a backup is already running"))?;
    let cfg = load_config(state).await?;
    let started = Instant::now();
    let result = do_backup(state, &cfg).await;
    let mut status = load_status(state).await?;
    status.last_run_at = Some(now());
    status.last_trigger = Some(trigger.into());
    status.last_duration_ms = Some(started.elapsed().as_millis() as u64);
    match result {
        Ok((file, size, pause_ms)) => {
            crate::activity::info(&state.pool, "backup", Some(trigger), format!("backup written: {file} ({size} bytes, app paused {pause_ms} ms)")).await;
            status.last_ok = Some(true);
            status.last_file = Some(file);
            status.last_size = Some(size);
            status.last_pause_ms = Some(pause_ms);
            status.last_error = None;
        }
        Err(e) => {
            crate::activity::error(&state.pool, "backup", Some(trigger), format!("backup failed: {e:#}")).await;
            status.last_ok = Some(false);
            status.last_error = Some(format!("{e:#}"));
        }
    }
    if cfg.enabled {
        if let Ok(start) = validate(&cfg) {
            // a manual or CLI run does not move the schedule
            let prev = status.next_run_at.as_deref().and_then(parse_ts);
            status.next_run_at = Some(reschedule(start, cfg.interval_hours, prev, trigger != "schedule", Utc::now()).to_rfc3339());
        }
    }
    set_setting(state, "backup_status", &status).await?;
    Ok(status)
}

async fn do_backup(state: &AppState, cfg: &BackupConfig) -> anyhow::Result<(String, u64, u64)> {
    let dir = PathBuf::from(&cfg.dir);
    ensure_writable(&dir)?;
    let stamp = Local::now().format("%Y%m%d-%H%M%S-%3f");
    let name = format!("backup-{stamp}-{}.zip", random_token(8));
    let staging = PrivateDir::create(&dir, "staging")?;
    let snapshot_dir = &staging.0;
    std::fs::create_dir(snapshot_dir.join("templates"))?;
    if state.cfg.events_enabled {
        let bytes = crate::events::backup_snapshot(state).await?;
        let mut file = private_file(&snapshot_dir.join(CALENDAR_DB))?;
        file.write_all(&bytes)?;
        validate_calendar_snapshot(&snapshot_dir.join(CALENDAR_DB)).await?;
    }

    // ---- pause: wait for running writes, hold new ones
    let pause_start = Instant::now();
    let guard = tokio::time::timeout(std::time::Duration::from_secs(120), state.gate.write())
        .await
        .context("the app stayed busy for 2 minutes - backup skipped")?;
    let snapshot: anyhow::Result<()> = async {
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&state.pool).await?;
        let db_path = snapshot_dir.join("pdfsign.db");
        sqlx::query("VACUUM INTO ?").bind(db_path.display().to_string()).execute(&state.pool).await.context("VACUUM INTO")?;
        Ok(())
    }
    .await;
    drop(guard);
    let pause_ms = pause_start.elapsed().as_millis() as u64;
    // ---- resumed
    snapshot?;
    // Template PDFs are never rewritten (a new upload gets a new id), so the ones the
    // snapshot references are copied after the pause instead of during it.
    copy_template_pdfs(state, snapshot_dir).await?;
    // a CLI backup does not share the running server's gate, so check the copy is complete
    validate_snapshot(snapshot_dir).await.context("snapshot check")?;
    clear_signing_sessions(snapshot_dir).await?;
    let final_path = dir.join(&name);
    let calendar = state.cfg.events_enabled;
    let tmp_path = snapshot_dir.join(&name);
    let staging2 = snapshot_dir.clone();
    let tmp2 = tmp_path.clone();
    let size = tokio::task::spawn_blocking(move || -> anyhow::Result<u64> {
        let f = private_file(&tmp2)?;
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true);
        let manifest = serde_json::json!({
            "app": "pdfsign", "format": BACKUP_FORMAT, "created_at": now(), "calendar": calendar,
        });
        zw.start_file("manifest.json", opts)?;
        zw.write_all(serde_json::to_vec_pretty(&manifest)?.as_slice())?;
        zw.start_file("pdfsign.db", opts)?;
        std::io::copy(&mut std::fs::File::open(staging2.join("pdfsign.db"))?, &mut zw)?;
        if calendar {
            zw.start_file(CALENDAR_DB, opts)?;
            std::io::copy(&mut File::open(staging2.join(CALENDAR_DB))?, &mut zw)?;
        }
        let mut templates: Vec<_> = std::fs::read_dir(staging2.join("templates"))?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        templates.sort();
        for p in templates {
            zw.start_file(format!("templates/{}", p.file_name().unwrap().to_string_lossy()), opts)?;
            std::io::copy(&mut std::fs::File::open(&p)?, &mut zw)?;
        }
        let f = zw.finish()?;
        f.sync_all()?;
        Ok(f.metadata()?.len())
    })
    .await??;
    std::fs::rename(&tmp_path, &final_path)?;
    prune(&dir, cfg.keep)?;
    Ok((name, size, pause_ms))
}

async fn copy_template_pdfs(state: &AppState, snapshot_dir: &Path) -> anyhow::Result<()> {
    let mut conn = SqliteConnectOptions::new().filename(snapshot_dir.join("pdfsign.db")).read_only(true).connect().await
        .context("opening the snapshot")?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM templates").fetch_all(&mut conn).await.context("reading templates")?;
    conn.close().await?;
    for id in ids {
        anyhow::ensure!(uuid::Uuid::parse_str(&id).is_ok(), "invalid template id {id}");
        let name = format!("{id}.pdf");
        tokio::fs::copy(state.cfg.data_dir.join("templates").join(&name), snapshot_dir.join("templates").join(&name))
            .await
            .with_context(|| format!("copying template PDF {id} (deleted during the backup? retry)"))?;
    }
    Ok(())
}

fn is_backup_name(n: &str) -> bool {
    n.strip_prefix("backup-").and_then(|n| n.strip_suffix(".zip"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
}

pub fn list_backups(dir: &Path) -> Vec<BackupFile> {
    let mut out: Vec<BackupFile> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if !e.file_type().ok()?.is_file() {
                return None;
            }
            let meta = e.metadata().ok()?;
            is_backup_name(&name).then(|| BackupFile {
                name,
                size: meta.len(),
                modified: meta.modified().ok().map(|m| DateTime::<Utc>::from(m).to_rfc3339()).unwrap_or_default(),
            })
        })
        .collect();
    out.sort_by(|a, b| b.name.cmp(&a.name));
    out
}

fn prune(dir: &Path, keep: u32) -> anyhow::Result<()> {
    if keep == 0 {
        return Ok(());
    }
    for old in list_backups(dir).into_iter().skip(keep as usize) {
        std::fs::remove_file(dir.join(&old.name))?;
        tracing::info!("pruned old backup {}", old.name);
    }
    Ok(())
}

pub fn backup_path(dir: &str, name: &str) -> AppResult<PathBuf> {
    if !is_backup_name(name) {
        return Err(AppError::bad_request("invalid backup name"));
    }
    Ok(PathBuf::from(dir).join(name))
}

/// Background scheduler.
pub fn spawn_scheduler(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tick.tick().await;
            let (Ok(cfg), Ok(status)) = (load_config(&state).await, load_status(&state).await) else { continue };
            if !cfg.enabled {
                continue;
            }
            let due = status.next_run_at.as_deref().and_then(parse_ts).is_none_or(|t| t <= Utc::now());
            if due {
                if let Err(e) = run_backup(&state, "schedule").await {
                    crate::activity::error(&state.pool, "backup", Some("schedule"), format!("scheduled backup: {}", e.message)).await;
                }
            }
        }
    });
}

/// Opens a staged `pdfsign.db`, runs SQLite's integrity check, and verifies that every
/// template it references has its PDF in `<dir>/templates`.
async fn validate_snapshot(dir: &Path) -> anyhow::Result<()> {
    let db = dir.join("pdfsign.db");
    let mut conn = checked_database(&db).await?;
    let ids: Vec<(String,)> = sqlx::query_as("SELECT id FROM templates").fetch_all(&mut conn).await.context("reading templates")?;
    sqlx::query("SELECT COUNT(*) FROM submissions").execute(&mut conn).await.context("reading submissions")?;
    conn.close().await?;
    let missing: Vec<&str> = ids.iter().map(|(id,)| id.as_str()).filter(|id| {
        uuid::Uuid::parse_str(id).is_err() || !dir.join("templates").join(format!("{id}.pdf")).is_file()
    }).collect();
    if !missing.is_empty() {
        bail!("{} template PDF(s) missing: {}", missing.len(), missing.join(", "));
    }
    Ok(())
}

async fn checked_database(path: &Path) -> anyhow::Result<SqliteConnection> {
    let mut conn = SqliteConnectOptions::new().filename(path).read_only(true).connect().await
        .with_context(|| format!("opening {}", path.display()))?;
    let check: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check").fetch_all(&mut conn).await.context("integrity check")?;
    anyhow::ensure!(check.len() == 1 && check[0] == "ok", "database integrity check failed for {}", path.display());
    Ok(conn)
}

async fn validate_calendar_snapshot(path: &Path) -> anyhow::Result<()> {
    let mut conn = checked_database(path).await?;
    for table in ["events", "event_rsvps", "event_credentials", "webhook_outbox"] {
        anyhow::ensure!(has_table(&mut conn, table).await?, "calendar snapshot is missing {table}");
    }
    conn.close().await?;
    Ok(())
}

async fn has_table(conn: &mut SqliteConnection, table: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?)")
        .bind(table).fetch_one(conn).await
}

async fn writable_snapshot(path: &Path) -> anyhow::Result<SqliteConnection> {
    Ok(SqliteConnectOptions::new().filename(path)
        .pragma("secure_delete", "ON").pragma("trusted_schema", "OFF")
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete).connect().await?)
}

const SESSION_TABLES: [(&str, &str); 3] = [
    ("user_sessions", "DELETE FROM user_sessions"),
    ("signer_sessions", "DELETE FROM signer_sessions"),
    ("oidc_states", "DELETE FROM oidc_states"),
];

async fn clear_signing_sessions(dir: &Path) -> anyhow::Result<()> {
    let mut conn = writable_snapshot(&dir.join("pdfsign.db")).await?;
    clear_tables(&mut conn, &SESSION_TABLES).await?;
    conn.close().await?;
    Ok(())
}

async fn clear_tables(conn: &mut SqliteConnection, tables: &[(&str, &'static str)]) -> anyhow::Result<()> {
    for &(table, delete) in tables {
        if has_table(conn, table).await? {
            sqlx::query(delete).execute(&mut *conn).await?;
        }
    }
    Ok(())
}

async fn retire_signing_credentials(dir: &Path) -> anyhow::Result<()> {
    let mut conn = writable_snapshot(&dir.join("pdfsign.db")).await?;
    let mut tx = conn.begin().await?;
    clear_tables(&mut tx, &SESSION_TABLES).await?;
    clear_tables(&mut tx, &[("api_keys", "DELETE FROM api_keys")]).await?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM submissions").fetch_all(&mut *tx).await?;
    for id in ids {
        sqlx::query("UPDATE submissions SET token = ?, failed_attempts = 0 WHERE id = ?")
            .bind(random_token(26)).bind(id).execute(&mut *tx).await?;
    }
    if has_table(&mut tx, "public_links").await? {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM public_links").fetch_all(&mut *tx).await?;
        for id in ids {
            sqlx::query("UPDATE public_links SET token = ?, active = 0 WHERE id = ?")
                .bind(random_token(26)).bind(id).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    conn.close().await?;
    Ok(())
}

async fn retire_calendar_credentials(path: &Path) -> anyhow::Result<()> {
    let mut conn = writable_snapshot(path).await?;
    let mut tx = conn.begin().await?;
    clear_tables(&mut tx, &[
        ("event_credentials", "DELETE FROM event_credentials"),
        ("webhook_outbox", "DELETE FROM webhook_outbox"),
    ]).await?;
    tx.commit().await?;
    conn.close().await?;
    Ok(())
}

const CALENDAR_FILES: [&str; 3] = [CALENDAR_DB, "calendar.sqlite-wal", "calendar.sqlite-shm"];
const SIGNING_FILES: [&str; 4] = ["pdfsign.db", "pdfsign.db-wal", "pdfsign.db-shm", "templates"];

pub struct RestoreResult {
    pub signing_previous: Option<PathBuf>,
    pub calendar_previous: Option<PathBuf>,
    pub calendar_restored: bool,
}

struct RestoreFiles {
    root: PathBuf,
    staging: PrivateDir,
    previous: Option<PrivateDir>,
    names: &'static [&'static str],
    moved: Vec<&'static str>,
    installed: Vec<&'static str>,
}

impl RestoreFiles {
    fn new(root: &Path, names: &'static [&'static str], force: bool) -> anyhow::Result<Self> {
        if names.iter().any(|name| root.join(name).symlink_metadata().is_ok()) && !force {
            bail!("{} already contains data; stop the service and pass --force to restore", root.display());
        }
        std::fs::create_dir_all(root)?;
        let staging = PrivateDir::create(root, "restore")?;
        Ok(Self { root: root.to_path_buf(), staging, previous: None, names, moved: Vec::new(), installed: Vec::new() })
    }

    fn install(&mut self) -> anyhow::Result<()> {
        for &name in self.names {
            let current = self.root.join(name);
            match current.symlink_metadata() {
                Ok(_) => {
                    if self.previous.is_none() {
                        self.previous = Some(PrivateDir::create(&self.root, "pre-restore")?);
                    }
                    let previous = self.previous.as_ref().unwrap();
                    std::fs::rename(current, previous.0.join(name)).with_context(|| format!("moving {name} aside"))?;
                    self.moved.push(name);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        for &name in self.names {
            let path = self.staging.0.join(name);
            if path.exists() {
                std::fs::rename(path, self.root.join(name)).with_context(|| format!("installing {name}"))?;
                self.installed.push(name);
            }
        }
        Ok(())
    }

    fn rollback(&mut self, errors: &mut Vec<String>) {
        for &name in self.installed.iter().rev() {
            let path = self.root.join(name);
            let result = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            if let Err(error) = result {
                errors.push(format!("removing {}: {error}", path.display()));
            }
        }
        if let Some(previous) = self.previous.take() {
            let mut failed = false;
            for &name in self.moved.iter().rev() {
                if let Err(error) = std::fs::rename(previous.0.join(name), self.root.join(name)) {
                    errors.push(format!("restoring {name}: {error}"));
                    failed = true;
                }
            }
            if failed {
                errors.push(format!("remaining previous data is in {}", previous.keep().display()));
            }
        }
    }
}

/// Restores a backup archive into `data_dir`. The server must not be running.
///
/// The archive is extracted into a staging folder next to the data and validated first
/// (SQLite integrity, template PDFs). Only then is the current database and template folder
/// moved to `.pre-restore-<stamp>` and the staged copy moved in. If installing fails, the
/// previous data is moved back. Returns the folder holding the previous data, if any.
pub async fn restore(
    archive: &Path, data_dir: &Path, calendar_dir: Option<&Path>, signing_only: bool, force: bool,
) -> anyhow::Result<RestoreResult> {
    let mut zip = zip::ZipArchive::new(File::open(archive).with_context(|| format!("opening {}", archive.display()))?)?;
    if zip.by_name("pdfsign.db").is_err() {
        bail!("{} is not a pdfsign backup", archive.display());
    }
    let format = match zip.by_name("manifest.json") {
        Ok(entry) => serde_json::from_reader::<_, serde_json::Value>(entry.take(64 * 1024)).ok().and_then(|m| m["format"].as_u64()),
        Err(_) => None,
    };
    anyhow::ensure!(format == Some(BACKUP_FORMAT), "{} was made by an earlier pdfsign version and cannot be restored", archive.display());
    let includes_calendar = zip.by_name(CALENDAR_DB).is_ok();
    anyhow::ensure!(!includes_calendar || calendar_dir.is_some() || signing_only,
        "this archive includes calendar data; pass --calendar-data-dir or explicitly select --signing-only");
    anyhow::ensure!(includes_calendar || calendar_dir.is_none(), "this archive does not contain calendar data");
    let mut signing = RestoreFiles::new(data_dir, &SIGNING_FILES, force)?;
    let mut calendar = calendar_dir.map(|dir| RestoreFiles::new(dir, &CALENDAR_FILES, force)).transpose()?;
    if let Some(calendar) = &calendar {
        let signing_root = data_dir.canonicalize()?;
        let calendar_root = calendar.root.canonicalize()?;
        anyhow::ensure!(calendar_root != signing_root && !calendar_root.starts_with(signing_root.join("templates")),
            "calendar data must be outside the signing template directory");
    }
    let staging = &signing.staging.0;
    let staged = async {
        std::fs::create_dir(staging.join("templates"))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let name = entry.name()?.to_string();
            let target = match name.as_str() {
                "pdfsign.db" => staging.join("pdfsign.db"),
                CALENDAR_DB => {
                    let Some(calendar) = &calendar else { continue };
                    anyhow::ensure!(entry.size() <= CALENDAR_MAX_BYTES as u64, "calendar snapshot exceeds 128 MiB");
                    calendar.staging.0.join(CALENDAR_DB)
                }
                n if n.strip_prefix("templates/").and_then(|n| n.strip_suffix(".pdf"))
                    .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok()) => staging.join(n),
                _ => continue,
            };
            let mut out = private_file(&target)?;
            if name == CALENDAR_DB {
                let copied = std::io::copy(&mut entry.take(CALENDAR_MAX_BYTES as u64 + 1), &mut out)?;
                anyhow::ensure!(copied <= CALENDAR_MAX_BYTES as u64, "calendar snapshot exceeds 128 MiB");
            } else {
                std::io::copy(&mut entry, &mut out)?;
            }
            out.sync_all()?;
        }
        validate_snapshot(staging).await?;
        retire_signing_credentials(staging).await?;
        if let Some(calendar) = &calendar {
            let snapshot = calendar.staging.0.join(CALENDAR_DB);
            validate_calendar_snapshot(&snapshot).await?;
            retire_calendar_credentials(&snapshot).await?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(e) = staged {
        return Err(e.context(format!("{} was not restored; the current data is unchanged", archive.display())));
    }

    let install = (|| -> anyhow::Result<()> {
        signing.install()?;
        if let Some(calendar) = &mut calendar {
            calendar.install()?;
        }
        Ok(())
    })();
    if let Err(e) = install {
        let mut rollback_errors = Vec::new();
        if let Some(calendar) = &mut calendar {
            calendar.rollback(&mut rollback_errors);
        }
        signing.rollback(&mut rollback_errors);
        if rollback_errors.is_empty() {
            return Err(e.context("restore failed; the previous data was put back"));
        }
        return Err(e.context(format!("restore failed and rollback was incomplete: {}", rollback_errors.join(", "))));
    }
    Ok(RestoreResult {
        signing_previous: signing.previous.take().map(PrivateDir::keep),
        calendar_restored: calendar.is_some(),
        calendar_previous: calendar.and_then(|mut files| files.previous.take().map(PrivateDir::keep)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: NaiveDate, h: u32, m: u32) -> DateTime<Utc> {
        local_to_utc(day.and_hms_opt(h, m, 0).unwrap())
    }

    #[test]
    fn next_run_is_after_now_and_aligned() {
        let start = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
        let now = Utc::now();
        let today = now.with_timezone(&Local).date_naive();
        let n = next_run(start, 24, today, now);
        assert!(n > now && n <= now + Duration::hours(25));
        assert_eq!(n.with_timezone(&Local).format("%H:%M").to_string(), "03:00");
        let n6 = next_run(start, 6, today, now);
        assert!(n6 > now && n6 <= now + Duration::hours(6));
    }

    #[test]
    fn multi_day_interval_survives_manual_runs() {
        let start = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
        let d0 = NaiveDate::from_ymd_opt(2026, 3, 25).unwrap();
        // scheduled run on day 0 -> next on day 3
        let due = reschedule(start, 72, Some(at(d0, 3, 0)), false, at(d0, 3, 1));
        assert_eq!(due, at(d0 + Duration::days(3), 3, 0));
        // a manual run on day 1 keeps it
        assert_eq!(reschedule(start, 72, Some(due), true, at(d0 + Duration::days(1), 10, 0)), due);
        // the scheduled run on day 3 continues the grid, across the DST change in between
        let next = reschedule(start, 72, Some(due), false, due + Duration::seconds(30));
        assert_eq!(next, at(d0 + Duration::days(6), 3, 0));
        assert_eq!(next.with_timezone(&Local).format("%H:%M").to_string(), "03:00");
        // missed runs (server was down) resume on the same grid
        let late = reschedule(start, 72, Some(due), false, at(d0 + Duration::days(8), 12, 0));
        assert_eq!(late, at(d0 + Duration::days(9), 3, 0));
    }
}
