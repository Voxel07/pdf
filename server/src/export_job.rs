//! Exports that run in the background so the admin UI can show a progress bar: a disk
//! export, or a ZIP that is built on the server and downloaded once it is ready.
//!
//! Jobs live in memory only; a ZIP is a temporary file. A finished job's ZIP is deleted
//! after its download or after [`KEEP`], whichever comes first.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::activity;
use crate::service::{self, ExportReport};
use crate::state::AppState;
use crate::util::*;

/// How long a finished job (and an undownloaded ZIP) is kept.
const KEEP: Duration = Duration::from_secs(30 * 60);
/// Exports render every document, so only a few may run at once.
const MAX_RUNNING: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Disk,
    Zip,
}

#[derive(Clone, Serialize)]
pub struct ExportJob {
    pub id: String,
    pub kind: Kind,
    pub running: bool,
    /// Documents rendered so far, and how many the export has.
    pub done: usize,
    pub total: usize,
    /// `rendering`, `packing` (ZIP), `finished` or `failed`.
    pub phase: String,
    pub template_name: Option<String>,
    pub started_by: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub report: Option<ExportReport>,
    pub error: Option<String>,
    /// A ZIP is ready to be downloaded.
    pub ready: bool,
    pub filename: Option<String>,
    #[serde(skip)]
    zip: Option<Arc<TempFile>>,
    #[serde(skip)]
    finished: Option<Instant>,
}

pub struct StartRequest {
    pub kind: Kind,
    pub dir: Option<PathBuf>,
    pub template: Option<String>,
}

fn prune(state: &AppState) {
    state.export_jobs.lock().unwrap().retain(|_, j| j.finished.is_none_or(|f| f.elapsed() < KEEP));
}

/// Starts an export and returns its job; poll [`get`] for progress.
pub async fn start(state: &AppState, req: StartRequest, actor: &str) -> AppResult<ExportJob> {
    prune(state);
    if state.export_jobs.lock().unwrap().values().filter(|j| j.running).count() >= MAX_RUNNING {
        return Err(AppError::conflict("export_busy", "two exports are already running - wait for one to finish"));
    }
    let template = match req.template.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => Some(service::get_template(&state.pool, t).await?),
        None => None,
    };
    let id = random_token(16);
    let job = ExportJob {
        id: id.clone(),
        kind: req.kind,
        running: true,
        done: 0,
        total: 0,
        phase: "rendering".into(),
        template_name: template.as_ref().map(|t| t.name.clone()),
        started_by: actor.to_string(),
        started_at: now(),
        finished_at: None,
        report: None,
        error: None,
        ready: false,
        filename: None,
        zip: None,
        finished: None,
    };
    state.export_jobs.lock().unwrap().insert(id.clone(), job.clone());

    let dir = req.dir.unwrap_or_else(|| state.cfg.export_dir.clone());
    let state = state.clone();
    let actor = actor.to_string();
    let kind = req.kind;
    tokio::spawn(async move {
        let work = tokio::spawn({
            let state = state.clone();
            let id = id.clone();
            let template = template.clone();
            let dir = dir.clone();
            async move {
                let jobs = state.export_jobs.clone();
                let progress = move |done: usize, total: usize| {
                    if let Some(j) = jobs.lock().unwrap().get_mut(&id) {
                        j.done = done;
                        j.total = total;
                        if kind == Kind::Zip && total > 0 && done == total {
                            j.phase = "packing".into();
                        }
                    }
                };
                let only = template.as_ref().map(|t| t.id.as_str());
                match kind {
                    Kind::Disk => service::export_all_with(&state, &dir, only, &progress).await.map(|r| (r, None)),
                    Kind::Zip => service::export_zip_with(&state, only, &progress).await.map(|(zip, r)| (r, Some(zip))),
                }
            }
        });
        let result = match work.await {
            Ok(r) => r.map_err(|e| e.message),
            Err(e) => Err(format!("the export stopped unexpectedly: {e}")),
        };
        let scope = template.as_ref().map(|t| format!("'{}'", t.name)).unwrap_or_else(|| "all templates".into());
        let what = match kind {
            Kind::Disk => format!("export of {scope} to {}", dir.display()),
            Kind::Zip => format!("ZIP download of {scope}"),
        };
        match &result {
            Ok((r, _)) if r.failed.is_empty() => {
                activity::info(&state.pool, "export", Some(actor.as_str()), format!("{what}: {} PDF(s) written", r.written)).await
            }
            Ok((r, _)) => {
                let msg = format!("{what}: {} PDF(s) written, {} failed: {}", r.written, r.failed.len(), r.failed.join("; "));
                activity::error(&state.pool, "export", Some(actor.as_str()), msg).await
            }
            Err(e) => activity::error(&state.pool, "export", Some(actor.as_str()), format!("{what} failed: {e}")).await,
        }
        let filename = match &template {
            Some(t) => format!("signatures-{}-{}.zip", t.slug, chrono::Utc::now().format("%Y-%m-%d")),
            None => format!("signatures-{}.zip", chrono::Utc::now().format("%Y-%m-%d")),
        };
        if let Some(j) = state.export_jobs.lock().unwrap().get_mut(&id) {
            j.running = false;
            j.finished_at = Some(now());
            j.finished = Some(Instant::now());
            match result {
                Ok((report, zip)) => {
                    j.phase = "finished".into();
                    j.done = j.total;
                    j.ready = zip.is_some();
                    j.filename = zip.as_ref().map(|_| filename);
                    j.zip = zip.map(Arc::new);
                    j.report = Some(report);
                }
                Err(e) => {
                    j.phase = "failed".into();
                    j.error = Some(e);
                }
            }
        }
    });
    Ok(job)
}

pub fn get(state: &AppState, id: &str) -> AppResult<ExportJob> {
    prune(state);
    state.export_jobs.lock().unwrap().get(id).cloned().ok_or_else(|| AppError::not_found("export not found (it may have expired)"))
}

/// Hands out a finished ZIP once; the job keeps its report but no longer holds the archive.
pub fn take_zip(state: &AppState, id: &str) -> AppResult<(String, Arc<TempFile>)> {
    let mut jobs = state.export_jobs.lock().unwrap();
    let j = jobs.get_mut(id).ok_or_else(|| AppError::not_found("export not found (it may have expired)"))?;
    let zip = j.zip.take().ok_or_else(|| AppError::not_found("this ZIP is not ready or was already downloaded"))?;
    j.ready = false;
    Ok((j.filename.clone().unwrap_or_else(|| "signatures.zip".into()), zip))
}
