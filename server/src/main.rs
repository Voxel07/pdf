mod activity;
mod auth;
mod backup;
mod docuseal;
mod export_job;
mod models;
mod pdf;
mod routes;
mod service;
mod state;
#[cfg(test)]
mod tests;
mod util;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use axum::response::Redirect;
use axum::routing::get;
use axum::Router;
use clap::{Parser, Subcommand};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::state::{AppState, Config};

#[derive(Parser)]
#[command(name = "pdfsign", about = "Self-hosted PDF signing service")]
struct Cli {
    /// Directory for the SQLite database and template PDFs.
    #[arg(long, env = "DATA_DIR", default_value = "./data", global = true)]
    data_dir: PathBuf,
    /// Default directory for PDF exports.
    #[arg(long, env = "EXPORT_DIR", global = true)]
    export_dir: Option<PathBuf>,
    /// Public base URL used to build signing links.
    #[arg(long, env = "PUBLIC_URL", default_value = "http://localhost:8080", global = true)]
    public_url: String,
    /// OIDC issuer, e.g. https://auth.example.com/application/o/pdfsign/
    #[arg(long, env = "OIDC_ISSUER", global = true)]
    oidc_issuer: Option<String>,
    #[arg(long, env = "OIDC_CLIENT_ID", global = true)]
    oidc_client_id: Option<String>,
    #[arg(long, env = "OIDC_CLIENT_SECRET", global = true, hide_env_values = true)]
    oidc_client_secret: Option<String>,
    #[arg(long, env = "OIDC_SCOPES", default_value = "openid profile email", global = true)]
    oidc_scopes: String,
    /// authentik group granting the admin role (everything incl. API keys, import, export to disk).
    #[arg(long, env = "OIDC_ADMIN_GROUP", default_value = "pdfsign-admins", global = true)]
    oidc_admin_group: String,
    /// authentik group granting the staff role (templates, documents, people).
    #[arg(long, env = "OIDC_STAFF_GROUP", default_value = "pdfsign-staff", global = true)]
    oidc_staff_group: String,
    /// Default folder for backups (mount a host folder here).
    #[arg(long, env = "BACKUP_DIR", global = true)]
    backup_dir: Option<PathBuf>,
    /// Built frontend (web/dist).
    #[arg(long, env = "WEB_DIR", default_value = "../web/dist", global = true)]
    web_dir: PathBuf,
    #[arg(long, env = "MAX_NAME_ATTEMPTS", default_value_t = 10, global = true)]
    max_name_attempts: i64,
    /// Disable the audit-trail page appended to generated PDFs.
    #[arg(long, env = "NO_AUDIT_PAGE", global = true)]
    no_audit_page: bool,
    /// Comma-separated proxy addresses / networks whose X-Forwarded-For header is trusted
    /// (client IP for the QR rate limit and the audit page). Empty: never trust it.
    #[arg(
        long,
        env = "TRUSTED_PROXIES",
        default_value = "127.0.0.0/8,::1/128,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,fc00::/7",
        global = true
    )]
    trusted_proxies: String,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the HTTP server (default).
    Serve {
        #[arg(long, env = "BIND", default_value = "0.0.0.0:8080")]
        bind: SocketAddr,
    },
    /// Export all completed documents as PDF files.
    Export {
        #[arg(long)]
        out: Option<PathBuf>,
        /// Only export the documents of this template (id or slug).
        #[arg(long)]
        template: Option<String>,
    },
    /// Create an API key for a bot / integration.
    CreateApiKey { name: String },
    /// Create a backup now (same as the button in the UI).
    ///
    /// This runs in its own process and does not pause a running server: the database copy
    /// is still consistent (VACUUM INTO), and the backup fails if a template PDF it
    /// references is missing from the copy. Retry in that case.
    Backup,
    /// Restore a backup archive into DATA_DIR. Stop the server first.
    Restore {
        archive: PathBuf,
        /// Overwrite an existing database.
        #[arg(long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info,sqlx=warn,tower_http=info".into()))
        .init();
    let cli = Cli::parse();
    if let Some(Cmd::Restore { archive, force }) = &cli.cmd {
        let previous = backup::restore(archive, &cli.data_dir, *force).await?;
        println!("restored {} into {}", archive.display(), cli.data_dir.display());
        if let Some(p) = previous {
            println!("the previous data was moved to {} - delete it once the restore is verified", p.display());
        }
        return Ok(());
    }
    let cfg = Config {
        backup_dir: cli.backup_dir.clone().unwrap_or_else(|| cli.data_dir.join("backups")),
        export_dir: cli.export_dir.clone().unwrap_or_else(|| cli.data_dir.join("export")),
        data_dir: cli.data_dir,
        public_url: cli.public_url,
        oidc: match (cli.oidc_issuer, cli.oidc_client_id, cli.oidc_client_secret) {
            (Some(issuer), Some(client_id), Some(client_secret)) if !issuer.is_empty() => Some(auth::OidcConfig {
                issuer,
                client_id,
                client_secret,
                scopes: cli.oidc_scopes,
                admin_group: cli.oidc_admin_group,
                staff_group: cli.oidc_staff_group,
                discovery: Default::default(),
            }),
            _ => None,
        },
        web_dir: cli.web_dir,
        max_name_attempts: cli.max_name_attempts,
        audit_page: !cli.no_audit_page,
        trusted_proxies: util::parse_nets(&cli.trusted_proxies).map_err(|e| anyhow::anyhow!("TRUSTED_PROXIES: {e}"))?,
    };
    let state = AppState::new(cfg).await?;

    match cli.cmd.unwrap_or(Cmd::Serve { bind: "0.0.0.0:8080".parse()? }) {
        Cmd::Serve { bind } => serve(state, bind).await,
        Cmd::Export { out, template } => {
            let dir = out.unwrap_or_else(|| state.cfg.export_dir.clone());
            let report = service::export_all(&state, &dir, template.as_deref()).await.map_err(|e| anyhow::anyhow!(e.message))?;
            println!("exported {} documents to {}", report.written, report.dir);
            for f in &report.failed {
                eprintln!("failed: {f}");
            }
            if !report.failed.is_empty() {
                anyhow::bail!("{} document(s) could not be exported", report.failed.len());
            }
            Ok(())
        }
        Cmd::Backup => {
            let s = backup::run_backup(&state, "cli").await.map_err(|e| anyhow::anyhow!(e.message))?;
            match s.last_ok {
                Some(true) => println!("backup written: {}", s.last_file.unwrap_or_default()),
                _ => anyhow::bail!("backup failed: {}", s.last_error.unwrap_or_default()),
            }
            Ok(())
        }
        Cmd::Restore { .. } => unreachable!(),
        Cmd::CreateApiKey { name } => {
            let (_, key) = auth::create_api_key(&state, &name, Some("cli")).await.map_err(|e| anyhow::anyhow!(e.message))?;
            println!("{key}");
            Ok(())
        }
    }
}

