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

#[tokio::test]
async fn admin_deletion_scopes_and_permissions() {
    let e = env().await;
    let st = &e.st;
    let t = template(st, 0).await;
    let other = template(st, 0).await;
    let p = create_person(&st.pool, "Signer", None, None).await.unwrap();
    let (first, _) = find_or_create_submission(&st.pool, &t, &p, true, None).await.unwrap();
    let (second, _) = find_or_create_submission(&st.pool, &t, &p, true, None).await.unwrap();
    let (third, _) = find_or_create_submission(&st.pool, &other, &p, true, None).await.unwrap();
    for sub in [&first, &second] {
        let h = session(st, sub).await;
        sign(st, sub, &t, &p, &h, &[("name", "Signer")]).await.unwrap();
    }
    for role in ["staff", "admin"] {
        sqlx::query("INSERT INTO users (id, sub, username, role, created_at, last_login_at) VALUES (?, ?, ?, ?, '', '')")
            .bind(role).bind(role).bind(role).bind(role).execute(&st.pool).await.unwrap();
        sqlx::query("INSERT INTO user_sessions VALUES (?, ?, '9999-01-01')").bind(sha256_hex(role)).bind(role).execute(&st.pool).await.unwrap();
    }
    sqlx::query("INSERT INTO public_links (id, template_id, token, created_at) VALUES ('qr', ?, 'link', '')")
        .bind(&t.id).execute(&st.pool).await.unwrap();
    let app = axum::Router::new().nest("/api/admin", crate::routes::admin::router())
        .layer(axum::middleware::from_fn_with_state(st.clone(), crate::backup::write_gate)).with_state(st.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/admin", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let delete = |path: &str, role: &str| client.delete(format!("{base}/{path}")).header("cookie", format!("pdfsign_session={role}"));
    let submission = format!("submissions/{}", first.id);
    let permanent = format!("templates/{}/permanent", t.id);
    let confirm = serde_json::json!({ "confirmation": "DELETE ALL DATA" });
    for path in [submission.as_str(), permanent.as_str(), "data"] {
        assert_eq!(delete(path, "staff").json(&confirm).send().await.unwrap().status(), 403);
    }
    assert_eq!(delete("data", "admin").json(&serde_json::json!({"confirmation": "no"})).send().await.unwrap().status(), 400);
    assert_eq!(delete(&submission, "admin").send().await.unwrap().status(), 200);
    assert!(load_values(&st.pool, &first.id).await.unwrap().is_empty());
    assert!(load_rounds(&st.pool, &first.id).await.unwrap().is_empty());
    assert!(submission_by_token(&st.pool, &first.token).await.is_err());
    assert!(get_submission(&st.pool, &second.id).await.is_ok());

    assert_eq!(delete(&format!("templates/{}", t.id), "staff").send().await.unwrap().status(), 200);
    assert!(get_template(&st.pool, &t.id).await.unwrap().archived);
    assert!(st.template_pdf_path(&t.id).exists());
    assert_eq!(delete(&permanent, "admin").send().await.unwrap().status(), 200);
    assert!(!st.template_pdf_path(&t.id).exists());
    assert!(get_submission(&st.pool, &second.id).await.is_err());
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM public_links").fetch_one(&st.pool).await.unwrap(), 0);
    assert!(get_submission(&st.pool, &third.id).await.is_ok());
    assert!(st.template_pdf_path(&other.id).exists());

    crate::auth::create_api_key(st, "bot", None).await.unwrap();
    sqlx::query("INSERT INTO settings VALUES ('test', 'data')").execute(&st.pool).await.unwrap();
    st.import.lock().unwrap().running = true;
    assert_eq!(delete("data", "admin").json(&confirm).send().await.unwrap().status(), 409);
    st.import.lock().unwrap().running = false;
    let orphan = st.template_pdf_path(&new_id());
    std::fs::write(&orphan, b"previous upload").unwrap();
    assert_eq!(delete("data", "admin").json(&confirm).send().await.unwrap().status(), 200);
    assert!(!orphan.exists());
    assert!(!st.template_pdf_path(&other.id).exists());
    assert_eq!(delete("data", "admin").json(&confirm).send().await.unwrap().status(), 401);
    for count in ["SELECT count(*) FROM users", "SELECT count(*) FROM templates", "SELECT count(*) FROM submissions",
                  "SELECT count(*) FROM persons", "SELECT count(*) FROM api_keys", "SELECT count(*) FROM settings",
                  "SELECT count(*) FROM activity_log", "SELECT count(*) FROM signer_sessions", "SELECT count(*) FROM deleted_template_pdfs"] {
        assert_eq!(sqlx::query_scalar::<_, i64>(count).fetch_one(&st.pool).await.unwrap(), 0, "{count}");
    }
    assert!(template(st, 0).await.id != other.id);
    server.abort();
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

#[tokio::test]
async fn people_pages_and_group_requirements_use_person_identity() {
    use reqwest::Method;
    use serde_json::{json, Value};
    let e = env().await;
    let st = &e.st;
    let t = template(st, 0).await;
    sqlx::query("UPDATE templates SET validity_days = 30 WHERE id = ?").bind(&t.id).execute(&st.pool).await.unwrap();
    // Identical names and update times exercise both pagination tie-breakers.
    sqlx::raw_sql("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<106)
        INSERT INTO persons (id, name, created_at) SELECT printf('p%03d',i), 'Alex Lee', '' FROM n;")
        .execute(&st.pool).await.unwrap();
    sqlx::query("INSERT INTO submissions (id, token, template_id, person_id, status, created_at, updated_at)
        SELECT id, id, ?, id, 'pending', '', '' FROM persons WHERE id != 'p001'")
        .bind(&t.id).execute(&st.pool).await.unwrap();
    sqlx::query("UPDATE submissions SET status = 'completed', completed_at = ? WHERE id = 'p002'")
        .bind(now()).execute(&st.pool).await.unwrap();
    sqlx::raw_sql("UPDATE submissions SET status = 'completed', completed_at = '2001-01-01T00:00:00Z' WHERE id = 'p003';
        UPDATE submissions SET status = 'revoked' WHERE id = 'p004';
        INSERT INTO users (id, sub, username, role, created_at, last_login_at) VALUES ('tester','tester','tester','staff','','');")
        .execute(&st.pool).await.unwrap();
    sqlx::query("INSERT INTO user_sessions VALUES (?, 'tester', '9999-01-01')").bind(sha256_hex("test-session")).execute(&st.pool).await.unwrap();
    let app = crate::routes::admin::router().with_state(st.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let send = |method: Method, path: String, body: Value| {
        let req = client.request(method, format!("{base}/{path}")).header("cookie", "pdfsign_session=test-session").json(&body);
        async move { req.send().await.unwrap().error_for_status().unwrap().json::<Value>().await.unwrap() }
    };
    for (path, key, expected) in [("persons", "name", 106), ("submissions", "updated_at", 105)] {
        let mut cursor = String::new();
        let mut seen = std::collections::HashSet::new();
        for expected_size in [50, 50, expected - 100] {
            let result = send(Method::GET, format!("{path}?limit=50{cursor}"), Value::Null).await;
            let batch = result.as_array().unwrap();
            assert_eq!(batch.len(), expected_size);
            for item in batch { assert!(seen.insert(item["id"].as_str().unwrap().to_owned())); }
            let last = batch.last().unwrap();
            cursor = format!("&after_key={}&after_id={}", last[key].as_str().unwrap(), last["id"].as_str().unwrap());
        }
    }
    let group = send(Method::POST, "groups".into(), json!({"name":"Crew"})).await;
    let group_id = group["id"].as_str().unwrap();
    for person in ["p001", "p002", "p003", "p004", "p005", "p005"] {
        send(Method::PUT, format!("groups/{group_id}/members/{person}"), Value::Null).await;
    }
    let report = format!("groups/{group_id}/requirements");
    let required = format!("{report}/{}", t.id);
    send(Method::PUT, required.clone(), Value::Null).await;
    let result = send(Method::GET, report.clone(), Value::Null).await;
    let missing: Vec<_> = result[0]["missing"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert_eq!(missing, ["p001", "p003", "p004", "p005"]); // absent, expired, revoked, pending
    let reasons: Vec<_> = result[0]["missing"].as_array().unwrap().iter().map(|p| p["status"].as_str().unwrap()).collect();
    assert_eq!(reasons, ["unissued", "expired", "revoked", "pending"]);
    let scoped = send(Method::GET, "submissions?person_id=p003&status=expired&limit=50".into(), Value::Null).await;
    assert_eq!(scoped.as_array().unwrap().len(), 1);
    assert_eq!(scoped[0]["person_id"], "p003");
    assert_eq!(scoped[0]["status"], "expired");
    send(Method::DELETE, format!("groups/{group_id}/members/p005"), Value::Null).await;
    merge_persons(&st.pool, "p001", "p006").await.unwrap();
    let result = send(Method::GET, report.clone(), Value::Null).await;
    let missing: Vec<_> = result[0]["missing"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert_eq!(missing, ["p003", "p004", "p006"]);
    send(Method::DELETE, required, Value::Null).await;
    assert_eq!(send(Method::GET, report, Value::Null).await, json!([]));
    server.abort();
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

#[tokio::test]
async fn duplicated_template_copies_layout_but_not_documents() {
    let e = env().await;
    let st = &e.st;
    let t = template(st, 2).await;
    let t = update_template(st, &t.id, serde_json::from_value(serde_json::json!({ "validity_days": 365, "description": "yearly" })).unwrap())
        .await
        .unwrap();
    let p = create_person(&st.pool, "Gina Wolf", None, None).await.unwrap();
    find_or_create_submission(&st.pool, &t, &p, false, None).await.unwrap();

    let copy = duplicate_template(st, &t.id, Some("Equipment 2027")).await.unwrap();
    assert_ne!(copy.id, t.id);
    assert_eq!(copy.name, "Equipment 2027");
    assert_eq!(copy.slug, "equipment-2027");
    assert_eq!(copy.fields.len(), t.fields.len());
    assert_eq!(copy.sections.len(), t.sections.len());
    assert_eq!(copy.validity_days, Some(365));
    assert_eq!(copy.description, "yearly");
    assert_eq!(copy.source, None);
    assert_eq!(copy.pdf_sha256, t.pdf_sha256);
    assert!(st.template_pdf_path(&copy.id).is_file());
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM submissions WHERE template_id = ?").bind(&copy.id).fetch_one(&st.pool).await.unwrap();
    assert_eq!(n, 0);
    // without a name the copy is called "<name> (copy)"
    assert_eq!(duplicate_template(st, &t.id, None).await.unwrap().name, format!("{} (copy)", t.name));
}

#[tokio::test]
async fn export_can_be_limited_to_one_template() {
    let e = env().await;
    let st = &e.st;
    let a = template(st, 0).await;
    let b = template(st, 0).await;
    for (t, name) in [(&a, "Hans Berg"), (&b, "Ida Busch"), (&b, "Jan Kurz")] {
        let p = create_person(&st.pool, name, None, None).await.unwrap();
        let (sub, _) = find_or_create_submission(&st.pool, t, &p, false, None).await.unwrap();
        let h = session(st, &sub).await;
        sign(st, &sub, t, &p, &h, &[("name", name)]).await.unwrap();
    }
    let all = export_all(st, &e.root.join("all"), None).await.unwrap();
    assert_eq!(all.written, 3);
    let one = export_all(st, &e.root.join("one"), Some(&b.slug)).await.unwrap();
    assert_eq!(one.written, 2);
    assert!(!e.root.join("one").join(&a.slug).exists());
    assert_eq!(std::fs::read_dir(e.root.join("one").join(&b.slug)).unwrap().count(), 2);
    assert!(export_all(st, &e.root.join("none"), Some("no-such-template")).await.is_err());
}

#[tokio::test]
async fn activity_log_filters_by_level_and_pages() {
    let e = env().await;
    let st = &e.st;
    crate::activity::info(&st.pool, "template", Some("alice"), "template 'A' saved").await;
    crate::activity::warn(&st.pool, "import", Some("alice"), "submission 7: image missing").await;
    crate::activity::error(&st.pool, "import", None, "submission 8: DocuSeal returned 500").await;
    let list = |v: serde_json::Value| {
        let f: crate::activity::Filter = serde_json::from_value(v).unwrap();
        let pool = st.pool.clone();
        async move { crate::activity::list(&pool, &f).await.unwrap() }
    };
    assert_eq!(list(serde_json::json!({})).await.len(), 3);
    assert_eq!(list(serde_json::json!({ "level": "warn" })).await.len(), 2);
    let errors = list(serde_json::json!({ "level": "error" })).await;
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("500"));
    assert_eq!(list(serde_json::json!({ "category": "template" })).await.len(), 1);
    assert_eq!(list(serde_json::json!({ "q": "image" })).await.len(), 1);
    // newest first, `before` continues with older entries
    let first = list(serde_json::json!({ "limit": 2 })).await;
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].level, "error");
    let rest = list(serde_json::json!({ "before": first[1].id })).await;
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].level, "info");
}

#[tokio::test]
async fn export_jobs_report_progress_and_hand_out_the_zip_once() {
    use crate::export_job::{self, Kind, StartRequest};
    let e = env().await;
    let st = &e.st;
    let t = template(st, 0).await;
    for name in ["Kai Lang", "Lea Roth"] {
        let p = create_person(&st.pool, name, None, None).await.unwrap();
        let (sub, _) = find_or_create_submission(&st.pool, &t, &p, false, None).await.unwrap();
        let h = session(st, &sub).await;
        sign(st, &sub, &t, &p, &h, &[("name", name)]).await.unwrap();
    }
    let wait = |id: String| async move {
        for _ in 0..200 {
            let j = export_job::get(st, &id).unwrap();
            if !j.running {
                return j;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("export did not finish");
    };

    let zip = export_job::start(st, StartRequest { kind: Kind::Zip, dir: None, template: Some(t.slug.clone()) }, "tester").await.unwrap();
    let done = wait(zip.id.clone()).await;
    assert_eq!((done.done, done.total), (2, 2));
    assert_eq!(done.phase, "finished");
    assert!(done.ready);
    assert_eq!(done.report.as_ref().unwrap().written, 2);
    let (name, bytes) = export_job::take_zip(st, &zip.id).unwrap();
    assert!(name.starts_with(&format!("signatures-{}-", t.slug)));
    assert!(zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap().len() >= 3);
    assert!(export_job::take_zip(st, &zip.id).is_err(), "a ZIP is handed out once");
    assert!(!export_job::get(st, &zip.id).unwrap().ready);

    let dir = e.root.join("disk");
    let disk = export_job::start(st, StartRequest { kind: Kind::Disk, dir: Some(dir.clone()), template: None }, "tester").await.unwrap();
    let done = wait(disk.id).await;
    assert_eq!(done.report.unwrap().written, 2);
    assert!(dir.join("index.csv").is_file());

    assert!(export_job::start(st, StartRequest { kind: Kind::Zip, dir: None, template: Some("missing".into()) }, "tester").await.is_err());
    let (logged,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM activity_log WHERE category = 'export'").fetch_one(&st.pool).await.unwrap();
    assert_eq!(logged, 2);
}
