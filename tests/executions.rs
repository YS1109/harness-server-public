use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use harness_server::{api, document, execution, store::Store};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "execution-test-only-token-00000000000000000000";
fn app() -> (TempDir, Router) {
    let d = TempDir::new().unwrap();
    let a = api::router(Store::open(d.path()).unwrap(), vec![TOKEN.into()]).unwrap();
    (d, a)
}
async fn req(
    a: &Router,
    method: &str,
    p: &str,
    v: Value,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut r = Request::builder()
        .method(method)
        .uri(p)
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("Content-Type", "application/json");
    if let Some(key) = key {
        r = r.header("Idempotency-Key", key);
    }
    let res = a
        .clone()
        .oneshot(r.body(Body::from(v.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let b = to_bytes(res.into_body(), 2 * 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&b)
            .unwrap_or_else(|_| json!({"rejection":String::from_utf8_lossy(&b)})),
    )
}
async fn ok(a: &Router, method: &str, p: &str, v: Value, key: Option<&str>) -> Value {
    let (s, b) = req(a, method, p, v, key).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    b
}
fn payload(id: &str) -> Value {
    json!({"task_id":id,"conversation_id":"conversation-a","source_key":format!("qq-msg-{id}"),"project":"demo","title":"实现接口","goal":"修复接口并保存可续接的进度","acceptance":"验证受影响接口"})
}
async fn create(a: &Router, id: &str) -> Value {
    ok(
        a,
        "POST",
        "/v1/executions",
        payload(id),
        Some(&format!("create-{id}")),
    )
    .await
}
fn endpoint(t: &Value, suffix: &str) -> String {
    format!(
        "/v1/executions/{}{}",
        t["task"]["task_id"].as_str().unwrap(),
        suffix
    )
}
async fn begin(a: &Router, t: &Value, key: &str) -> Value {
    ok(
        a,
        "POST",
        &endpoint(t, "/attempts"),
        json!({"expected_sha256":t["sha256"],"agent":"pi"}),
        Some(key),
    )
    .await
}
fn checkpoint(t: &Value, status: &str) -> Value {
    json!({"expected_sha256":t["sha256"],"attempt_id":t["task"]["current_attempt_id"],"status":status,"native_session_id":"pi-session-1","checkpoint":{"current":"接口已定位，准备验证","completed_steps":["已检查接口"],"next_steps":["运行相关验证"],"artifacts":["branch:fix-api"]}})
}
async fn update(a: &Router, t: &Value, status: &str, key: &str) -> Value {
    ok(
        a,
        "POST",
        &endpoint(t, "/checkpoint"),
        checkpoint(t, status),
        Some(key),
    )
    .await
}

#[tokio::test]
async fn restart_restores_multiple_tasks_attempt_checkpoints_context_and_receipts() {
    let (d, a) = app();
    let t = create(&a, "one").await;
    create(&a, "two").await;
    let t = begin(&a, &t, "attempt-one").await;
    let cp = checkpoint(&t, "RUNNING");
    let t = ok(
        &a,
        "POST",
        &endpoint(&t, "/checkpoint"),
        cp.clone(),
        Some("checkpoint-one"),
    )
    .await;
    let c=ok(&a,"PUT","/v1/execution-contexts/conversation-a",json!({"title":"QQ任务调度","goal":"完成两项任务","constraints":["按影响验证"],"open_questions":["确认发布窗口"],"task_ids":["one","two"]}),Some("context-one")).await;
    drop(a);
    let a = api::router(Store::open(d.path()).unwrap(), vec![TOKEN.into()]).unwrap();
    let got = ok(&a, "GET", "/v1/executions/one", Value::Null, None).await;
    assert_eq!(got["task"]["status"], "RUNNING");
    assert_eq!(
        got["latest_attempt"]["checkpoint"]["artifacts"][0],
        "branch:fix-api"
    );
    assert_eq!(got["sha256"], t["sha256"]);
    let listed = ok(
        &a,
        "GET",
        "/v1/executions?conversation_id=conversation-a&active_only=true&limit=1",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(listed["total"], 2);
    assert_eq!(listed["remaining"], 1);
    assert!(listed["tasks"][0].get("checkpoint").is_none());
    let c2 = ok(
        &a,
        "GET",
        "/v1/execution-contexts/conversation-a",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(c["context"], c2["context"]);
    assert_eq!(c["sha256"], c2["sha256"]);
    let replay = ok(
        &a,
        "POST",
        "/v1/executions/one/checkpoint",
        cp,
        Some("checkpoint-one"),
    )
    .await;
    assert_eq!(replay["idempotent"], true);
    assert_eq!(replay["sha256"], t["sha256"]);
}
#[tokio::test]
async fn source_dedup_is_independent_of_http_key_and_rejects_changed_intent() {
    let (_d, a) = app();
    let t = create(&a, "one").await;
    let dup = ok(
        &a,
        "POST",
        "/v1/executions",
        payload("one"),
        Some("new-http-key"),
    )
    .await;
    assert_eq!(dup["created"], false);
    assert_eq!(dup["deduplicated"], true);
    assert_eq!(dup["task"]["task_id"], t["task"]["task_id"]);
    for field in ["goal", "conversation_id", "project", "task_id"] {
        let mut p = payload("one");
        p[field] = json!("changed");
        assert_eq!(
            req(&a, "POST", "/v1/executions", p, Some(field)).await.0,
            StatusCode::CONFLICT
        );
    }
    assert_eq!(
        ok(&a, "GET", "/v1/executions", Value::Null, None).await["total"],
        1
    );
    let p = json!({"conversation_id":"conversation-a","source_key":"qq-generated","project":"demo","title":"自动ID","goal":"一个目标"});
    let generated = ok(
        &a,
        "POST",
        "/v1/executions",
        p.clone(),
        Some("generated-one"),
    )
    .await;
    let dup = ok(&a, "POST", "/v1/executions", p, Some("generated-two")).await;
    assert_eq!(dup["task"]["task_id"], generated["task"]["task_id"]);
}
#[tokio::test]
async fn concurrent_claims_create_one_attempt_and_independent_tasks_can_progress() {
    let (_d, a) = app();
    let t = create(&a, "one").await;
    let other = create(&a, "two").await;
    let v = json!({"expected_sha256":t["sha256"],"agent":"pi"});
    let p = endpoint(&t, "/attempts");
    let (r1, r2) = tokio::join!(
        req(&a, "POST", &p, v.clone(), Some("claim-a")),
        req(&a, "POST", &p, v, Some("claim-b"))
    );
    let mut statuses = [r1.0, r2.0];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
    let t = if r1.0 == StatusCode::OK { r1.1 } else { r2.1 };
    assert_eq!(t["task"]["attempt_count"], 1);
    let other = begin(&a, &other, "claim-other").await;
    let (r1, r2) = tokio::join!(
        update(&a, &t, "RUNNING", "run-one"),
        update(&a, &other, "RUNNING", "run-two")
    );
    assert_eq!(r1["task"]["status"], "RUNNING");
    assert_eq!(r2["task"]["status"], "RUNNING");
}
#[tokio::test]
async fn uncertain_dispatch_is_not_retried_and_stale_attempts_cannot_write_after_retry() {
    let (_d, a) = app();
    let t = create(&a, "one").await;
    let t = begin(&a, &t, "first-attempt").await;
    assert_eq!(
        req(
            &a,
            "POST",
            &endpoint(&t, "/checkpoint"),
            checkpoint(&t, "SUCCEEDED"),
            Some("premature-success")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let t = update(&a, &t, "RECONCILING", "lost-ack").await;
    assert_eq!(
        req(
            &a,
            "POST",
            &endpoint(&t, "/attempts"),
            json!({"expected_sha256":t["sha256"],"agent":"pi"}),
            Some("blind-retry")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let t = update(&a, &t, "FAILED", "verified-failure").await;
    let old = t.clone();
    let t = begin(&a, &t, "second-attempt").await;
    assert_eq!(t["task"]["attempt_count"], 2);
    let mut p = checkpoint(&old, "RUNNING");
    p["expected_sha256"] = t["sha256"].clone();
    assert_eq!(
        req(
            &a,
            "POST",
            &endpoint(&t, "/checkpoint"),
            p,
            Some("old-worker")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let t = update(&a, &t, "RUNNING", "second-running").await;
    let mut p = checkpoint(&t, "RUNNING");
    p["native_session_id"] = json!("different-session");
    assert_eq!(
        req(
            &a,
            "POST",
            &endpoint(&t, "/checkpoint"),
            p,
            Some("replace-native")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let old_id = old["task"]["current_attempt_id"].as_str().unwrap();
    let previous = ok(
        &a,
        "GET",
        &format!("/v1/executions/one/attempts/{old_id}"),
        Value::Null,
        None,
    )
    .await;
    assert_eq!(previous["attempt"]["status"], "FAILED");
    let attempts = ok(
        &a,
        "GET",
        "/v1/executions/one/attempts?limit=1",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(attempts["attempts"][0]["number"], 2);
    assert_eq!(attempts["total"], 2);
    assert!(attempts["attempts"][0].get("checkpoint").is_none());
}
#[tokio::test]
async fn cancellation_requests_do_not_claim_a_running_process_has_stopped() {
    let (_d, a) = app();
    let queued = create(&a, "queued").await;
    let cancelled = ok(
        &a,
        "POST",
        &endpoint(&queued, "/cancel"),
        json!({"expected_sha256":queued["sha256"]}),
        Some("cancel-queued"),
    )
    .await;
    assert_eq!(cancelled["task"]["status"], "CANCELLED");
    assert_eq!(cancelled["task"]["attempt_count"], 0);
    let t = create(&a, "one").await;
    let t = begin(&a, &t, "begin-one").await;
    let t = update(&a, &t, "RUNNING", "running-one").await;
    let t = ok(
        &a,
        "POST",
        &endpoint(&t, "/cancel"),
        json!({"expected_sha256":t["sha256"]}),
        Some("request-cancel"),
    )
    .await;
    assert_eq!(t["task"]["status"], "RUNNING");
    assert_eq!(t["task"]["cancel_requested"], true);
    let t = update(&a, &t, "CANCELLED", "confirmed-stop").await;
    assert_eq!(t["task"]["delivery_status"], "PENDING");
}
async fn delivered(a: &Router, t: &Value, status: &str, key: &str, summary: &str) -> Value {
    ok(a,"POST",&endpoint(t,"/delivery"),json!({"expected_sha256":t["sha256"],"delivery_key":t["task"]["delivery_key"],"status":status,"summary":summary}),Some(key)).await
}
#[tokio::test]
async fn results_and_delivery_have_separate_state_and_reply_keys_change_on_retry() {
    let (_d, a) = app();
    let t = create(&a, "one").await;
    let t = begin(&a, &t, "attempt-one").await;
    let t = update(&a, &t, "FAILED", "failed-one").await;
    let first_key = t["task"]["delivery_key"].clone();
    let t = delivered(&a, &t, "SENDING", "failure-sending", "").await;
    let t = delivered(&a, &t, "SENT", "failure-sent", "").await;
    let t = begin(&a, &t, "attempt-two").await;
    assert_ne!(first_key, t["task"]["delivery_key"]);
    let t = update(&a, &t, "RUNNING", "running-two").await;
    let t = update(&a, &t, "SUCCEEDED", "success-two").await;
    assert_eq!(t["task"]["delivery_status"], "PENDING");
    let bad = json!({"expected_sha256":t["sha256"],"delivery_key":first_key,"status":"SENDING"});
    assert_eq!(
        req(
            &a,
            "POST",
            &endpoint(&t, "/delivery"),
            bad,
            Some("old-reply")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let t = delivered(&a, &t, "SENDING", "reply-sending", "").await;
    let t = delivered(&a, &t, "UNKNOWN", "reply-uncertain", "").await;
    assert_eq!(t["task"]["status"], "SUCCEEDED");
    let bad = json!({"expected_sha256":t["sha256"],"delivery_key":t["task"]["delivery_key"],"status":"PENDING"});
    assert_eq!(
        req(
            &a,
            "POST",
            &endpoint(&t, "/delivery"),
            bad,
            Some("blind-resend")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let t = delivered(&a, &t, "SENT", "verified-reply", "平台回执已核验").await;
    assert_eq!(t["task"]["attempt_count"], 2);
    assert_eq!(t["task"]["delivery_status"], "SENT");
    assert_eq!(
        req(
            &a,
            "POST",
            &endpoint(&t, "/checkpoint"),
            checkpoint(&t, "RUNNING"),
            Some("rerun-success")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
#[tokio::test]
async fn context_cas_and_conversation_links_roll_back_atomically() {
    let (d, a) = app();
    create(&a, "one").await;
    let c = ok(
        &a,
        "PUT",
        "/v1/execution-contexts/conversation-a",
        json!({"title":"主Agent","goal":"完成当前工作","task_ids":["one"]}),
        Some("context"),
    )
    .await;
    let head = std::fs::read(d.path().join("HEAD")).unwrap();
    let mut p = json!({"expected_sha256":c["sha256"],"title":"其他会话","goal":"越界关联","task_ids":["one"]});
    assert_eq!(
        req(
            &a,
            "PUT",
            "/v1/execution-contexts/conversation-b",
            p.clone(),
            Some("cross-context")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    p["expected_sha256"] = json!("");
    assert_eq!(
        req(
            &a,
            "PUT",
            "/v1/execution-contexts/conversation-b",
            p,
            Some("cross-context-no-sha")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(std::fs::read(d.path().join("HEAD")).unwrap(), head);
    let restored = ok(
        &a,
        "GET",
        "/v1/execution-contexts/conversation-a",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(restored["context"], c["context"]);
    assert_eq!(restored["sha256"], c["sha256"]);
    assert_eq!(
        req(
            &a,
            "PUT",
            "/v1/execution-contexts/conversation-a",
            json!({"title":"主Agent","goal":"覆盖旧上下文"}),
            Some("missing-sha")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut child = payload("child");
    child["conversation_id"] = json!("conversation-b");
    child["parent_task_id"] = json!("one");
    assert_eq!(
        req(&a, "POST", "/v1/executions", child, Some("cross-parent"))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut child = payload("child");
    child["parent_task_id"] = json!("one");
    assert_eq!(
        req(&a, "POST", "/v1/executions", child, Some("valid-child"))
            .await
            .0,
        StatusCode::OK
    );
}
#[tokio::test]
async fn records_are_authenticated_bounded_sensitive_checked_and_separate_from_knowledge() {
    let (_d, a) = app();
    for (method, p) in [
        ("GET", "/v1/executions"),
        ("POST", "/v1/executions"),
        ("GET", "/v1/executions/one"),
        ("POST", "/v1/executions/one/attempts"),
        ("GET", "/v1/executions/one/attempts"),
        ("GET", "/v1/executions/one/attempts/a"),
        ("POST", "/v1/executions/one/checkpoint"),
        ("POST", "/v1/executions/one/cancel"),
        ("POST", "/v1/executions/one/delivery"),
        ("GET", "/v1/execution-contexts/c"),
        ("PUT", "/v1/execution-contexts/c"),
    ] {
        let res = a
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(p)
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        req(&a, "POST", "/v1/executions", payload("one"), None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    for (field, value) in [
        ("goal", json!("sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ012345")),
        ("title", json!("长".repeat(101))),
        ("unknown", json!(true)),
    ] {
        let mut p = payload("one");
        p[field] = value;
        assert_eq!(
            req(&a, "POST", "/v1/executions", p, Some(field)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let t = create(&a, "one").await;
    let t = begin(&a, &t, "begin").await;
    let mut p = checkpoint(&t, "RUNNING");
    p["checkpoint"]["completed_steps"] = json!(vec!["界".repeat(120); 8]);
    p["checkpoint"]["next_steps"] = json!(vec!["界".repeat(120); 5]);
    assert_eq!(
        req(
            &a,
            "POST",
            &endpoint(&t, "/checkpoint"),
            p,
            Some("over-budget")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let doc = ok(
        &a,
        "GET",
        "/v1/documents?path=executions/tasks/one.md",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(req(&a,"PUT","/v1/documents",json!({"path":"executions/tasks/one.md","content":doc["content"],"expected_sha256":doc["sha256"]}),Some("generic-put")).await.0,StatusCode::BAD_REQUEST);
    assert_eq!(
        ok(&a, "GET", "/v1/documents", Value::Null, None).await["total"],
        0
    );
    let route = ok(
        &a,
        "POST",
        "/v1/route",
        json!({"project":"demo","scenario":"接口","content":true}),
        None,
    )
    .await;
    assert!(
        route
            .get("project_topics")
            .and_then(Value::as_array)
            .is_none_or(|v| v.is_empty())
    );
    assert!(!route.to_string().contains("executions/"));
    assert_eq!(
        req(&a, "GET", "/v1/executions?limit=51", Value::Null, None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        req(&a, "GET", "/v1/executions?secret=true", Value::Null, None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let missing = ok(&a, "GET", "/v1/health", Value::Null, None).await;
    assert_eq!(missing["status"], "healthy");
}
#[tokio::test]
async fn daily_skips_records_receipts_and_metrics() {
    let (d, a) = app();
    let mut p = payload("one");
    p["project"] = json!("daily");
    assert_eq!(
        ok(&a, "POST", "/v1/executions", p, Some("daily-write")).await["mode"],
        "SKIP"
    );
    assert_eq!(
        ok(&a, "GET", "/v1/executions?project=daily", Value::Null, None).await["mode"],
        "SKIP"
    );
    assert_eq!(
        ok(&a, "GET", "/v1/stats", Value::Null, None).await["metrics"]["observed_calls"],
        0
    );
    assert!(!d.path().join("HEAD").exists());
}
#[test]
fn failed_multi_record_commit_does_not_publish_task_attempt_or_receipt() {
    let d = TempDir::new().unwrap();
    let mut store = Store::open(d.path()).unwrap();
    store
        .transact("create", &payload("one"), |s| {
            execution::create(s, &payload("one"))
        })
        .unwrap();
    let head = std::fs::read(d.path().join("HEAD")).unwrap();
    let response = execution::get(&store.state.documents, "one").unwrap();
    // Simulate a disk failure when creating the next revision.
    std::fs::rename(d.path().join("revisions"), d.path().join("saved-revisions")).unwrap();
    std::fs::write(d.path().join("revisions"), "block directory").unwrap();
    let v = json!({"expected_sha256":response["sha256"],"agent":"pi"});
    assert!(
        store
            .transact("begin-fails", &v, |s| execution::begin_attempt(
                s, "one", &v
            ))
            .is_err()
    );
    assert_eq!(
        execution::get(&store.state.documents, "one").unwrap()["task"]["status"],
        "QUEUED"
    );
    assert!(!store.state.receipts.contains_key("begin-fails"));
    assert_eq!(std::fs::read(d.path().join("HEAD")).unwrap(), head);
    std::fs::remove_file(d.path().join("revisions")).unwrap();
    std::fs::rename(d.path().join("saved-revisions"), d.path().join("revisions")).unwrap();
    drop(store);
    let store = Store::open(d.path()).unwrap();
    assert_eq!(
        execution::get(&store.state.documents, "one").unwrap()["task"]["status"],
        "QUEUED"
    );
}
#[test]
fn corruption_in_task_attempt_links_is_detected_before_publish() {
    let d = TempDir::new().unwrap();
    let mut store = Store::open(d.path()).unwrap();
    let t = store
        .transact("create", &payload("one"), |s| {
            execution::create(s, &payload("one"))
        })
        .unwrap();
    let v = json!({"expected_sha256":t["sha256"],"agent":"pi"});
    let t = store
        .transact("begin", &v, |s| execution::begin_attempt(s, "one", &v))
        .unwrap();
    let mut docs = store.state.documents.clone();
    let task_path = "executions/tasks/one.md";
    let (mut m, b) = document::parse(&docs[task_path]).unwrap();
    m["attempt_count"] = json!(9);
    docs.insert(task_path.into(), document::render(&m, &b).unwrap());
    assert!(document::validate_all(&docs).is_err());
    let mut docs = store.state.documents.clone();
    let ap = format!(
        "executions/attempts/{}.md",
        t["task"]["current_attempt_id"].as_str().unwrap()
    );
    let (mut m, b) = document::parse(&docs[&ap]).unwrap();
    m["task_id"] = json!("missing");
    docs.insert(ap, document::render(&m, &b).unwrap());
    assert!(document::validate_all(&docs).is_err());
}