/// Release version (set by the container build), falling back to the crate version.
pub fn version() -> String {
    std::env::var("PDFSIGN_VERSION").ok().filter(|v| !v.is_empty() && v != "dev").unwrap_or_else(|| format!("{}-dev", env!("CARGO_PKG_VERSION")))
}

async fn serve(state: AppState, bind: SocketAddr) -> Result<()> {
    if state.cfg.oidc.is_none() {
        tracing::warn!("OIDC is not configured - nobody can log in to the admin UI");
    }
    let index = state.cfg.web_dir.join("index.html");
    // Static assets and the public signing pages (/s/<token>, /p/<token>) are served without
    // login; /admin is guarded.
    let spa = ServeDir::new(&state.cfg.web_dir).fallback(ServeFile::new(index));
    let app = Router::new()
        .route("/", get(|| async { Redirect::to("/admin") }))
        .route("/admin", get(auth::guard_admin_page))
        .route("/admin/{*rest}", get(auth::guard_admin_page))
        .route("/healthz", get(|| async { "ok" }))
        .nest("/auth", auth::router())
        .nest("/api/admin", routes::admin::router())
        .nest("/api/v1", routes::api::router())
        .nest("/api/public", routes::public::router())
        .fallback_service(spa)
        .layer(axum::middleware::from_fn_with_state(state.clone(), backup::write_gate))
        .layer(axum::middleware::from_fn_with_state(state.clone(), activity::record_errors))
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());
    backup::spawn_scheduler(state.clone());
    activity::info(&state.pool, "server", None, format!("pdfsign {} started", version())).await;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("pdfsign {} listening on http://{bind} (public url {})", version(), state.cfg.public_url);
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}
