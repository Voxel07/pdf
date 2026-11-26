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
    env_with_events(false).await
}

async fn env_with_events(events_enabled: bool) -> Env {
    env_with_calendar(events_enabled, None, None).await
}

async fn env_with_calendar(events_enabled: bool, url: Option<reqwest::Url>, token: Option<String>) -> Env {
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
        events_enabled,
        calendar_url: url,
        calendar_pdfsign_token: token,
        trusted_proxies: vec![],
    };
    Env { st: AppState::new(cfg).await.unwrap(), root }
}

#[tokio::test]
async fn databases_from_earlier_versions_are_refused_with_a_clear_error() {
    let e = env().await;
    // reopening a current database works
    AppState::new(clone_config(&e.st.cfg)).await.unwrap();
    // a 0.0.3 persons table has no name_key column
    let data_dir = e.root.join("old-data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let opts = sqlx::sqlite::SqliteConnectOptions::new().filename(data_dir.join("pdfsign.db")).create_if_missing(true);
    let pool = sqlx::SqlitePool::connect_with(opts).await.unwrap();
    sqlx::raw_sql("CREATE TABLE persons (id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let err = AppState::new(Config { data_dir, ..clone_config(&e.st.cfg) }).await.err().unwrap();
    assert!(format!("{err:#}").contains("pdfsign 0.0.3 or earlier"), "{err:#}");
}

fn clone_config(c: &Config) -> Config {
    Config {
        data_dir: c.data_dir.clone(),
        export_dir: c.export_dir.clone(),
        backup_dir: c.backup_dir.clone(),
        public_url: c.public_url.clone(),
        oidc: None,
        web_dir: c.web_dir.clone(),
        max_name_attempts: c.max_name_attempts,
        audit_page: c.audit_page,
        events_enabled: c.events_enabled,
        calendar_url: c.calendar_url.clone(),
        calendar_pdfsign_token: c.calendar_pdfsign_token.clone(),
        trusted_proxies: c.trusted_proxies.clone(),
    }
}

async fn http_app(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = crate::app_router(state);
    let server = tokio::spawn(async move {
        axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap()
    });
    (base, server)
}

async fn login_session(state: &AppState, role: &str) -> String {
    sqlx::query("INSERT INTO users (id, sub, username, role, created_at, last_login_at) VALUES (?, ?, ?, ?, '', '')")
        .bind(role).bind(role).bind(role).bind(role).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO user_sessions (token_hash, user_id, expires_at) VALUES (?, ?, '9999-01-01')")
        .bind(sha256_hex(role)).bind(role).execute(&state.pool).await.unwrap();
    format!("pdfsign_session={role}")
}

#[tokio::test]
async fn public_signing_survives_disabled_and_unavailable_events() {
    for enabled in [false, true] {
        let e = env_with_events(enabled).await;
        let st = &e.st;
        let (base, server) = http_app(st.clone()).await;
        let client = reqwest::Client::new();
        let health = client.get(format!("{base}/api/events/health")).send().await.unwrap();
        assert_eq!(health.status().as_u16(), if enabled { 503 } else { 404 });
        if enabled {
            let staff = login_session(st, "staff").await;
            let res = client.get(format!("{base}/api/admin/events")).header("cookie", staff).send().await.unwrap();
            assert_eq!(res.status(), 503);
        } else {
            assert_eq!(client.get(format!("{base}/api/admin/events")).send().await.unwrap().status(), 404);
        }
        let t = template(st, 0).await;
        let person = create_person(&st.pool, "Signer", None, None).await.unwrap();
        let (sub, _) = find_or_create_submission(&st.pool, &t, &person, true, None).await.unwrap();
        let url = format!("{base}/api/public/s/{}", sub.token);
        let response: serde_json::Value = client.post(format!("{url}/unlock"))
            .json(&serde_json::json!({"name": "Signer"})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        let session = response["session"].as_str().unwrap();
        let response: serde_json::Value = client.post(format!("{url}/submit")).header("x-session", session)
            .json(&serde_json::json!({"values": {"name": "Signer"}})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(response["status"], "completed");
        let pdf = client.get(format!("{url}/signed.pdf")).header("x-session", session)
            .send().await.unwrap().error_for_status().unwrap().bytes().await.unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        server.abort();
    }
}

#[tokio::test]
async fn signing_attempts_lock_atomically_and_qr_respects_the_lock() {
    let e = env().await;
    let t = template(&e.st, 1).await;
    let person = create_person(&e.st.pool, "Test Signer", None, Some("12345")).await.unwrap();
    let (sub, _) = find_or_create_submission(&e.st.pool, &t, &person, false, None).await.unwrap();
    sqlx::query("INSERT INTO public_links (id, template_id, token, created_at) VALUES ('test-link', ?, 'test-qr-token', ?)")
        .bind(&t.id).bind(now()).execute(&e.st.pool).await.unwrap();
    let (base, server) = http_app(e.st.clone()).await;
    let client = reqwest::Client::new();
    let unlock = format!("{base}/api/public/s/{}/unlock", sub.token);
    let answers = futures_join_all((0..16).map(|_| async {
        client.post(&unlock).json(&serde_json::json!({"name": "Different name"})).send().await.unwrap().status().as_u16()
    })).await;
    assert_eq!(answers.iter().filter(|&&status| status == 403).count(), 10, "{answers:?}");
    assert_eq!(answers.iter().filter(|&&status| status == 423).count(), 6, "{answers:?}");
    assert_eq!(get_submission(&e.st.pool, &sub.id).await.unwrap().failed_attempts, 10);
    assert_eq!(client.post(format!("{base}/api/public/p/test-qr-token/identify"))
        .json(&serde_json::json!({"name": person.name, "zip": "12345"})).send().await.unwrap().status(), 423);
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM signer_sessions").fetch_one(&e.st.pool).await.unwrap(), 0);
    server.abort();
}

#[tokio::test]
async fn administrators_can_revoke_all_sessions_for_a_user() {
    let e = env().await;
    let admin = login_session(&e.st, "admin").await;
    let staff = login_session(&e.st, "staff").await;
    let (base, server) = http_app(e.st.clone()).await;
    let client = reqwest::Client::new();
    let sessions = format!("{base}/api/admin/users/staff/sessions");
    assert_eq!(client.delete(&sessions).header("cookie", &staff).send().await.unwrap().status(), 403);
    let response: serde_json::Value = client.delete(sessions).header("cookie", &admin)
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(response["revoked"], 1);
    assert_eq!(client.get(format!("{base}/api/admin/me")).header("cookie", &staff).send().await.unwrap().status(), 401);
    assert_eq!(client.get(format!("{base}/api/admin/me")).header("cookie", &admin).send().await.unwrap().status(), 200);
    server.abort();
}

#[tokio::test]
#[ignore = "requires a disposable calendar service; see calendar/README.md"]
async fn event_access_and_calendar_share_the_calendar_database() {
    use serde_json::{json, Value};

    let calendar_origin = std::env::var("PDFSIGN_TEST_CALENDAR_URL").unwrap();
    let calendar_token = std::env::var("PDFSIGN_TEST_CALENDAR_TOKEN").unwrap();
    let calendar_admin_token = std::env::var("PDFSIGN_TEST_CALENDAR_ADMIN_TOKEN").unwrap();
    let e = env_with_calendar(true, Some(calendar_origin.parse().unwrap()), Some(calendar_token)).await;
    let staff = login_session(&e.st, "staff").await;
    let admin = login_session(&e.st, "admin").await;
    let attendee = login_session(&e.st, "attendee").await;
    let (_, signing_key) = crate::auth::create_api_key(&e.st, "signing", None).await.unwrap();
    let (base, server) = http_app(e.st.clone()).await;
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let staff_events = format!("{base}/api/admin/events");
    let api_events = format!("{base}/api/events/v1/events");
    let credentials = format!("{staff_events}/credentials");
    assert_eq!(client.get(format!("{base}/api/events/health")).send().await.unwrap().status(), 204);
    let res = client.get(format!("{base}/admin/events/new")).send().await.unwrap();
    assert_eq!(res.status(), 303);
    assert_eq!(res.headers()["location"], "/auth/login?next=%2Fadmin%2Fevents%2Fnew");
    assert_eq!(client.get(&staff_events).send().await.unwrap().status(), 401);
    assert_eq!(client.get(&api_events).bearer_auth(&signing_key).send().await.unwrap().status(), 401);
    assert_eq!(client.get(&api_events).header("cookie", &staff).send().await.unwrap().status(), 401);
    assert_eq!(client.get(format!("{base}/api/admin/templates")).header("cookie", &attendee).send().await.unwrap().status(), 403);
    assert_eq!(client.get(&credentials).header("cookie", &staff).send().await.unwrap().status(), 403);

    let issue = |name: &str, scope: &str| {
        client.post(&credentials).header("cookie", &admin).json(&json!({"name": name, "scope": scope})).send()
    };
    let read: Value = issue("Calendar bridge", "read").await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let read = read["secret"].as_str().unwrap();
    let write: Value = issue("Event creator", "write").await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let write = write["secret"].as_str().unwrap();
    let calendar: Value = issue("Office calendar", "calendar").await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let calendar_path = reqwest::Url::parse(calendar["secret"].as_str().unwrap()).unwrap();
    let calendar_url = format!("{base}{}", calendar_path.path());

    let event = json!({
        "title": "Club, meetup",
        "description": format!("First line\nSecond; {}", "é".repeat(50)),
        "location": "HQ",
        "starts_at": "2090-04-22T18:00:00+02:00",
        "ends_at": "2090-04-22T19:00:00+02:00"
    });
    assert_eq!(client.post(&staff_events).header("cookie", &attendee).json(&event).send().await.unwrap().status(), 403);
    assert_eq!(client.post(&api_events).bearer_auth(read).json(&event).send().await.unwrap().status(), 403);
    let create = |input: &Value| client.post(&api_events).bearer_auth(write).header("idempotency-key", "club-meetup").json(input).send();
    let created = create(&event).await.unwrap();
    assert_eq!(created.status(), 201);
    let created: Value = created.json().await.unwrap();
    let duplicate = create(&event).await.unwrap();
    assert_eq!(duplicate.status(), 200);
    assert_eq!(duplicate.json::<Value>().await.unwrap()["id"], created["id"]);
    let mut changed = event.clone();
    changed["title"] = json!("Other title");
    assert_eq!(create(&changed).await.unwrap().status(), 409);
    assert_eq!(created["starts_at"], "2090-04-22T16:00:00Z");
    assert_eq!(created["phase"], "upcoming");
    let id = created["id"].as_str().unwrap();
    let standalone = format!("{calendar_origin}/api/v1/events");
    let direct: Value = client.get(format!("{standalone}/{id}")).bearer_auth(&calendar_admin_token)
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(direct["title"], event["title"]);
    let all_day: Value = client.post(&standalone).bearer_auth(&calendar_admin_token).json(&json!({
        "title": "Calendar UI event", "all_day": true, "starts_at": "2090-05-01", "ends_at": "2090-05-02"
    })).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let all_day_id = all_day["id"].as_str().unwrap();
    let detail: Value = client.get(format!("{staff_events}/{all_day_id}")).header("cookie", &staff)
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(detail["all_day"], true);
    assert_eq!(detail["starts_at"], "2090-05-01");
    let updated = client.patch(format!("{standalone}/{id}")).bearer_auth(&calendar_admin_token)
        .header("if-match", "\"0\"").json(&json!({"location": "New HQ"}))
        .send().await.unwrap();
    assert_eq!(updated.status(), 200);
    let rsvp_url = format!("{staff_events}/{id}/rsvp");
    let respond = |response: &str| client.put(&rsvp_url).header("cookie", &attendee).json(&json!({"response": response})).send();
    assert_eq!(respond("maybe").await.unwrap().status(), 200);
    let going: Value = respond("going").await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(going["my_rsvp"], "going");
    let view: Value = client.get(format!("{api_events}/{id}")).bearer_auth(read).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(view["going"], 1);
    assert_eq!(view["maybe"], 0);
    assert!(view.get("my_rsvp").is_none());
    assert_eq!(client.get(&staff_events).bearer_auth(read).send().await.unwrap().status(), 401);

    let mut cancelled = event.clone();
    cancelled["revision"] = json!(1);
    cancelled["status"] = json!("cancelled");
    let update = || client.put(format!("{api_events}/{id}")).bearer_auth(write).json(&cancelled).send();
    assert_eq!(update().await.unwrap().status(), 200);
    assert_eq!(update().await.unwrap().status(), 409);
    assert_eq!(respond("declined").await.unwrap().status(), 409);
    let direct: Value = client.get(format!("{standalone}/{id}")).bearer_auth(&calendar_admin_token)
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(direct["status"], "cancelled");
    assert_eq!(direct["revision"], 2);

    let feed = client.get(&calendar_url).send().await.unwrap().error_for_status().unwrap();
    assert_eq!(feed.headers()["content-type"], "text/calendar; charset=utf-8");
    assert_eq!(feed.headers()["cache-control"], "no-store");
    let feed = feed.text().await.unwrap();
    assert!(feed.contains("SUMMARY:Club\\, meetup\r\n"));
    assert!(feed.contains("DESCRIPTION:First line\\nSecond\\; "));
    assert!(feed.contains("SEQUENCE:2\r\n"));
    assert!(feed.contains("STATUS:CANCELLED\r\n"));
    assert!(feed.contains("DTSTART;VALUE=DATE:20900501\r\n"));
    assert!(feed.split("\r\n").all(|line| line.len() <= 75));
    let revoked = client.delete(format!("{credentials}/{}", calendar["id"].as_str().unwrap()))
        .header("cookie", &admin).send().await.unwrap();
    assert_eq!(revoked.status(), 204);
    assert_eq!(client.get(calendar_url).send().await.unwrap().status(), 404);
    assert_eq!(client.delete(format!("{standalone}/{all_day_id}")).bearer_auth(&calendar_admin_token)
        .send().await.unwrap().status(), 204);
    assert_eq!(client.get(format!("{staff_events}/{all_day_id}")).header("cookie", &staff)
        .send().await.unwrap().status(), 404);
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sqlite_schema WHERE name IN ('events', 'event_rsvps', 'event_credentials')")
        .fetch_one(&e.st.pool).await.unwrap(), 0);

    let snapshot = format!("{calendar_origin}/internal/pdfsign/backup");
    assert_eq!(client.get(snapshot).bearer_auth(&calendar_admin_token).send().await.unwrap().status(), 404);
    let backup = crate::backup::run_backup(&e.st, "calendar integration test").await.unwrap();
    assert_eq!(backup.last_ok, Some(true), "{:?}", backup.last_error);
    let archive = e.st.cfg.backup_dir.join(backup.last_file.unwrap());
    let signing_dir = e.root.join("restored-signing");
    let calendar_dir = e.root.join("restored-calendar");
    assert!(crate::backup::restore(&archive, &signing_dir, None, false, false).await.is_err());
    let restore = crate::backup::restore(&archive, &signing_dir, Some(&calendar_dir), false, false).await.unwrap();
    assert!(restore.calendar_restored);
    let db = sqlx::SqlitePool::connect(&format!("sqlite://{}", calendar_dir.join("calendar.sqlite").display())).await.unwrap();
    assert_eq!(sqlx::query_scalar::<_, String>("SELECT status FROM events WHERE id = ?").bind(id).fetch_one(&db).await.unwrap(), "cancelled");
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_rsvps WHERE event_id = ?").bind(id).fetch_one(&db).await.unwrap(), 1);
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_credentials").fetch_one(&db).await.unwrap(), 0);
    db.close().await;
    server.abort();
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
        sqlx::query("INSERT INTO user_sessions (token_hash, user_id, expires_at) VALUES (?, ?, '9999-01-01')").bind(sha256_hex(role)).bind(role).execute(&st.pool).await.unwrap();
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
        INSERT INTO persons (id, name, name_key, created_at) SELECT printf('p%03d',i), 'Alex Lee', 'alex lee', '' FROM n;")
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
    sqlx::query("INSERT INTO user_sessions (token_hash, user_id, expires_at) VALUES (?, 'tester', '9999-01-01')").bind(sha256_hex("test-session")).execute(&st.pool).await.unwrap();
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
    submit_round(st, sub, t, p, &input, SubmitContext { signer_name: &p.name, ip: "", user_agent: "test", session_hash: Some(hash) }).await
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
    login_session(st, "admin").await;
    crate::auth::create_api_key(st, "backup test", None).await.unwrap();
    sqlx::query("INSERT INTO public_links (id, template_id, token, created_at) VALUES ('qr', ?, 'old-qr', ?)")
        .bind(&t.id).bind(now()).execute(&st.pool).await.unwrap();

    let status = crate::backup::run_backup(st, "test").await.unwrap();
    assert_eq!(status.last_ok, Some(true), "{:?}", status.last_error);
    let archive = st.cfg.backup_dir.join(status.last_file.unwrap());

    // Restoring keeps the signed record and templates and retires access granted before the restore.
    let fresh = e.root.join("fresh");
    assert!(crate::backup::restore(&archive, &fresh, None, false, false).await.unwrap().signing_previous.is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&archive).unwrap().permissions().mode() & 0o077, 0);
    }
    assert!(fresh.join("templates").join(format!("{}.pdf", t.id)).is_file());
    let restored = sqlx::SqlitePool::connect(&format!("sqlite://{}", fresh.join("pdfsign.db").display())).await.unwrap();
    for query in ["SELECT count(*) FROM signer_sessions", "SELECT count(*) FROM user_sessions", "SELECT count(*) FROM api_keys"] {
        assert_eq!(sqlx::query_scalar::<_, i64>(query).fetch_one(&restored).await.unwrap(), 0);
    }
    let restored_sub: Submission = get_submission(&restored, &sub.id).await.unwrap();
    assert_eq!(restored_sub.status, "completed");
    assert_ne!(restored_sub.token, sub.token);
    let qr: (String, bool) = sqlx::query_as("SELECT token, active FROM public_links WHERE id = 'qr'").fetch_one(&restored).await.unwrap();
    assert_ne!(qr.0, "old-qr");
    assert!(!qr.1);
    restored.close().await;

    // restoring over existing data moves it aside
    let previous = crate::backup::restore(&archive, &fresh, None, false, true).await.unwrap().signing_previous.expect("previous data kept");
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
    assert!(crate::backup::restore(&bad, &fresh, None, false, true).await.is_err());
    let no_pdf = e.root.join("no-pdf.zip");
    {
        let mut src = zip::ZipArchive::new(std::fs::File::open(&archive).unwrap()).unwrap();
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&no_pdf).unwrap());
        let mut db = Vec::new();
        std::io::Read::read_to_end(&mut src.by_name("pdfsign.db").unwrap(), &mut db).unwrap();
        zw.start_file("pdfsign.db", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut zw, &db).unwrap();
        zw.raw_copy_file(src.by_name("manifest.json").unwrap()).unwrap();
        zw.finish().unwrap();
    }
    let err = crate::backup::restore(&no_pdf, &fresh, None, false, true).await.err().unwrap();
    assert!(format!("{err:#}").contains("template PDF"), "{err:#}");
    // archives from earlier versions (format 1 or no manifest) are refused
    let old = e.root.join("old.zip");
    {
        let mut src = zip::ZipArchive::new(std::fs::File::open(&archive).unwrap()).unwrap();
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&old).unwrap());
        for i in 0..src.len() {
            let entry = src.by_index(i).unwrap();
            if entry.name().unwrap() != "manifest.json" {
                zw.raw_copy_file(entry).unwrap();
            }
        }
        zw.start_file("manifest.json", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut zw, br#"{"app": "pdfsign", "format": 1}"#).unwrap();
        zw.finish().unwrap();
    }
    let err = crate::backup::restore(&old, &fresh, None, false, true).await.err().unwrap();
    assert!(format!("{err:#}").contains("earlier pdfsign version"), "{err:#}");
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
    let (name, archive) = export_job::take_zip(st, &zip.id).unwrap();
    assert!(name.starts_with(&format!("signatures-{}-", t.slug)));
    assert!(zip::ZipArchive::new(std::fs::File::open(&archive.0).unwrap()).unwrap().len() >= 3);
    let path = archive.0.clone();
    drop((archive, done));
    assert!(!path.exists(), "the archive is deleted with its last handle");
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

