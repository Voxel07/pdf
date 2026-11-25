//! In-process regression tests for signing, reuse-or-create, revocation, and restore,
//! using a disposable data directory.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::models::*;
use crate::service::*;
use crate::state::{AppState, Config};
use crate::util::*;

struct Env {
    st: AppState,
    root: PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn env() -> Env {
    let root = std::env::temp_dir().join(format!("pdfsign-test-{}", random_token(12)));
    let cfg = Config {
        data_dir: root.join("data"),
        export_dir: root.join("export"),
        backup_dir: root.join("backups"),
        public_url: "http://localhost".into(),
        oidc: None,
        web_dir: root.join("web"),
        max_name_attempts: 10,
        audit_page: true,
        trusted_proxies: vec![],
    };
    Env { st: AppState::new(cfg).await.unwrap(), root }
}

fn text_field(id: &str, section: u32, y: f64) -> Field {
    Field {
        id: id.into(), name: id.into(), kind: FieldKind::Text, required: true, section, page: 0,
        x: 0.1, y, w: 0.3, h: 0.04, options: vec![], font_size: None, format: None,
        autofill: None, min: None, max: None, placeholder: None, source_ref: None,
    }
}

/// A recipient section (required) plus `rows` optional rows, one text field each.
async fn template(st: &AppState, rows: u32) -> Template {
    let mut sections = vec![Section { id: 0, name: "Recipient".into(), required: true }];
    let mut fields = vec![text_field("name", 0, 0.05)];
    for r in 1..=rows {
        sections.push(Section { id: r, name: format!("Row {r}"), required: false });
        fields.push(text_field(&format!("item{r}"), r, 0.05 + r as f64 * 0.05));
    }
    create_template(st, "Equipment", &crate::pdf::tests::blank_pdf(), fields, sections, None).await.unwrap()
}

async fn session(st: &AppState, sub: &Submission) -> String {
    let hash = sha256_hex(random_token(32));
    sqlx::query("INSERT INTO signer_sessions (token_hash, submission_id, expires_at) VALUES (?, ?, ?)")
        .bind(&hash)
        .bind(&sub.id)
        .bind((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339())
        .execute(&st.pool)
        .await
        .unwrap();
    hash
}

async fn sign(st: &AppState, sub: &Submission, t: &Template, p: &Person, hash: &str, values: &[(&str, &str)]) -> AppResult<Submission> {
    let input: HashMap<String, String> = values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    submit_round(&st.pool, sub, t, p, &input, SubmitContext { signer_name: &p.name, ip: "", user_agent: "test", session_hash: Some(hash) }).await
}

#[tokio::test]
async fn competing_rounds_commit_once_and_independent_rows_both_commit() {
    let e = env().await;
    let st = &e.st;
    let t = template(st, 3).await;
    let p = create_person(&st.pool, "Anna Schmidt", None, Some("12345")).await.unwrap();
    let (sub, created) = find_or_create_submission(&st.pool, &t, &p, false, None).await.unwrap();
    assert!(created);
    let h = session(st, &sub).await;

    // required section missing -> rejected, nothing stored
    assert!(sign(st, &sub, &t, &p, &h, &[("item1", "x")]).await.is_err());
    let first = sign(st, &sub, &t, &p, &h, &[("name", "Anna"), ("item1", "Helmet")]).await.unwrap();
    assert_eq!(first.status, "completed");
    let completed_at = first.completed_at.clone();

    // two requests for the same row, both validated against the same snapshot
    let (a, b) = tokio::join!(
        sign(st, &first, &t, &p, &h, &[("item2", "Radio A")]),
        sign(st, &first, &t, &p, &h, &[("item2", "Radio B")]),
    );
    assert!(a.is_ok() != b.is_ok(), "exactly one competing round must commit");
    let loser = a.as_ref().err().or(b.as_ref().err()).unwrap();
    assert_eq!(loser.code, "already_signed");
    let rounds = load_rounds(&st.pool, &sub.id).await.unwrap();
    assert_eq!(rounds.iter().filter(|r| r.sections == "[2]").count(), 1);
    let item2 = load_values(&st.pool, &sub.id).await.unwrap().into_iter().find(|v| v.field_id == "item2").unwrap();
    let winner = if a.is_ok() { "Radio A" } else { "Radio B" };
    assert_eq!(item2.value, winner);
    assert_eq!(item2.round_id, rounds.iter().find(|r| r.sections == "[2]").unwrap().id);

    // a signed row stays locked
    assert!(sign(st, &first, &t, &p, &h, &[("item1", "changed")]).await.is_err());

    // a continuation keeps the original completion time
    let (c,) = (sign(st, &first, &t, &p, &h, &[("item3", "Lamp")]).await.unwrap(),);
    assert_eq!(c.completed_at, completed_at);
}

#[tokio::test]
async fn independent_optional_rows_can_be_signed_concurrently() {
    let e = env().await;
    let st = &e.st;
    let t = template(st, 2).await;
    let p = create_person(&st.pool, "Ben Meyer", None, None).await.unwrap();
    let (sub, _) = find_or_create_submission(&st.pool, &t, &p, false, None).await.unwrap();
    let h = session(st, &sub).await;
    let sub = sign(st, &sub, &t, &p, &h, &[("name", "Ben")]).await.unwrap();
    let (a, b) = tokio::join!(sign(st, &sub, &t, &p, &h, &[("item1", "A")]), sign(st, &sub, &t, &p, &h, &[("item2", "B")]));
    a.unwrap();
    b.unwrap();
    assert_eq!(load_rounds(&st.pool, &sub.id).await.unwrap().len(), 3);
}

#[tokio::test]
async fn revocation_wins_over_a_racing_first_round() {
    let e = env().await;
    let st = &e.st;
    let t = template(st, 0).await;
    let p = create_person(&st.pool, "Cem Yilmaz", None, None).await.unwrap();
    for _ in 0..20 {
        let (sub, _) = find_or_create_submission(&st.pool, &t, &p, true, None).await.unwrap();
        let h = session(st, &sub).await;
        let (signed, revoked) = tokio::join!(sign(st, &sub, &t, &p, &h, &[("name", "Cem")]), revoke_submission(&st.pool, &sub.id));
        revoked.unwrap();
        let now = get_submission(&st.pool, &sub.id).await.unwrap();
        assert_eq!(now.status, "revoked", "a round must not un-revoke a document");
        // with the session gone, nothing more is accepted
        let late = sign(st, &now, &t, &p, &h, &[("name", "Cem")]).await;
        assert!(late.is_err());
        let rounds = load_rounds(&st.pool, &sub.id).await.unwrap().len();
        assert_eq!(rounds, signed.is_ok() as usize);
    }
}

#[tokio::test]
async fn rotated_token_invalidates_running_sessions() {
    let e = env().await;
    let st = &e.st;
    let t = template(st, 0).await;
    let p = create_person(&st.pool, "Dana Kraus", None, None).await.unwrap();
    let (sub, _) = find_or_create_submission(&st.pool, &t, &p, false, None).await.unwrap();
    let h = session(st, &sub).await;
    rotate_submission_token(&st.pool, &sub.id).await.unwrap();
    let err = sign(st, &sub, &t, &p, &h, &[("name", "Dana")]).await.unwrap_err();
    assert_eq!(err.code, "session_expired");
    assert_eq!(get_submission(&st.pool, &sub.id).await.unwrap().status, "pending");
}

#[tokio::test]
async fn concurrent_reuse_or_create_yields_one_submission() {
    let e = env().await;
    let st = &e.st;
    let t = template(st, 0).await;
    let p = create_person(&st.pool, "Eva Lang", None, None).await.unwrap();
    let calls = (0..8).map(|_| find_or_create_submission(&st.pool, &t, &p, false, None));
    let results = futures_join_all(calls).await;
    let ids: std::collections::BTreeSet<String> = results.into_iter().map(|r| r.unwrap().0.id).collect();
    assert_eq!(ids.len(), 1);
    // force_new still creates a separate document
    let (forced, created) = find_or_create_submission(&st.pool, &t, &p, true, None).await.unwrap();
    assert!(created && !ids.contains(&forced.id));
}

async fn futures_join_all<F: std::future::Future>(fs: impl Iterator<Item = F>) -> Vec<F::Output> {
    let mut set = Vec::new();
    for f in fs {
        set.push(f);
    }
    let mut out = Vec::new();
    // poll all futures concurrently on one task
    let mut pinned: Vec<_> = set.into_iter().map(Box::pin).collect();
    let mut done: Vec<Option<F::Output>> = (0..pinned.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (i, f) in pinned.iter_mut().enumerate() {
            if done[i].is_none() {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => done[i] = Some(v),
                    std::task::Poll::Pending => pending = true,
                }
            }
        }
        if pending { std::task::Poll::Pending } else { std::task::Poll::Ready(()) }
    })
    .await;
    out.extend(done.into_iter().map(Option::unwrap));
    out
}

