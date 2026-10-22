use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use harness_server::{
    api,
    document::{self, Documents},
    routing_eval,
    store::Store,
};
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
};
use tower::ServiceExt;
const TOKEN: &str = "fixture-agent-000000000000000000000000";
const ADMIN: &str = "fixture-admin-000000000000000000000000";
fn service(root: &std::path::Path) -> Router {
    api::router_with_admin(
        Store::open(root).unwrap(),
        harness_server::auth::legacy(vec![TOKEN.into()]),
        Some(ADMIN.into()),
    )
    .unwrap()
}
async fn call(
    app: &Router,
    admin: bool,
    method: &str,
    path: &str,
    value: Value,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .uri(path)
        .method(method)
        .header(
            "Authorization",
            format!("Bearer {}", if admin { ADMIN } else { TOKEN }),
        )
        .header("Content-Type", "application/json");
    if let Some(key) = key {
        req = req.header("Idempotency-Key", key);
    }
    let r = app
        .clone()
        .oneshot(req.body(Body::from(value.to_string())).unwrap())
        .await
        .unwrap();
    let status = r.status();
    let b = to_bytes(r.into_body(), 4 * 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&b).unwrap())
}
fn project(id: &str) -> String {
    document::render(json!({"schema_version":1,"memory_type":"project","project_id":id,"title":id,"status":"ACTIVE","verified_at":"unknown"}).as_object().unwrap(),"# Fixture").unwrap()
}
fn fixtures() -> Documents {
    let mut docs = Documents::new();
    docs.insert("memory/project/agent-home.md".into(), project("agent-home"));
    docs.insert("memory/project/other.md".into(), project("other"));
    for (id, triggers, aliases, body) in [
        (
            "client-rules",
            json!(["客户端规则", "instructions"]),
            json!(["embedded rules"]),
            "# Client\n\nclient_rules write_protocol",
        ),
        (
            "namespace",
            json!(["知识空间"]),
            json!([]),
            "# Namespace\n\nnamespace_id",
        ),
        (
            "shared-http-service",
            json!(["HTTP"]),
            json!([]),
            "# Example\n\n示例符号client_rules与namespace_id",
        ),
    ] {
        let content=document::render(json!({"schema_version":1,"memory_type":"project_topic","project_id":"agent-home","topic_id":id,"title":id,"status":"ACTIVE","verified_at":"unknown","summary":"Desensitized fixture","triggers":triggers,"aliases":aliases}).as_object().unwrap(),body).unwrap();
        docs.insert(format!("memory/project/agent-home/{id}.md"), content);
    }
    docs
}
#[test]
fn invalid_result_is_rejected_locally_without_http_or_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = dir.path().join("client.json");
    std::fs::write(
        &config,
        json!({"url":format!("http://{}",listener.local_addr().unwrap())}).to_string(),
    )
    .unwrap();
    for (value, field) in [
        (
            json!({"current":"Done","verification":"not-an-array"}),
            "result.verification",
        ),
        (
            json!({"current":"Done","project_topics":[{"topic_id":"topic","title":"Title","summary":"Short","triggers":["1","2","3","4","5","6","7","8","9"],"body":"Knowledge"}]}),
            "result.project_topics[0].triggers",
        ),
        (
            json!({"current":"Done","verification":["中".repeat(121)]}),
            "result.verification[0]",
        ),
    ] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_harness"));
        cmd.arg("--config")
            .arg(&config)
            .args([
                "--timings",
                "finish",
                "--mode",
                "log",
                "--project",
                "demo",
                "--result-file",
                "-",
            ])
            .env("HARNESS_TOKEN", TOKEN)
            .env_remove("HARNESS_URL")
            .env_remove("HARNESS_NAMESPACE")
            .env_remove("HARNESS_AGENT_ID")
            .env_remove("HARNESS_READ_NAMESPACES")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(value.to_string().as_bytes())
            .unwrap();
        let o = child.wait_with_output().unwrap();
        assert!(!o.status.success());
        let error = String::from_utf8_lossy(&o.stderr);
        assert!(error.contains(field), "{error}");
        assert!(error.contains("\"http_requests\":0"));
        assert!(o.stdout.is_empty());
    }
    assert!(listener.accept().is_err());
}
#[tokio::test]
async fn phases_groups_replays_and_scoped_protocol_have_honest_denominators() {
    let dir = tempfile::tempdir().unwrap();
    let app = service(dir.path());
    let start =
        json!({"agent":"test","native_session_id":"phases","project":"demo","title":"Task"});
    let (s, r) = call(
        &app,
        false,
        "POST",
        "/v2/scoped/start",
        start.clone(),
        Some("start"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        call(
            &app,
            false,
            "POST",
            "/v2/scoped/start",
            start,
            Some("start")
        )
        .await
        .1["idempotent"],
        true
    );
    let group = r["execution_group_id"].clone();
    let finish = json!({"session_id":r["session_id"],"expected_workstream_sha256":r["workstream_sha256"],"execution_group_id":group,"result":{"current":"Done","verification":[]}});
    assert_eq!(
        call(
            &app,
            false,
            "POST",
            "/v2/scoped/finish",
            finish,
            Some("finish")
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, stats) = call(&app, false, "GET", "/v1/stats", Value::Null, None).await;
    assert_eq!(stats["metrics"]["observed_calls"], 3);
    assert_eq!(stats["metrics"]["grouped_calls"], 3);
    assert_eq!(stats["metrics"]["execution_groups"], 1);
    assert_eq!(stats["metrics"]["protocols"]["scoped-v2"], 3);
    let starts = &stats["command_metrics"]["start"];
    assert_eq!(starts["observed_calls"], 2);
    assert_eq!(starts["retries"], 1);
    assert_eq!(starts["commit_observed_calls"], 1);
    assert!(starts["operation_ms_p50"].is_number());
    assert!(starts["lock_wait_ms_p50"].is_number());
    assert_eq!(
        call(&app, false, "GET", "/v1/stats", Value::Null, None)
            .await
            .1["cache_hit"],
        true
    );
    let (_, other) = call(
        &app,
        false,
        "GET",
        "/v1/stats?project=other",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(other["metrics"]["observed_calls"], 0);
    assert_eq!(other["cache_hit"], false);
    call(
        &app,
        false,
        "POST",
        "/v1/route",
        json!({"project":"demo"}),
        None,
    )
    .await;
    let (_, stats) = call(&app, false, "GET", "/v1/stats", Value::Null, None).await;
    assert_eq!(stats["cache_hit"], false);
    assert_eq!(stats["metrics"]["observed_calls"], 4);
    assert!(stats["command_metrics"]["route"]["commit_ms_p50"].is_null());
}
#[tokio::test]
async fn runtime_needs_no_disk_scan_and_disk_failures_remain_independent() {
    let dir = tempfile::tempdir().unwrap();
    let app = service(dir.path());
    let (_, before) = call(&app, true, "GET", "/admin/api/runtime", Value::Null, None).await;
    assert!(before["storage"]["disk_bytes"].is_null());
    assert!(before["routing_evaluation"].is_null());
    std::fs::rename(
        dir.path().join("revisions"),
        dir.path().join("revisions-held"),
    )
    .unwrap();
    assert_eq!(
        call(&app, true, "GET", "/admin/api/runtime", Value::Null, None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, true, "GET", "/admin/api/storage", Value::Null, None)
            .await
            .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    std::fs::rename(
        dir.path().join("revisions-held"),
        dir.path().join("revisions"),
    )
    .unwrap();
    let (_, first) = call(&app, true, "GET", "/admin/api/storage", Value::Null, None).await;
    assert!(first["storage"]["disk_bytes"].is_number());
    assert_eq!(first["cache_hit"], false);
    let (_, cached) = call(&app, true, "GET", "/admin/api/storage", Value::Null, None).await;
    assert_eq!(cached["cache_hit"], true);
    assert_eq!(cached["sample_at"], first["sample_at"]);
    assert_eq!(
        call(
            &app,
            true,
            "GET",
            "/admin/api/storage?fresh=true",
            Value::Null,
            None
        )
        .await
        .1["cache_hit"],
        false
    );
    assert_eq!(
        call(&app, false, "GET", "/admin/api/storage", Value::Null, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}
#[test]
fn routing_suite_scores_missing_dependencies_misses_and_false_selections_explicitly() {
    let docs = fixtures();
    let scopes = vec!["shared".into()];
    let suite = json!({"version":"fixture-v1","cases":[
        {"id":"correct","project":"agent-home","query":"instructions","expected_topic":"client-rules"},
        {"id":"miss","project":"agent-home","query":"天文望远镜","expected_topic":"client-rules"},
        {"id":"negative","project":"agent-home","query":"instructions","expected_topic":null},
        {"id":"unrelated","project":"agent-home","query":"天文望远镜","expected_topic":null},
        {"id":"missing","project":"missing","query":"instructions","expected_topic":null},
        {"id":"missing-topic","project":"agent-home","query":"gone","expected_topic":"gone"}
    ]});
    let r = routing_eval::evaluate(&docs, &scopes, "shared", &suite).unwrap();
    let s = &r["summary"];
    assert_eq!(s["total"], 6);
    assert_eq!(s["unavailable"], 2);
    assert_eq!(s["positive_evaluated"], 2);
    assert_eq!(s["positive_correct"], 1);
    assert_eq!(s["positive_accuracy"], 0.5);
    assert_eq!(s["missed"], 1);
    assert_eq!(s["negative_evaluated"], 2);
    assert_eq!(s["false_selections"], 1);
    assert!(!r.to_string().contains("天文望远镜"));
    assert_eq!(r["suite_version"], "fixture-v1");
    assert_eq!(
        r["snapshot_fingerprint"],
        routing_eval::evaluate(&docs, &scopes, "shared", &suite).unwrap()["snapshot_fingerprint"]
    );
    let mut changed = docs.clone();
    changed
        .get_mut("memory/project/agent-home.md")
        .unwrap()
        .push_str("\nChanged\n");
    assert_ne!(
        r["snapshot_fingerprint"],
        routing_eval::evaluate(&changed, &scopes, "shared", &suite).unwrap()["snapshot_fingerprint"]
    );
    assert!(routing_eval::parse(&json!({"version":"v1","cases":[{"id":"implicit-negative","project":"agent-home","query":"x"}]})).is_err());
    let no_positive = json!({"version":"v1","cases":[{"id":"negative","project":"other","query":"instructions","expected_topic":null}]});
    assert!(routing_eval::evaluate(&docs,&scopes,"shared",&no_positive).unwrap()["summary"]["positive_accuracy"].is_null());
}
#[tokio::test]
async fn invalid_server_result_keeps_revision_and_receipts_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let app = service(dir.path());
    let (_, before) = call(&app, true, "GET", "/admin/api/runtime", Value::Null, None).await;
    let (status, error) = call(&app, false, "POST", "/v1/finish",
        json!({"mode":"log", "project":"demo", "result":{"current":"Done", "verification":"private-invalid-value"}}), Some("invalid")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["details"]["field"], "result.verification");
    assert_eq!(error["error"]["details"]["actual"], "string");
    assert_eq!(error["error"]["details"]["saved"], false);
    assert!(!error.to_string().contains("private-invalid-value"));
    let (_, after) = call(&app, true, "GET", "/admin/api/runtime", Value::Null, None).await;
    assert_eq!(before["storage"]["revision"], after["storage"]["revision"]);
    assert_eq!(before["storage"]["receipts"], after["storage"]["receipts"]);
}
#[tokio::test]
async fn evaluation_client_makes_one_readonly_request_and_checks_all_scopes() {
    use axum::middleware::{self, Next};
    use std::sync::{Arc, Mutex};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let mut store = Store::open(&root).unwrap();
    store.import(fixtures()).unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observed_mw = observed.clone();
    let app = api::router_with_admin(
        store,
        harness_server::auth::legacy(vec![TOKEN.into()]),
        Some(ADMIN.into()),
    )
    .unwrap()
    .layer(middleware::from_fn(
        move |req: Request<Body>, next: Next| {
            let observed = observed_mw.clone();
            async move {
                observed.lock().unwrap().push((
                    req.uri().path().to_owned(),
                    req.headers().contains_key("Idempotency-Key"),
                ));
                next.run(req).await
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server_app = app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, server_app).await.unwrap() });
    let config = dir.path().join("client.json");
    std::fs::write(
        &config,
        json!({"url":url,"namespace":"shared","read_namespaces":["shared"]}).to_string(),
    )
    .unwrap();
    let suite = dir.path().join("suite.json");
    std::fs::write(&suite, include_str!("../examples/routing-suite.json")).unwrap();
    // Failure to persist evaluation metadata must not fail the read-only evaluation.
    std::fs::create_dir_all(root.join(".runtime")).unwrap();
    std::fs::write(root.join(".runtime/evaluations"), "blocked").unwrap();
    let result = tokio::task::spawn_blocking(move || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_harness"));
        c.arg("--config")
            .arg(config)
            .args(["--timings", "eval-routing", "--json", "--suite"])
            .arg(suite)
            .env("HARNESS_TOKEN", TOKEN);
        for name in [
            "HARNESS_URL",
            "HARNESS_NAMESPACE",
            "HARNESS_AGENT_ID",
            "HARNESS_READ_NAMESPACES",
            "HARNESS_CLIENT_CONFIG",
        ] {
            c.env_remove(name);
        }
        c.output().unwrap()
    })
    .await
    .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("\"http_requests\":1"));
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["report_saved"], false);
    assert_eq!(report["summary"]["positive_accuracy"], 1.0);
    assert_eq!(
        *observed.lock().unwrap(),
        vec![("/v1/eval-routing".into(), false)]
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/eval-routing")
                .header("Authorization", format!("Bearer {TOKEN}"))
                .header("Content-Type", "application/json")
                .header("X-Harness-Read-Namespaces", "shared,forbidden")
                .body(Body::from(include_str!("../examples/routing-suite.json")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let (_, stats) = call(&app, false, "GET", "/v1/stats", Value::Null, None).await;
    assert_eq!(stats["metrics"]["observed_calls"], 0);
    server.abort();
}
#[tokio::test]
async fn explicit_evaluation_uses_live_readonly_snapshot_without_receipts_or_metrics() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.import(fixtures()).unwrap();
    let app = api::router_with_admin(
        store,
        harness_server::auth::legacy(vec![TOKEN.into()]),
        Some(ADMIN.into()),
    )
    .unwrap();
    let (_, before) = call(&app, true, "GET", "/admin/api/runtime", Value::Null, None).await;
    let suite: Value =
        serde_json::from_str(include_str!("../examples/routing-suite.json")).unwrap();
    let (s, r) = call(&app, false, "POST", "/v1/eval-routing", suite, None).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["summary"]["unavailable"], 0);
    assert_eq!(r["summary"]["false_selections"], 0);
    assert_eq!(r["summary"]["positive_accuracy"], 1.0);
    assert_eq!(r["report_saved"], true);
    let (_, after) = call(&app, true, "GET", "/admin/api/runtime", Value::Null, None).await;
    assert_eq!(after["storage"]["revision"], before["storage"]["revision"]);
    assert_eq!(after["metrics"]["observed_calls"], 0);
    assert_eq!(
        after["routing_evaluation"]["suite_fingerprint"],
        r["suite_fingerprint"]
    );
    let (_, latest) = call(
        &app,
        true,
        "GET",
        "/admin/api/routing-evaluation?namespace=shared",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(
        latest["latest"]["snapshot_fingerprint"],
        r["snapshot_fingerprint"]
    );
    let mut private = Documents::new();
    for (p, c) in fixtures() {
        private.insert(harness_server::namespace::physical("device", &p), c);
    }
    let unavailable=routing_eval::evaluate(&private,&["shared".into()],"shared",&json!({"version":"isolation","cases":[{"id":"isolated","project":"agent-home","query":"instructions","expected_topic":"client-rules"}]})).unwrap();
    assert_eq!(unavailable["summary"]["unavailable"], 1);
    // A corrupted result is distinguished from a missing run; no memory transaction.
    std::fs::write(
        dir.path().join(".runtime/evaluations/latest.json"),
        "broken",
    )
    .unwrap();
    assert_eq!(
        routing_eval::latest(dir.path(), None)["status"],
        "unavailable"
    );
}
#[test]
#[ignore = "manual local full-revision capacity baseline; not run per turn"]
fn revision_growth_baseline() {
    for n in [10, 100, 500] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let seed = Documents::from_iter((0..n).map(|i| {
            let id = format!("fixture-{i}");
            (format!("memory/project/{id}.md"), project(&id))
        }));
        store.import(seed).unwrap();
        let before = store.usage().unwrap();
        let mut times = Vec::new();
        for i in 0..3 {
            let t = std::time::Instant::now();
            store
                .transact(&format!("sample-{i}"), &json!({"sample":i}), |state| {
                    state
                        .documents
                        .get_mut("memory/project/fixture-0.md")
                        .unwrap()
                        .push_str("\nStable\n");
                    Ok(json!({"updated":true}))
                })
                .unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        let after = store.usage().unwrap();
        println!(
            "{}",
            json!({"documents":n,"samples":3,"transaction_ms_p50":times[1],"disk_before":before["disk_bytes"],"disk_after":after["disk_bytes"],"receipts":after["receipts"],"scope":"local debug full snapshots with synthetic documents; not production/task latency"})
        );
    }
}