#[tokio::test]
async fn general_links_need_the_zip_and_never_reveal_the_personal_token() {
    let e = env().await;
    let st = &e.st;
    let t = template(st, 0).await;
    let alice = create_person(&st.pool, "Alice Example", None, Some("12345")).await.unwrap();
    // a pending personal link, as the bot would DM it
    let (personal, _) = find_or_create_submission(&st.pool, &t, &alice, false, None).await.unwrap();
    create_person(&st.pool, "Bob Nozip", None, None).await.unwrap();
    sqlx::query("INSERT INTO public_links (id, template_id, token, created_at) VALUES ('qr', ?, 'qr-link', ?)")
        .bind(&t.id).bind(now()).execute(&st.pool).await.unwrap();
    let (base, server) = http_app(st.clone()).await;
    let client = reqwest::Client::new();
    let identify = |body: serde_json::Value| client.post(format!("{base}/api/public/p/qr-link/identify")).json(&body).send();

    // without a ZIP code, registered and unknown names get the same answer
    for name in ["Alice Example", "Nobody Known"] {
        let r: serde_json::Value = identify(serde_json::json!({"name": name})).await.unwrap().json().await.unwrap();
        assert_eq!(r, serde_json::json!({"result": "need_zip", "reason": "verify"}));
    }
    // the right name with a wrong ZIP code does not reach Alice's document
    let r: serde_json::Value = identify(serde_json::json!({"name": "Alice Example", "zip": "99999"})).await.unwrap().json().await.unwrap();
    assert_eq!(r["result"], "sign");
    let reached: String = sqlx::query_scalar("SELECT submission_id FROM signer_sessions WHERE handle = ?")
        .bind(r["handle"].as_str().unwrap()).fetch_one(&st.pool).await.unwrap();
    assert_ne!(reached, personal.id);
    // a namesake without a ZIP code on file cannot be confirmed
    assert_eq!(identify(serde_json::json!({"name": "Bob Nozip", "zip": "12345"})).await.unwrap().status(), 409);

    // name + ZIP: a session on a handle, never the personal token
    let r: serde_json::Value = identify(serde_json::json!({"name": "alice  example", "zip": "12 345"})).await.unwrap().json().await.unwrap();
    assert_eq!(r["result"], "sign");
    assert!(r.get("token").is_none());
    let handle = r["handle"].as_str().unwrap();
    assert!(handle.starts_with("qr-") && !r.to_string().contains(&personal.token));
    let session = r["session"].as_str().unwrap();
    let page = format!("{base}/api/public/s/{handle}");
    assert_eq!(client.get(&page).send().await.unwrap().status(), 200);
    // the handle is not a link: it cannot be unlocked by name
    assert_eq!(client.post(format!("{page}/unlock")).json(&serde_json::json!({"name": "Alice Example"})).send().await.unwrap().status(), 410);
    let form: serde_json::Value = client.post(format!("{page}/submit")).header("x-session", session)
        .json(&serde_json::json!({"values": {"name": "Alice Example"}})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(form["status"], "completed");
    assert_eq!(get_submission(&st.pool, &personal.id).await.unwrap().status, "completed", "the open document was used");
    // once the session ends, so does the handle
    sqlx::query("UPDATE signer_sessions SET expires_at = '2000-01-01'").execute(&st.pool).await.unwrap();
    assert_eq!(client.get(&page).send().await.unwrap().status(), 410);
    server.abort();
}

#[tokio::test]
async fn discord_registration_cannot_rename_a_person() {
    let e = env().await;
    let p = upsert_discord_person(&e.st.pool, "123456789", Some("Carla Example"), Some("carla"), Some("11111")).await.unwrap();
    assert_eq!(p.zip.as_deref(), Some("11111"));
    // the same name in another spelling is fine, the ZIP code on file stays
    let same = upsert_discord_person(&e.st.pool, "123456789", Some("carla  EXAMPLE"), None, Some("22222")).await.unwrap();
    assert_eq!((same.name.as_str(), same.zip.as_deref()), ("Carla Example", Some("11111")));
    let err = upsert_discord_person(&e.st.pool, "123456789", Some("Someone Else"), None, None).await.unwrap_err();
    assert_eq!(err.code, "already_registered");
    assert_eq!(get_person(&e.st.pool, &p.id).await.unwrap().name, "Carla Example");
    // a person renamed by staff (name and key together) is found under the new name only
    sqlx::query("UPDATE persons SET name = 'Carla Renamed', name_key = ? WHERE id = ?")
        .bind(crate::util::normalize_name("Carla Renamed")).bind(&p.id).execute(&e.st.pool).await.unwrap();
    assert_eq!(persons_by_name(&e.st.pool, "Carla Renamed").await.unwrap().len(), 1);
    assert!(persons_by_name(&e.st.pool, "Carla Example").await.unwrap().is_empty());
}

#[tokio::test]
async fn browsers_get_hardened_responses_and_cross_site_writes_are_refused() {
    let e = env().await;
    let st = &e.st;
    std::fs::create_dir_all(&st.cfg.web_dir).unwrap();
    std::fs::write(st.cfg.web_dir.join("index.html"), "<!doctype html>").unwrap();
    let staff = login_session(st, "staff").await;
    let (base, server) = http_app(st.clone()).await;
    let client = reqwest::Client::new();
    let page = client.get(format!("{base}/s/whatever")).send().await.unwrap();
    assert!(page.headers()["content-security-policy"].to_str().unwrap().contains("frame-ancestors"));
    assert_eq!(page.headers()["referrer-policy"], "no-referrer");
    assert_eq!(page.headers()["x-content-type-options"], "nosniff");

    let group = |site: Option<&str>, origin: Option<&str>| {
        let mut r = client.post(format!("{base}/api/admin/groups")).header("cookie", &staff).json(&serde_json::json!({"name": random_token(8)}));
        if let Some(site) = site { r = r.header("sec-fetch-site", site); }
        if let Some(origin) = origin { r = r.header("origin", origin); }
        r.send()
    };
    assert_eq!(group(Some("same-site"), None).await.unwrap().status(), 403);
    assert_eq!(group(None, Some("https://calendar.example.com")).await.unwrap().status(), 403);
    assert_eq!(group(Some("same-origin"), None).await.unwrap().status(), 200);
    assert_eq!(group(None, Some("http://localhost")).await.unwrap().status(), 200);
    assert_eq!(group(None, None).await.unwrap().status(), 200);
    server.abort();
}

#[tokio::test]
async fn internal_errors_are_not_shown_to_clients() {
    use axum::response::IntoResponse;
    let res = AppError::from(anyhow::anyhow!("/data/secret/path: permission denied")).into_response();
    assert_eq!(res.status(), 500);
    let detail = res.extensions().get::<crate::activity::FailedRequest>().map(|f| f.0.clone()).unwrap();
    assert!(detail.contains("/data/secret/path"), "the activity log keeps the detail");
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("secret"));
}

