use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::util::{AppError, AppResult};

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
    pub events_enabled: bool,
    pub calendar_url: Option<reqwest::Url>,
    pub calendar_pdfsign_token: Option<String>,
    /// Peers whose `X-Forwarded-For` headers are believed.
    pub trusted_proxies: Vec<crate::util::IpNet>,
}

/// Parsed template PDFs by template id, checked against the stored SHA-256.
type TemplateCache = HashMap<String, (String, Arc<lopdf::Document>)>;
const TEMPLATE_CACHE_ENTRIES: usize = 16;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub cfg: Arc<Config>,
    limiter: Arc<Mutex<RateLimits>>,
    /// Write gate: mutating requests hold a read guard, backups take the write guard.
    pub gate: Arc<tokio::sync::RwLock<()>>,
    pub backup_running: Arc<tokio::sync::Mutex<()>>,
    /// Progress of the current or last DocuSeal import (one at a time).
    pub import: Arc<Mutex<crate::docuseal::ImportProgress>>,
    /// Running and recently finished export jobs by id (see export_job.rs).
    pub export_jobs: Arc<Mutex<HashMap<String, crate::export_job::ExportJob>>>,
    /// Bounds concurrent CPU-heavy work (PDF rendering, image decoding) on the blocking pool.
    cpu: Arc<tokio::sync::Semaphore>,
    templates: Arc<Mutex<TemplateCache>>,
    /// Set once the missing-`TRUSTED_PROXIES` warning was logged.
    pub forwarded_warning: Arc<AtomicBool>,
}

#[derive(Default)]
struct RateLimits {
    buckets: HashMap<String, VecDeque<Instant>>,
    next_cleanup: Option<Instant>,
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
        // schema.sql only creates missing tables, so a database from 0.0.3 or earlier would keep
        // its old tables and fail on the first new column with a bare SQL error.
        let (has_persons, has_name_key): (bool, bool) = sqlx::query_as(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'persons'), \
                    EXISTS(SELECT 1 FROM pragma_table_info('persons') WHERE name = 'name_key')",
        )
        .fetch_one(&pool)
        .await?;
        anyhow::ensure!(
            !has_persons || has_name_key,
            "{} was created by pdfsign 0.0.3 or earlier, which this version cannot open. \
             Stop pdfsign, move the data folder aside, start with an empty DATA_DIR, and re-import from DocuSeal if needed",
            db_path.display()
        );
        sqlx::raw_sql(include_str!("schema.sql")).execute(&pool).await.context("creating the database schema")?;
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).max(2);
        let state = Self {
            pool,
            cfg: Arc::new(cfg),
            limiter: Default::default(),
            gate: Default::default(),
            backup_running: Default::default(),
            import: Default::default(),
            export_jobs: Default::default(),
            cpu: Arc::new(tokio::sync::Semaphore::new(workers)),
            templates: Default::default(),
            forwarded_warning: Default::default(),
        };
        if let Err(e) = state.clean_deleted_pdfs().await {
            tracing::error!("pending template PDF cleanup: {e:#}");
        }
        Ok(state)
    }

    pub async fn clean_deleted_pdfs(&self) -> Result<()> {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM deleted_template_pdfs").fetch_all(&self.pool).await?;
        for id in ids {
            anyhow::ensure!(uuid::Uuid::parse_str(&id).is_ok(), "invalid template PDF id");
            self.templates.lock().unwrap().remove(&id);
            match tokio::fs::remove_file(self.template_pdf_path(&id)).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).context("records deleted, but PDF cleanup is pending"),
            }
            sqlx::query("DELETE FROM deleted_template_pdfs WHERE id = ?").bind(id).execute(&self.pool).await?;
        }
        Ok(())
    }

    pub fn template_pdf_path(&self, template_id: &str) -> PathBuf {
        self.cfg.data_dir.join("templates").join(format!("{template_id}.pdf"))
    }

    /// Runs CPU-heavy work on the blocking pool, at most one job per core at a time, so
    /// renders and image decoding never stall the async workers that serve other requests.
    pub async fn blocking<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> AppResult<T> {
        let _permit = self.cpu.clone().acquire_owned().await.map_err(|e| anyhow::anyhow!(e))?;
        tokio::task::spawn_blocking(f).await.map_err(|e| AppError::from(anyhow::anyhow!("worker failed: {e}")))
    }

    /// The parsed template PDF, from the cache when its hash still matches.
    pub async fn template_document(&self, template_id: &str, sha256: &str) -> AppResult<Arc<lopdf::Document>> {
        if let Some((hash, doc)) = self.templates.lock().unwrap().get(template_id) {
            if hash == sha256 {
                return Ok(doc.clone());
            }
        }
        let bytes = crate::service::template_pdf(self, template_id).await?;
        let doc = Arc::new(self.blocking(move || crate::pdf::load(&bytes)).await??);
        let mut cache = self.templates.lock().unwrap();
        if cache.len() >= TEMPLATE_CACHE_ENTRIES && !cache.contains_key(template_id) {
            if let Some(key) = cache.keys().next().cloned() {
                cache.remove(&key);
            }
        }
        cache.insert(template_id.to_string(), (sha256.to_string(), doc.clone()));
        Ok(doc)
    }

    pub fn sign_url(&self, token: &str) -> String {
        format!("{}/s/{}", self.cfg.public_url.trim_end_matches('/'), token)
    }

    pub fn public_link_url(&self, token: &str) -> String {
        format!("{}/p/{}", self.cfg.public_url.trim_end_matches('/'), token)
    }

    pub fn clear_rate_limits(&self) {
        *self.limiter.lock().unwrap() = RateLimits::default();
        self.templates.lock().unwrap().clear();
    }

    /// Logs once when proxied requests arrive but `TRUSTED_PROXIES` does not trust the proxy:
    /// every client then shares the proxy's address and its rate limits.
    pub fn warn_untrusted_forwarding(&self) {
        if !self.forwarded_warning.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "requests carry X-Forwarded-For from a peer that TRUSTED_PROXIES does not list; \
                 all clients behind that proxy share one rate limit and audit IP. Set TRUSTED_PROXIES to the proxy address."
            );
        }
    }

    /// Sliding-window rate limit: at most `max` hits per `window` for `key`.
    pub fn rate_ok(&self, key: &str, max: usize) -> bool {
        const MAX_BUCKETS: usize = 16_384;
        const WINDOW: Duration = Duration::from_secs(600);
        let now = Instant::now();
        let mut limits = self.limiter.lock().unwrap();
        if limits.next_cleanup.is_none_or(|cleanup| now >= cleanup) {
            limits.buckets.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < WINDOW));
            limits.next_cleanup = Some(now + Duration::from_secs(30));
        }
        if limits.buckets.len() >= MAX_BUCKETS && !limits.buckets.contains_key(key) {
            // make room by forgetting the least recently active client rather than refusing
            // every new one (which would let a flood of addresses lock everybody out)
            let oldest = limits.buckets.iter().min_by_key(|(_, q)| q.back().copied()).map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                limits.buckets.remove(&oldest);
            }
        }
        let q = limits.buckets.entry(key.to_string()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= WINDOW) {
            q.pop_front();
        }
        if q.len() >= max {
            return false;
        }
        q.push_back(now);
        true
    }
}

/// The rate-limit identity of a client address: IPv6 clients are grouped by their /64,
/// since a single host usually controls a whole /64.
pub fn rate_key(ip: &str) -> String {
    match ip.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(v6)) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        _ => ip.to_string(),
    }
}