#[tokio::test]
async fn restore_is_validated_and_keeps_current_data_on_failure() {
    let e = env().await;
    let st = &e.st;
    let t = template(st, 1).await;
    let p = create_person(&st.pool, "Finn Vogel", None, None).await.unwrap();
    let (sub, _) = find_or_create_submission(&st.pool, &t, &p, false, None).await.unwrap();
    let h = session(st, &sub).await;
    sign(st, &sub, &t, &p, &h, &[("name", "Finn")]).await.unwrap();

    let status = crate::backup::run_backup(st, "test").await.unwrap();
    assert_eq!(status.last_ok, Some(true), "{:?}", status.last_error);
    let archive = st.cfg.backup_dir.join(status.last_file.unwrap());

    // restore into a fresh directory: template PDF and the signer session come back
    let fresh = e.root.join("fresh");
    assert_eq!(crate::backup::restore(&archive, &fresh, false).await.unwrap(), None);
    assert!(fresh.join("templates").join(format!("{}.pdf", t.id)).is_file());
    let restored = sqlx::SqlitePool::connect(&format!("sqlite://{}", fresh.join("pdfsign.db").display())).await.unwrap();
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM signer_sessions WHERE token_hash = ?").bind(&h).fetch_one(&restored).await.unwrap();
    assert_eq!(n, 1);
    restored.close().await;

    // restoring over existing data moves it aside
    let previous = crate::backup::restore(&archive, &fresh, true).await.unwrap().expect("previous data kept");
    assert!(previous.join("pdfsign.db").is_file());

    // a broken archive (garbage database, missing PDF) leaves the current data untouched
    let before = std::fs::read(fresh.join("pdfsign.db")).unwrap();
    let bad = e.root.join("bad.zip");
    {
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&bad).unwrap());
        zw.start_file("pdfsign.db", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut zw, b"not a database").unwrap();
        zw.finish().unwrap();
    }
    assert!(crate::backup::restore(&bad, &fresh, true).await.is_err());
    let no_pdf = e.root.join("no-pdf.zip");
    {
        let mut src = zip::ZipArchive::new(std::fs::File::open(&archive).unwrap()).unwrap();
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&no_pdf).unwrap());
        let mut db = Vec::new();
        std::io::Read::read_to_end(&mut src.by_name("pdfsign.db").unwrap(), &mut db).unwrap();
        zw.start_file("pdfsign.db", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut zw, &db).unwrap();
        zw.finish().unwrap();
    }
    let err = crate::backup::restore(&no_pdf, &fresh, true).await.unwrap_err();
    assert!(format!("{err:#}").contains("template PDF"), "{err:#}");
    assert_eq!(std::fs::read(fresh.join("pdfsign.db")).unwrap(), before);
    assert!(fresh.join("templates").join(format!("{}.pdf", t.id)).is_file());
    assert!(!std::fs::read_dir(&fresh).unwrap().any(|d| d.unwrap().file_name().to_string_lossy().starts_with(".restore-")));
}

