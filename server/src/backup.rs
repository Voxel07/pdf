//! Scheduled backups.
//!
//! A backup briefly pauses the app: every state-changing request holds a read guard on
//! `AppState::gate`; the backup takes the write guard, which waits for in-flight requests to
//! finish and holds new ones until the snapshot is taken (they are delayed, not rejected).
//! While paused, the WAL is checkpointed and `VACUUM INTO` writes a consistent copy of the
//! database. That copy includes all user and signer sessions, so nobody is logged out after a
//! restore. The template PDFs are copied too. Then the app resumes, and the snapshot is
//! zipped into `<dir>/backup-YYYYMMDD-HHMMSS.zip`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context};
use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::state::AppState;
use crate::util::*;

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
    if !cfg.dir.starts_with('/') {
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
    let probe = dir.join(".pdfsign-write-test");
    std::fs::write(&probe, b"ok").with_context(|| format!("{} is not writable", dir.display()))?;
    let _ = std::fs::remove_file(probe);
    Ok(())
}

/// Middleware: state-changing requests wait while a backup snapshot is being taken.
pub async fn write_gate(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if req.method() == Method::DELETE && path == "/api/admin/data" {
        let _guard = state.gate.write().await;
        return next.run(req).await;
    }
    // Auth callbacks and ZIP exports also write records. Manual backups take their own guard.
    let read_only = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS)
        && !path.starts_with("/auth/") && path != "/api/admin/export.zip";
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
    let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let staging = dir.join(format!(".staging-{stamp}"));
    std::fs::create_dir_all(staging.join("templates"))?;

    // ---- pause: wait for running writes, hold new ones
    let pause_start = Instant::now();
    let guard = tokio::time::timeout(std::time::Duration::from_secs(120), state.gate.write())
        .await
        .context("the app stayed busy for 2 minutes - backup skipped")?;
    let snapshot: anyhow::Result<()> = async {
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&state.pool).await?;
        let db_path = staging.join("pdfsign.db");
        sqlx::query("VACUUM INTO ?").bind(db_path.display().to_string()).execute(&state.pool).await.context("VACUUM INTO")?;
        let tdir = state.cfg.data_dir.join("templates");
        if tdir.exists() {
            for e in std::fs::read_dir(&tdir)? {
                let p = e?.path();
                if p.extension().is_some_and(|x| x == "pdf") {
                    std::fs::copy(&p, staging.join("templates").join(p.file_name().unwrap()))?;
                }
            }
        }
        Ok(())
    }
    .await;
    drop(guard);
    let pause_ms = pause_start.elapsed().as_millis() as u64;
    // ---- resumed
    // a CLI backup does not share the running server's gate, so check the copy is complete
    let snapshot = match snapshot {
        Ok(()) => validate_snapshot(&staging).await.context("snapshot check"),
        Err(e) => Err(e),
    };
    if let Err(e) = snapshot {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    let name = format!("backup-{stamp}.zip");
    let final_path = dir.join(&name);
    let staging2 = staging.clone();
    let tmp_path = dir.join(format!(".{name}.tmp"));
    let tmp2 = tmp_path.clone();
    let size = tokio::task::spawn_blocking(move || -> anyhow::Result<u64> {
        let f = std::fs::File::create(&tmp2)?;
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true);
        let manifest = serde_json::json!({
            "app": "pdfsign", "format": 1, "created_at": now(),
            "restore": "pdfsign restore <this file> --force (with the server stopped)",
        });
        zw.start_file("manifest.json", opts)?;
        zw.write_all(serde_json::to_vec_pretty(&manifest)?.as_slice())?;
        zw.start_file("pdfsign.db", opts)?;
        std::io::copy(&mut std::fs::File::open(staging2.join("pdfsign.db"))?, &mut zw)?;
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
    let _ = std::fs::remove_dir_all(&staging);
    prune(&dir, cfg.keep)?;
    Ok((name, size, pause_ms))
}

fn is_backup_name(n: &str) -> bool {
    n.starts_with("backup-") && n.ends_with(".zip") && !n.contains('/') && !n.contains("..")
}

pub fn list_backups(dir: &Path) -> Vec<BackupFile> {
    let mut out: Vec<BackupFile> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
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
    use sqlx::{ConnectOptions, Connection};
    let db = dir.join("pdfsign.db");
    let mut conn = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&db)
        .create_if_missing(false)
        .connect()
        .await
        .with_context(|| format!("opening {}", db.display()))?;
    let check: Vec<(String,)> = sqlx::query_as("PRAGMA integrity_check").fetch_all(&mut conn).await.context("integrity check")?;
    if check.len() != 1 || check[0].0 != "ok" {
        bail!("database integrity check failed: {}", check.iter().map(|c| c.0.as_str()).take(5).collect::<Vec<_>>().join("; "));
    }
    let ids: Vec<(String,)> = sqlx::query_as("SELECT id FROM templates").fetch_all(&mut conn).await.context("reading templates")?;
    sqlx::query("SELECT COUNT(*) FROM submissions").execute(&mut conn).await.context("reading submissions")?;
    conn.close().await?;
    let missing: Vec<&str> = ids.iter().map(|(id,)| id.as_str()).filter(|id| !dir.join("templates").join(format!("{id}.pdf")).is_file()).collect();
    if !missing.is_empty() {
        bail!("{} template PDF(s) missing: {}", missing.len(), missing.join(", "));
    }
    Ok(())
}