#[tokio::test]
async fn output_folders_cannot_be_published_or_live_data() {
    let e = env().await;
    let st = &e.st;
    for bad in [st.cfg.web_dir.join("exports"), st.cfg.data_dir.clone(), st.cfg.data_dir.join("templates"), PathBuf::from("relative")] {
        assert!(check_output_dir(st, &bad).is_err(), "{}", bad.display());
    }
    assert!(check_output_dir(st, &st.cfg.export_dir).is_ok());
    assert!(check_output_dir(st, &st.cfg.data_dir.join("backups")).is_ok());
    // the defaults for a relative DATA_DIR (`./data/export`, `./data/backups`) are usable
    for relative in ["./data/export", "./data/backups"] {
        let dir = crate::output_dir(PathBuf::from(relative)).unwrap();
        assert!(dir.is_absolute(), "{}", dir.display());
        assert!(check_output_dir(st, &dir).is_ok(), "{}", dir.display());
    }
}

#[test]
fn ipv6_clients_share_a_rate_limit_per_64() {
    use crate::state::rate_key;
    assert_eq!(rate_key("2001:db8:1:2:aaaa::1"), rate_key("2001:db8:1:2:bbbb::2"));
    assert_ne!(rate_key("2001:db8:1:2::1"), rate_key("2001:db8:1:3::1"));
    assert_eq!(rate_key("192.0.2.1"), "192.0.2.1");
}