#[tokio::test]
async fn template_is_validated_as_a_whole() {
    let e = env().await;
    let st = &e.st;
    let t = template(st, 1).await;
    // dropping a section that still has fields is rejected even without sending fields
    let up = |v: serde_json::Value| serde_json::from_value::<TemplateUpdate>(v).unwrap();
    let r = update_template(st, &t.id, up(serde_json::json!({ "sections": [{ "id": 0, "name": "Recipient", "required": true }] }))).await;
    assert!(r.is_err());
    // geometry outside the page
    let mut f = t.fields.clone();
    f[1].y = 0.99;
    assert!(update_template(st, &t.id, up(serde_json::json!({ "fields": f }))).await.is_err());
    // a required section with only autofill fields can never be completed
    let mut f = t.fields.clone();
    f[0].autofill = Some("person_name".into());
    assert!(update_template(st, &t.id, up(serde_json::json!({ "fields": f }))).await.is_err());
    // signature fields cannot be autofilled
    let mut f = t.fields.clone();
    f[1].kind = FieldKind::Signature;
    f[1].autofill = Some("today".into());
    assert!(update_template(st, &t.id, up(serde_json::json!({ "fields": f }))).await.is_err());
    // renaming a valid template still works
    assert_eq!(update_template(st, &t.id, up(serde_json::json!({ "name": "Kit" }))).await.unwrap().name, "Kit");
}