const DB_FILES: [&str; 3] = ["pdfsign.db", "pdfsign.db-wal", "pdfsign.db-shm"];

/// Restores a backup archive into `data_dir`. The server must not be running.
///
/// The archive is extracted into a staging folder next to the data and validated first
/// (SQLite integrity, template PDFs). Only then is the current database and template folder
/// moved to `.pre-restore-<stamp>` and the staged copy moved in. If installing fails, the
/// previous data is moved back. Returns the folder holding the previous data, if any.
pub async fn restore(archive: &Path, data_dir: &Path, force: bool) -> anyhow::Result<Option<PathBuf>> {
    let db = data_dir.join("pdfsign.db");
    if db.exists() && !force {
        bail!("{} exists - stop the server and pass --force to overwrite it", db.display());
    }
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).with_context(|| format!("opening {}", archive.display()))?)?;
    if zip.by_name("pdfsign.db").is_err() {
        bail!("{} is not a pdfsign backup", archive.display());
    }
    std::fs::create_dir_all(data_dir)?;
    let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let staging = data_dir.join(format!(".restore-{stamp}"));
    let staged = async {
        std::fs::create_dir_all(staging.join("templates"))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let name = entry.name()?.to_string();
            let target = match name.as_str() {
                "pdfsign.db" => staging.join("pdfsign.db"),
                n if n.starts_with("templates/") && n.ends_with(".pdf") && !n.contains("..") && n.matches('/').count() == 1 => staging.join(n),
                _ => continue,
            };
            let mut out = std::fs::File::create(&target)?;
            std::io::copy(&mut entry, &mut out)?;
            out.sync_all()?;
        }
        validate_snapshot(&staging).await
    }
    .await;
    if let Err(e) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e.context(format!("{} was not restored; the current data is unchanged", archive.display())));
    }

    // move the current data aside
    let previous = data_dir.join(format!(".pre-restore-{stamp}"));
    let mut moved: Vec<&str> = Vec::new();
    let current: Vec<&str> = DB_FILES.iter().copied().chain(["templates"]).filter(|n| data_dir.join(n).exists()).collect();
    let install = (|| -> anyhow::Result<()> {
        if !current.is_empty() {
            std::fs::create_dir_all(&previous)?;
        }
        for n in &current {
            std::fs::rename(data_dir.join(n), previous.join(n)).with_context(|| format!("moving {n} aside"))?;
            moved.push(n);
        }
        for n in DB_FILES.iter().copied().chain(["templates"]) {
            if staging.join(n).exists() {
                std::fs::rename(staging.join(n), data_dir.join(n)).with_context(|| format!("installing {n}"))?;
            }
        }
        Ok(())
    })();
    if let Err(e) = install {
        // roll back: drop what was installed, put the previous data back
        for n in DB_FILES.iter().copied().chain(["templates"]) {
            if moved.contains(&n) || !current.contains(&n) {
                let p = data_dir.join(n);
                if p.is_dir() {
                    let _ = std::fs::remove_dir_all(&p);
                } else {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        let mut rollback_errors = Vec::new();
        for n in &moved {
            if let Err(re) = std::fs::rename(previous.join(n), data_dir.join(n)) {
                rollback_errors.push(format!("{n}: {re}"));
            }
        }
        if rollback_errors.is_empty() {
            let _ = std::fs::remove_dir(&previous);
            return Err(e.context("restore failed; the previous data was put back"));
        }
        return Err(e.context(format!("restore failed and rollback was incomplete ({}); the previous data is in {}", rollback_errors.join(", "), previous.display())));
    }
    let _ = std::fs::remove_dir_all(&staging);
    Ok((!current.is_empty()).then_some(previous))
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
