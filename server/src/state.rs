use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;

#[derive(Debug)]
pub struct Config {
    pub data_dir: PathBuf,
    pub export_dir: PathBuf,
    pub backup_dir: PathBuf,
    pub public_url: String,
    pub oidc: Option<crate::auth::OidcConfig>,
    pub web_dir: PathBuf,
    /// Failed name attempts before a link is locked.
    pub max_name_attempts: i64,
    /// Append an audit trail page to generated PDFs.
    pub audit_page: bool,
    /// Peers whose `X-Forwarded-For` / `X-Real-IP` headers are believed.
    pub trusted_proxies: Vec<crate::util::IpNet>,
}

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub cfg: Arc<Config>,
    pub limiter: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
    /// Write gate: mutating requests hold a read guard, backups take the write guard.
    pub gate: Arc<tokio::sync::RwLock<()>>,
    pub backup_running: Arc<tokio::sync::Mutex<()>>,
    /// Progress of the current or last DocuSeal import (one at a time).
    pub import: Arc<Mutex<crate::docuseal::ImportProgress>>,
    /// Running and recently finished export jobs by id (see export_job.rs).
    pub export_jobs: Arc<Mutex<HashMap<String, crate::export_job::ExportJob>>>,
}

impl AppState {
    pub async fn new(cfg: Config) -> Result<Self> {
        std::fs::create_dir_all(cfg.data_dir.join("templates"))
            .with_context(|| format!("creating {}", cfg.data_dir.display()))?;
        let db_path = cfg.data_dir.join("pdfsign.db");
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", db_path.display()))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new().max_connections(8).connect_with(opts).await?;
        sqlx::raw_sql(include_str!("schema.sql")).execute(&pool).await.context("migrating database")?;
        // columns added after the first release (CREATE TABLE IF NOT EXISTS won't add them)
        for (table, column, ddl) in [("persons", "zip", "ALTER TABLE persons ADD COLUMN zip TEXT")] {
            let cols: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info(?)").bind(table).fetch_all(&pool).await?;
            if !cols.iter().any(|c| c.0 == column) {
                sqlx::raw_sql(ddl).execute(&pool).await.with_context(|| format!("adding {table}.{column}"))?;
            }
        }
        Ok(Self { pool, cfg: Arc::new(cfg), limiter: Default::default(), gate: Default::default(), backup_running: Default::default(), import: Default::default(), export_jobs: Default::default() })
    }

    pub fn template_pdf_path(&self, template_id: &str) -> PathBuf {
        self.cfg.data_dir.join("templates").join(format!("{template_id}.pdf"))
    }

    pub fn sign_url(&self, token: &str) -> String {
        format!("{}/s/{}", self.cfg.public_url.trim_end_matches('/'), token)
    }

    pub fn public_link_url(&self, token: &str) -> String {
        format!("{}/p/{}", self.cfg.public_url.trim_end_matches('/'), token)
    }

    /// Sliding-window rate limit: at most `max` hits per `window` for `key`.
    pub fn rate_ok(&self, key: &str, max: usize, window: Duration) -> bool {
        let now = Instant::now();
        let mut map = self.limiter.lock().unwrap();
        if map.len() > 10_000 {
            map.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < window));
        }
        let q = map.entry(key.to_string()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= window) {
            q.pop_front();
        }
        if q.len() >= max {
            return false;
        }
        q.push_back(now);
        true
    }
}
