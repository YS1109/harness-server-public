use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use harness_server::{
    api,
    auth::Principal,
    document::{self, Documents},
    store::Store,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const A: &str = "namespace-test-a-0000000000000000000000000";
const B: &str = "namespace-test-b-0000000000000000000000000";
const R: &str = "namespace-test-read-0000000000000000000000";
fn principals() -> Vec<Principal> {
    vec![
        Principal {
            agent_id: Some("alice".into()),
            token: A.into(),
            read_namespaces: vec!["alice".into(), "shared".into()],
            write_namespaces: vec!["alice".into(), "shared".into()],
        },
        Principal {
            agent_id: Some("bob".into()),
            token: B.into(),
            read_namespaces: vec!["bob".into(), "shared".into()],
            write_namespaces: vec!["bob".into()],
        },
        Principal {
            agent_id: Some("reader".into()),
            token: R.into(),
            read_namespaces: vec!["alice".into()],
            write_namespaces: vec![],
        },
    ]
}
fn project() -> String {
    document::render(json!({"schema_version":1,"memory_type":"project","project_id":"demo","title":"Demo","status":"ACTIVE","verified_at":"unknown"}).as_object().unwrap(),"# Shared project").unwrap()
}
fn process(text: &str) -> String {
    document::render(json!({"schema_version":1,"memory_type":"process","process_id":"coding","title":"Coding","status":"ACTIVE","scope":"global","verified_at":"unknown","triggers":["coding"]}).as_object().unwrap(),text).unwrap()
}
fn setup() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("store")).unwrap();
    let mut docs = Documents::new();
    docs.insert("memory/project/demo.md".into(), project());
    docs.insert(
        "memory/process/coding.md".into(),
        process("# Shared coding"),
    );
    store.import(docs).unwrap();
    (
        dir,
        api::router_with_principals(store, principals()).unwrap(),
    )
}
#[allow(clippy::too_many_arguments)]
async fn req(
    app: &Router,
    token: &str,
    namespace: &str,
    agent: Option<&str>,
    method: &str,
    path: &str,
    body: Value,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("X-Harness-Namespace", namespace)
        .header("Content-Type", "application/json");
    if let Some(id) = agent {
        r = r.header("X-Harness-Agent-Id", id);
    }
    if let Some(k) = key {
        r = r.header("Idempotency-Key", k);
    }
    let response = app
        .clone()
        .oneshot(r.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let code = response.status();
    let v = serde_json::from_slice(
        &to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    (code, v)
}
fn start() -> Value {
    json!({"agent":"pi","native_session_id":"same-native-id","project":"demo","workstream_id":"same-workstream","title":"Task","content":true})
}

#[tokio::test]
async fn namespaces_isolate_same_ids_and_retry_keys_but_inherit_bounded_shared_memory() {
    let (_dir, app) = setup();
    for (token, ns) in [(A, "alice"), (B, "bob")] {
        let (code, first) = req(
            &app,
            token,
            ns,
            Some(ns),
            "POST",
            "/v1/start",
            start(),
            Some("same-key"),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{first}");
        assert_eq!(first["identity"]["agent_id"], ns);
        assert_eq!(first["handoff"]["session"]["namespace"], ns);
        assert_eq!(first["memories"]["content"][0]["namespace"], "shared");
        let (code, retry) = req(
            &app,
            token,
            ns,
            Some(ns),
            "POST",
            "/v1/start",
            start(),
            Some("same-key"),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(retry["idempotent"], true);
        assert_eq!(retry["execution_group_id"], first["execution_group_id"]);
        assert_eq!(
            req(
                &app,
                token,
                ns,
                Some(ns),
                "GET",
                "/v1/documents?kind=session",
                Value::Null,
                None
            )
            .await
            .1["total"],
            1
        );
    }
    let shared = req(
        &app,
        A,
        "shared",
        Some("alice"),
        "GET",
        "/v1/documents?kind=session",
        Value::Null,
        None,
    )
    .await
    .1;
    assert_eq!(shared["total"], 0);
    let route = req(
        &app,
        A,
        "alice",
        None,
        "POST",
        "/v1/route",
        json!({"project":"demo","scenario":"coding","content":true}),
        None,
    )
    .await
    .1;
    assert!(route["content"].as_array().unwrap().len() <= 4);
    assert_eq!(route["content"][1]["namespace"], "shared");
}

#[tokio::test]
async fn grants_apply_to_all_endpoints_and_identity_cannot_be_spoofed() {
    let (_dir, app) = setup();
    for path in [
        "/v1/health",
        "/v1/stats",
        "/v1/documents?kind=session",
        "/v1/executions",
        "/v1/executions/x",
        "/v1/executions/x/attempts",
        "/v1/execution-contexts/x",
    ] {
        assert_eq!(
            req(
                &app,
                A,
                "bob",
                Some("alice"),
                "GET",
                path,
                Value::Null,
                None
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        req(
            &app,
            A,
            "alice",
            Some("bob"),
            "GET",
            "/v1/health",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        req(
            &app,
            R,
            "alice",
            Some("reader"),
            "POST",
            "/v1/start",
            start(),
            Some("deny")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        req(
            &app,
            B,
            "shared",
            Some("bob"),
            "PUT",
            "/v1/documents",
            json!({"path":"memory/project/demo.md","content":project()}),
            Some("deny")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        req(
            &app,
            A,
            "../bob",
            None,
            "GET",
            "/v1/health",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        req(
            &app,
            A,
            "alice",
            None,
            "PUT",
            "/v1/documents",
            json!({"path":"namespaces/bob/memory/project/demo.md","content":project()}),
            Some("escape")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        req(
            &app,
            A,
            "alice",
            None,
            "GET",
            "/v1/documents?path=namespaces/bob/memory/project/demo.md",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let health = req(
        &app,
        R,
        "alice",
        None,
        "GET",
        "/v1/health",
        Value::Null,
        None,
    )
    .await
    .1;
    assert_eq!(health["documents"], 0); // no shared read grant, so no inherited knowledge.
}

#[tokio::test]
async fn explicit_overrides_are_local_and_shared_updates_are_explicit() {
    let (_dir, app) = setup();
    let (_, shared) = req(
        &app,
        A,
        "alice",
        None,
        "GET",
        "/v1/documents?path=memory/process/coding.md",
        Value::Null,
        None,
    )
    .await;
    let payload = json!({"path":"memory/process/coding.md","content":process("# Alice coding"),"expected_sha256":shared["sha256"]});
    assert_eq!(
        req(
            &app,
            A,
            "alice",
            None,
            "PUT",
            "/v1/documents",
            payload.clone(),
            Some("override")
        )
        .await
        .0,
        StatusCode::OK
    );
    for (token, ns, expected) in [
        (A, "alice", "# Alice coding"),
        (B, "bob", "# Shared coding"),
        (A, "shared", "# Shared coding"),
    ] {
        let v = req(
            &app,
            token,
            ns,
            None,
            "GET",
            "/v1/documents?path=memory/process/coding.md",
            Value::Null,
            None,
        )
        .await
        .1;
        assert!(v["content"].as_str().unwrap().contains(expected));
    }
    assert_eq!(
        req(
            &app,
            A,
            "alice",
            None,
            "PUT",
            "/v1/documents",
            payload,
            Some("stale-sha")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn atomic_finish_restart_and_backup_preserve_namespace_and_receipts() {
    let (dir, app) = setup();
    let first = req(
        &app,
        A,
        "alice",
        None,
        "POST",
        "/v1/start",
        start(),
        Some("begin"),
    )
    .await
    .1;
    let finish = json!({"session_id":first["session_id"],"expected_workstream_sha256":first["workstream_sha256"],"result":{"current":"Done","project_topics":[{"topic_id":"design","title":"Design","summary":"Stable decision","triggers":["namespace"],"body":"# Private design"}]}});
    let (code, v) = req(
        &app,
        A,
        "alice",
        None,
        "POST",
        "/v1/finish",
        finish.clone(),
        Some("finish"),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{v}");
    assert_eq!(
        req(
            &app,
            B,
            "bob",
            None,
            "GET",
            "/v1/documents?path=memory/project/demo/design.md",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        req(
            &app,
            A,
            "shared",
            None,
            "GET",
            "/v1/documents?path=memory/project/demo/design.md",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    drop(app);
    let store = Store::open(dir.path().join("store")).unwrap();
    assert!(
        !store
            .state
            .documents
            .contains_key("namespaces/alice/memory/process/coding.md")
    );
    let cfg = harness_server::backup::Config {
        schema_version: 1,
        data_dir: dir.path().join("store"),
        output_dir: dir.path().join("backup"),
        oss: None,
    };
    let backup = harness_server::backup::create(&cfg, true).unwrap();
    let archive = cfg
        .output_dir
        .join(backup["backup"]["archive"].as_str().unwrap());
    let restore = dir.path().join("restore");
    std::fs::create_dir(&restore).unwrap();
    tar::Archive::new(flate2::read::GzDecoder::new(
        std::fs::File::open(archive).unwrap(),
    ))
    .unpack(&restore)
    .unwrap();
    let restored = Store::open(&restore).unwrap();
    assert_eq!(restored.state.documents, store.state.documents);
    let receipt_key =
        harness_server::namespace::receipt_key("alice", Some("alice"), "finish").unwrap();
    assert_eq!(
        restored.state.receipts[&receipt_key].response["identity"]["agent_id"],
        "alice"
    );
    drop(store);
    let app = api::router_with_principals(restored, principals()).unwrap();
    let retry = req(
        &app,
        A,
        "alice",
        None,
        "POST",
        "/v1/finish",
        finish,
        Some("finish"),
    )
    .await
    .1;
    assert_eq!(retry["idempotent"], true);
    assert_eq!(
        req(
            &app,
            A,
            "alice",
            None,
            "GET",
            "/v1/documents?kind=workstream",
            Value::Null,
            None
        )
        .await
        .1["total"],
        1
    );
}

#[tokio::test]
async fn scoped_statistics_and_daily_skip_do_not_leak_other_namespaces() {
    let (_dir, app) = setup();
    req(
        &app,
        A,
        "alice",
        None,
        "POST",
        "/v1/start",
        json!({"project":"daily"}),
        Some("daily"),
    )
    .await;
    assert_eq!(
        req(
            &app,
            A,
            "alice",
            None,
            "GET",
            "/v1/stats",
            Value::Null,
            None
        )
        .await
        .1["metrics"]["observed_calls"],
        0
    );
    req(
        &app,
        B,
        "bob",
        None,
        "POST",
        "/v1/start",
        start(),
        Some("bob"),
    )
    .await;
    assert_eq!(
        req(
            &app,
            A,
            "alice",
            None,
            "GET",
            "/v1/stats",
            Value::Null,
            None
        )
        .await
        .1["metrics"]["observed_calls"],
        0
    );
    assert_eq!(
        req(&app, B, "bob", None, "GET", "/v1/stats", Value::Null, None)
            .await
            .1["metrics"]["observed_calls"],
        1
    );
}

#[test]
fn physical_paths_and_credentials_fail_closed() {
    for p in [
        "namespaces/shared/memory/project/demo.md",
        "namespaces/alice/namespaces/bob/memory/project/demo.md",
        "namespaces/../memory/project/demo.md",
    ] {
        assert!(document::path_kind(p).is_err());
    }
    let mut duplicate = principals();
    duplicate[1].token = A.into();
    assert!(harness_server::auth::validate(&mut duplicate).is_err());
    let mut grants = principals();
    grants[1].write_namespaces.push("alice".into());
    assert!(harness_server::auth::validate(&mut grants).is_err());
}

#[tokio::test]
async fn execution_tasks_attempts_and_contexts_stay_inside_namespace() {
    let (dir, app) = setup();
    let payload = json!({"task_id":"same-task","conversation_id":"qq","source_key":"message","project":"demo","title":"QQ task","goal":"Recover safely"});
    let (code, task) = req(
        &app,
        A,
        "alice",
        None,
        "POST",
        "/v1/executions",
        payload.clone(),
        Some("task"),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{task}");
    assert_eq!(
        req(
            &app,
            B,
            "bob",
            None,
            "GET",
            "/v1/executions/same-task",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let attempt = req(
        &app,
        A,
        "alice",
        None,
        "POST",
        "/v1/executions/same-task/attempts",
        json!({"agent":"pi","expected_sha256":task["sha256"]}),
        Some("claim"),
    )
    .await;
    assert_eq!(attempt.0, StatusCode::OK, "{}", attempt.1);
    assert_eq!(
        req(
            &app,
            B,
            "bob",
            None,
            "GET",
            "/v1/executions/same-task/attempts",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let context = req(
        &app,
        A,
        "alice",
        None,
        "PUT",
        "/v1/execution-contexts/qq",
        json!({"title":"Dispatch","goal":"Continue","task_ids":["same-task"]}),
        Some("context"),
    )
    .await;
    assert_eq!(context.0, StatusCode::OK, "{}", context.1);
    assert_eq!(
        req(
            &app,
            B,
            "bob",
            None,
            "GET",
            "/v1/execution-contexts/qq",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        req(
            &app,
            B,
            "bob",
            None,
            "POST",
            "/v1/executions",
            payload,
            Some("task")
        )
        .await
        .0,
        StatusCode::OK
    );
    drop(app);
    let app =
        api::router_with_principals(Store::open(dir.path().join("store")).unwrap(), principals())
            .unwrap();
    assert_eq!(
        req(
            &app,
            A,
            "alice",
            None,
            "GET",
            "/v1/executions/same-task/attempts",
            Value::Null,
            None
        )
        .await
        .1["total"],
        1
    );
    assert_eq!(
        req(
            &app,
            B,
            "bob",
            None,
            "GET",
            "/v1/executions/same-task/attempts",
            Value::Null,
            None
        )
        .await
        .1["total"],
        0
    );
}

#[tokio::test]
async fn failed_private_finish_does_not_publish_partial_memory_or_receipt() {
    let (dir, app) = setup();
    let start = req(
        &app,
        A,
        "alice",
        None,
        "POST",
        "/v1/start",
        start(),
        Some("start"),
    )
    .await
    .1;
    let head = std::fs::read(dir.path().join("store/HEAD")).unwrap();
    let result = json!({"session_id":start["session_id"],"expected_workstream_sha256":start["workstream_sha256"],"result":{"current":"Done","project_topics":[{"topic_id":"too-large","title":"Large","summary":"Not saved","triggers":["x"],"body":"x".repeat(6001)}]}});
    assert_eq!(
        req(
            &app,
            A,
            "alice",
            None,
            "POST",
            "/v1/finish",
            result,
            Some("failed-finish")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(std::fs::read(dir.path().join("store/HEAD")).unwrap(), head);
    drop(app);
    let store = Store::open(dir.path().join("store")).unwrap();
    let key =
        harness_server::namespace::receipt_key("alice", Some("alice"), "failed-finish").unwrap();
    assert!(!store.state.receipts.contains_key(&key));
    assert!(
        !store
            .state
            .documents
            .keys()
            .any(|p| p.contains("too-large"))
    );
}

#[tokio::test]
async fn upgrading_credential_configuration_preserves_legacy_shared_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let app = api::router(store, vec![A.into()]).unwrap();
    assert_eq!(
        req(
            &app,
            A,
            "alice",
            None,
            "GET",
            "/v1/health",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        req(
            &app,
            A,
            "shared",
            Some("alice"),
            "GET",
            "/v1/health",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (code, first) = req(
        &app,
        A,
        "shared",
        None,
        "POST",
        "/v1/start",
        start(),
        Some("old-retry"),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{first}");
    drop(app);
    let app = api::router_with_principals(Store::open(dir.path()).unwrap(), principals()).unwrap();
    let (code, retry) = req(
        &app,
        A,
        "shared",
        None,
        "POST",
        "/v1/start",
        start(),
        Some("old-retry"),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{retry}");
    assert_eq!(retry["idempotent"], true);
    assert_eq!(retry["execution_group_id"], first["execution_group_id"]);
    assert_eq!(retry["identity"]["agent_id"], "alice");
}
