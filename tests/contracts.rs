use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use harness_server::{
    api,
    document::{self, Documents},
    engine,
    store::{State, Store, import_directory},
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "test-only-device-token-0000000000000000000000";
fn project(id: &str) -> String {
    document::render(&json!({"schema_version":1,"memory_type":"project","project_id":id,"title":id,"aliases":[format!("old-{id}")],"roots":["/not-a-server-path"],"status":"ACTIVE","verified_at":"unknown","updated_at":"2026-01-01T00:00:00Z"}).as_object().unwrap().clone(),&format!("# {id}\n")).unwrap()
}
fn topic(id: &str, summary: &str) -> String {
    document::render(&json!({"schema_version":1,"memory_type":"project_topic","project_id":"demo","topic_id":id,"title":"主题","summary":summary,"triggers":[format!("exact-{id}")],"aliases":[],"status":"ACTIVE","verified_at":"unknown","updated_at":"2026-01-01T00:00:00Z"}).as_object().unwrap().clone(),"# 主题\n\nsecret_body_marker").unwrap()
}
async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("Content-Type", "application/json");
    if let Some(key) = key {
        req = req.header("Idempotency-Key", key);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn app() -> (TempDir, Router) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let app = api::router(store, vec![TOKEN.into()]).unwrap();
    (dir, app)
}
async fn seed(app: &Router) {
    let (status, _) = request(
        app,
        "PUT",
        "/v1/documents",
        json!({"path":"memory/project/demo.md","content":project("demo")}),
        Some("seed"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
fn start_payload(id: &str) -> Value {
    json!({"project":"demo","agent":"codex","native_session_id":id,"title":"任务","workstream_id":"demo-shared","content":true})
}
fn finish_payload(id: &str, sha: &str) -> Value {
    json!({"session_id":format!("codex-{id}"),"expected_workstream_sha256":sha,"result":{"status":"IDLE","current":"已完成","verification":["相关检查通过"]}})
}

#[tokio::test]
async fn all_data_endpoints_are_authenticated_and_liveness_is_minimal() {
    let (_dir, app) = app();
    let (status, health) = request(&app, "GET", "/v1/health", Value::Null, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["build"], harness_server::build_info::metadata());
    for path in [
        "/v1/health",
        "/v1/stats",
        "/v1/documents?path=memory/project/demo.md",
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = String::from_utf8(
            to_bytes(response.into_body(), 10000)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(!body.contains(TOKEN));
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 10000).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        json!({"status":"ok"})
    );
}
#[tokio::test]
async fn cas_identity_and_credential_guards_prevent_mutation() {
    let (_dir, app) = app();
    seed(&app).await;
    let payload = json!({"path":"memory/project/demo.md","content":project("demo"),"expected_sha256":"wrong"});
    assert_eq!(
        request(&app, "PUT", "/v1/documents", payload, Some("cas"))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (hash_status, doc) = request(
        &app,
        "GET",
        "/v1/documents?path=memory/project/demo.md",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(hash_status, StatusCode::OK);
    let mut m = document::parse(doc["content"].as_str().unwrap()).unwrap().0;
    m.insert("verified_at".into(), json!("2026-01-01"));
    assert_eq!(request(&app,"PUT","/v1/documents",json!({"path":"memory/project/demo.md","content":document::render(&m,"# demo").unwrap(),"expected_sha256":doc["sha256"]}),Some("change-verified")).await.0,StatusCode::BAD_REQUEST);
    for path in [
        "../outside.md",
        "/etc/passwd",
        "memory/project/../outside.md",
        "memory/project/README.md",
    ] {
        assert_eq!(
            request(
                &app,
                "PUT",
                "/v1/documents",
                json!({"path":path,"content":project("demo")}),
                Some("bad-path")
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let secret = format!("{}{}", "sk-", "a".repeat(25));
    let c = project("safe") + &secret;
    assert_eq!(
        request(
            &app,
            "PUT",
            "/v1/documents",
            json!({"path":"memory/project/safe.md","content":c}),
            Some("secret")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
async fn tracked_roundtrip_and_competing_workstream_update_conflict() {
    let (_dir, app) = app();
    seed(&app).await;
    let (status, a) = request(
        &app,
        "POST",
        "/v1/start",
        start_payload("a"),
        Some("start-a"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{a}");
    let (_, b) = request(
        &app,
        "POST",
        "/v1/start",
        start_payload("b"),
        Some("start-b"),
    )
    .await;
    assert_eq!(a["workstream_sha256"], b["workstream_sha256"]);
    let (_, done) = request(
        &app,
        "POST",
        "/v1/finish",
        finish_payload("a", a["workstream_sha256"].as_str().unwrap()),
        Some("finish-a"),
    )
    .await;
    assert_eq!(done["memory_extraction"]["status"], "no_change");
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/finish",
            finish_payload("b", b["workstream_sha256"].as_str().unwrap()),
            Some("finish-b")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, session) = request(
        &app,
        "GET",
        "/v1/documents?path=handoffs/sessions/active/codex-a.md",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(
        document::string(
            &document::parse(session["content"].as_str().unwrap())
                .unwrap()
                .0,
            "status"
        ),
        "IDLE"
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/start",
            json!({"project":"other", "agent":"codex","native_session_id":"a","title":"任务"}),
            Some("cross-project")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
#[tokio::test]
async fn idempotency_is_persistent_and_rejects_different_payloads() {
    let (dir, app) = app();
    let p = json!({"path":"memory/project/demo.md","content":project("demo")});
    let (status, first) = request(&app, "PUT", "/v1/documents", p.clone(), Some("same")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["idempotent"], false);
    drop(app);
    let app = api::router(Store::open(dir.path()).unwrap(), vec![TOKEN.into()]).unwrap();
    let (_, retry) = request(&app, "PUT", "/v1/documents", p, Some("same")).await;
    assert_eq!(retry["idempotent"], true);
    assert_eq!(
        request(
            &app,
            "PUT",
            "/v1/documents",
            json!({"path":"memory/project/other.md","content":project("other")}),
            Some("same")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, stats) = request(&app, "GET", "/v1/stats", Value::Null, None).await;
    assert_eq!(stats["metrics"]["retries"], 1);
}
#[tokio::test]
async fn invalid_finish_rolls_back_session_topic_and_receipt_together() {
    let (dir, app) = app();
    seed(&app).await;
    let (_, start) = request(&app, "POST", "/v1/start", start_payload("a"), Some("start")).await;
    let head = std::fs::read(dir.path().join("HEAD")).unwrap();
    let mut finish = finish_payload("a", start["workstream_sha256"].as_str().unwrap());
    finish["result"]["project_topics"] = json!([{"topic_id":"test","title":"测试","summary":"测试","triggers":["测试"],"body":"# 测试"}]);
    finish["result"]["process_id"] = json!("overflow");
    finish["result"]["process_learnings"] = json!(["x".repeat(241)]);
    assert_eq!(
        request(&app, "POST", "/v1/finish", finish, Some("bad-finish"))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(std::fs::read(dir.path().join("HEAD")).unwrap(), head);
    assert_eq!(
        request(
            &app,
            "GET",
            "/v1/documents?path=memory/project/demo/test.md",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/finish",
            finish_payload("a", start["workstream_sha256"].as_str().unwrap()),
            Some("bad-finish")
        )
        .await
        .0,
        StatusCode::OK
    );
}
#[tokio::test]
async fn daily_does_not_write_receipts_revisions_or_metrics() {
    let (dir, app) = app();
    for (command, v) in [
        ("route", json!({"project":"daily","scenario":"coding"})),
        ("start", json!({"project":"daily"})),
        (
            "finish",
            json!({"mode":"log","project":"daily","result":{"current":"无"}}),
        ),
    ] {
        let (status, result) =
            request(&app, "POST", &format!("/v1/{command}"), v, Some("daily")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["mode"], "SKIP");
    }
    assert!(!dir.path().join("HEAD").exists());
    let (_, stats) = request(&app, "GET", "/v1/stats", Value::Null, None).await;
    assert_eq!(stats["metrics"]["observed_calls"], 0);
}
#[tokio::test]
async fn missing_idempotency_and_unsupported_fields_do_not_write() {
    let (dir, app) = app();
    assert_eq!(
        request(&app, "POST", "/v1/start", start_payload("a"), None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut p = start_payload("a");
    p["project_root"] = json!("/users/mac/path");
    assert_eq!(
        request(&app, "POST", "/v1/start", p, Some("unknown-field"))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert!(!dir.path().join("HEAD").exists());
}
#[tokio::test]
async fn provisional_abort_preserves_other_sessions_and_confirmed_workstreams() {
    let (_dir, app) = app();
    let (_, a) = request(&app, "POST", "/v1/start", start_payload("a"), Some("a")).await;
    let (_, b) = request(&app, "POST", "/v1/start", start_payload("b"), Some("b")).await;
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/abort",
            json!({"session_id":"codex-a","expected_sha256":a["handoff"]["session"]["sha256"]}),
            Some("abort")
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/finish",
            finish_payload("b", b["workstream_sha256"].as_str().unwrap()),
            Some("finish-b")
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, session) = request(
        &app,
        "GET",
        "/v1/documents?path=handoffs/sessions/active/codex-b.md",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/abort",
            json!({"session_id":"codex-b","expected_sha256":session["sha256"]}),
            Some("abort-confirmed")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
#[tokio::test]
async fn concurrent_cas_writers_only_commit_once() {
    let (_dir, app) = app();
    seed(&app).await;
    let (_, doc) = request(
        &app,
        "GET",
        "/v1/documents?path=memory/project/demo.md",
        Value::Null,
        None,
    )
    .await;
    let c = doc["content"].as_str().unwrap();
    let payload = json!({"path":"memory/project/demo.md","content":format!("{c}\n- changed\n"),"expected_sha256":doc["sha256"]});
    let (a, b) = tokio::join!(
        request(
            &app,
            "PUT",
            "/v1/documents",
            payload.clone(),
            Some("writer-a")
        ),
        request(&app, "PUT", "/v1/documents", payload, Some("writer-b"))
    );
    let mut statuses = [a.0.as_u16(), b.0.as_u16()];
    statuses.sort();
    assert_eq!(statuses, [200, 409]);
}
#[test]
fn summary_ranking_is_bounded_and_never_selects_weak_bodies() {
    let mut docs = Documents::new();
    docs.insert("memory/project/demo.md".into(), project("demo"));
    for id in ["a", "b", "c", "d", "z-upgrade"] {
        docs.insert(
            format!("memory/project/demo/{id}.md"),
            topic(
                id,
                if id == "z-upgrade" {
                    "服务器版本升级与恢复"
                } else {
                    "普通摘要"
                },
            ),
        );
    }
    let r = engine::route(
        &docs,
        &json!({"project":"old-demo","scenario":"服务器升级失败了怎么恢复","content":true}),
    )
    .unwrap();
    assert_eq!(r["project_topics"], json!([]));
    assert_eq!(r["topic_candidates"][0]["topic_id"], "z-upgrade");
    assert_eq!(r["topic_candidates"].as_array().unwrap().len(), 3);
    assert_eq!(r["content"].as_array().unwrap().len(), 1);
    assert_eq!(r["candidate_remaining"], 2);
    let r = engine::route(
        &docs,
        &json!({"project":"demo","scenario":"exact-z-upgrade"}),
    )
    .unwrap();
    assert_eq!(r["route_outcome"], "auto");
    let r = engine::route(
        &docs,
        &json!({"project":"demo","scenario":"unrelated","candidate_offset":3}),
    )
    .unwrap();
    assert_eq!(r["topic_candidates"].as_array().unwrap().len(), 2);
    let r = engine::route(
        &docs,
        &json!({"project":"demo","scenario":"不要服务器升级，只看普通摘要"}),
    )
    .unwrap();
    assert_ne!(r["topic_candidates"][0]["topic_id"], "z-upgrade");
}
#[test]
fn import_preserves_markdown_and_ignores_runtime_and_indexes() {
    let source = TempDir::new().unwrap();
    let dir = TempDir::new().unwrap();
    let p = source.path().join("memory/project");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("demo.md"), project("demo")).unwrap();
    std::fs::write(p.join("README.md"), "derived").unwrap();
    let logs = source.path().join("memory/logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("2026-01.md"), "# 历史日志").unwrap();
    let docs = import_directory(source.path()).unwrap();
    assert_eq!(docs.len(), 2);
    let mut store = Store::open(dir.path()).unwrap();
    store.import(docs).unwrap();
    assert_eq!(
        store.state.documents["memory/project/demo.md"],
        project("demo")
    );
    assert!(store.import(Documents::new()).is_err());
}
#[test]
fn single_writer_and_unpublished_revisions_are_safe() {
    let dir = TempDir::new().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    assert!(Store::open(dir.path()).is_err());
    s.transact("initial", &json!({"operation":"seed"}), |state| {
        state
            .documents
            .insert("memory/project/demo.md".into(), project("demo"));
        Ok(json!({"written":true}))
    })
    .unwrap();
    let revision = s.revision.clone();
    let abandoned = dir
        .path()
        .join("revisions")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir(&abandoned).unwrap();
    std::fs::write(abandoned.join("partial.md"), "incomplete").unwrap();
    drop(s);
    let s = Store::open(dir.path()).unwrap();
    assert_eq!(s.revision, revision);
    assert_eq!(s.state.documents.len(), 1);
}
#[test]
fn filesystem_failure_does_not_publish_candidate_state() {
    let dir = TempDir::new().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    std::fs::rename(
        dir.path().join("revisions"),
        dir.path().join("saved-revisions"),
    )
    .unwrap();
    std::fs::write(dir.path().join("revisions"), "not a directory").unwrap();
    assert!(
        s.transact("failure", &json!({}), |state| {
            state
                .documents
                .insert("memory/project/demo.md".into(), project("demo"));
            Ok(json!({}))
        })
        .is_err()
    );
    assert!(s.state.documents.is_empty());
    assert!(s.state.receipts.is_empty());
    assert!(!dir.path().join("HEAD").exists());
}
#[cfg(unix)]
#[test]
fn store_and_import_reject_symlinks() {
    use std::os::unix::fs::symlink;
    let d = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    symlink(outside.path(), d.path().join("linked")).unwrap();
    assert!(Store::open(d.path().join("linked")).is_err());
    std::fs::create_dir_all(d.path().join("memory/project")).unwrap();
    symlink(outside.path(), d.path().join("memory/project/link")).unwrap();
    assert!(import_directory(d.path()).is_err());
}
#[test]
fn topic_overflow_and_cross_project_references_are_rejected() {
    let mut state = State::default();
    let mut p = start_payload("a");
    engine::start(&mut state, &p).unwrap();
    p["project"] = json!("other");
    assert!(engine::start(&mut state, &p).is_err());
    let c = topic("big", "摘要") + &"x".repeat(6001);
    assert!(document::validate("memory/project/demo/big.md", &c).is_err());
    assert!(
        document::validate_all(&Documents::from([(
            "memory/project/demo/orphan.md".into(),
            topic("orphan", "摘要")
        )]))
        .is_err()
    );
}

#[tokio::test]
async fn listing_is_bounded_project_scoped_and_does_not_return_bodies() {
    let (_dir, app) = app();
    seed(&app).await;
    request(
        &app,
        "POST",
        "/v1/start",
        start_payload("a"),
        Some("list-start"),
    )
    .await;
    let (status, list) = request(
        &app,
        "GET",
        "/v1/documents?kind=session&project=old-demo&limit=1",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["total"], 1);
    assert_eq!(list["documents"][0]["session_id"], "codex-a");
    assert!(list["documents"][0].get("content").is_none());
    assert_eq!(
        request(&app, "GET", "/v1/documents?limit=51", Value::Null, None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (_, list) = request(
        &app,
        "GET",
        "/v1/documents?project=other",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(list["total"], 0);
}
#[tokio::test]
async fn body_limit_and_unknown_character_observations_are_explicit() {
    let (_dir, app) = app();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/route")
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({"project":"demo","scenario":"x".repeat(1024*1024)}).to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let (_, stats) = request(&app, "GET", "/v1/stats", Value::Null, None).await;
    assert!(stats["metrics"]["returned_characters"].is_null());
    assert_eq!(stats["metrics"]["returned_character_observed_calls"], 0);
}
#[test]
fn finish_preserves_existing_long_term_goal() {
    let mut s = State::default();
    let started = engine::start(&mut s, &start_payload("a")).unwrap();
    let path = "handoffs/workstreams/demo-shared.md";
    let (m, _) = document::parse(&s.documents[path]).unwrap();
    let content = document::render(
        &m,
        "# 任务\n\n- 目标：保持跨阶段目标，不随本轮标题替换。\n- 当前：等待执行。",
    )
    .unwrap();
    s.documents.insert(path.into(), content.clone());
    engine::finish(&mut s, &finish_payload("a", &document::sha(&content))).unwrap();
    assert!(s.documents[path].contains("- 目标：保持跨阶段目标，不随本轮标题替换。"));
    assert_eq!(started["session_id"], "codex-a");
}
