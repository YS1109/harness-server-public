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
const TOKEN: &str = "test-agent-0000000000000000000000000000";
const ADMIN: &str = "test-admin-0000000000000000000000000000";
fn principals() -> Vec<Principal> {
    vec![Principal {
        agent_id: Some("alice".into()),
        token: TOKEN.into(),
        read_namespaces: vec![],
        write_namespaces: vec![],
    }]
}
fn setup() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let app = api::router_with_admin(store, principals(), Some(ADMIN.into())).unwrap();
    (dir, app)
}
#[allow(clippy::too_many_arguments)]
async fn call(
    app: &Router,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
    ns: Option<&str>,
    scopes: Option<&str>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .uri(path)
        .method(method)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json");
    if let Some(ns) = ns {
        b = b.header("X-Harness-Namespace", ns);
    }
    if let Some(scopes) = scopes {
        b = b.header("X-Harness-Read-Namespaces", scopes);
    }
    if let Some(key) = key {
        b = b.header("Idempotency-Key", key);
    }
    let r = app
        .clone()
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = r.status();
    let v =
        serde_json::from_slice(&to_bytes(r.into_body(), 3 * 1024 * 1024).await.unwrap()).unwrap();
    (status, v)
}
async fn overview(app: &Router) -> Value {
    let (s, v) = call(
        app,
        ADMIN,
        "GET",
        "/admin/api/overview",
        Value::Null,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}
async fn create(app: &Router, name: &str) -> String {
    let (s, v) = call(
        app,
        ADMIN,
        "POST",
        "/admin/api/namespaces",
        json!({"name":name,"description":""}),
        None,
        None,
        Some(name),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["namespace"]["id"].as_str().unwrap().into()
}
async fn grant(
    app: &Router,
    reads: &[&str],
    writes: &[&str],
    defaults: &[&str],
    write: Option<&str>,
) -> Value {
    let o = overview(app).await;
    let (s,v)=call(app,ADMIN,"PUT","/admin/api/agents/alice",json!({"name":"Alice","read_namespaces":reads,"write_namespaces":writes,"default_read_namespaces":defaults,"default_write_namespace":write,"expected_sha256":o["agents"][0]["sha256"]}),None,None,Some(&uuid::Uuid::new_v4().to_string())).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}
fn project() -> String {
    document::render(json!({"schema_version":1,"memory_type":"project","project_id":"demo","title":"Demo","status":"ACTIVE","verified_at":"unknown"}).as_object().unwrap(),"# Demo").unwrap()
}
fn topic(id: &str, triggers: Value) -> String {
    document::render(json!({"schema_version":1,"memory_type":"project_topic","project_id":"demo","topic_id":id,"title":id,"summary":"Stable knowledge","triggers":triggers,"status":"ACTIVE","verified_at":"unknown"}).as_object().unwrap(),"# Knowledge").unwrap()
}
async fn put(app: &Router, ns: &str, path: &str, content: String) {
    let (s, v) = call(
        app,
        TOKEN,
        "PUT",
        "/v1/documents",
        json!({"path":path,"content":content}),
        Some(ns),
        None,
        Some(&uuid::Uuid::new_v4().to_string()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}
#[tokio::test]
async fn server_creates_ids_and_empty_agents_discover_without_auto_registration() {
    let (_dir, app) = setup();
    let (s, v) = call(
        &app,
        TOKEN,
        "GET",
        "/v1/namespaces",
        Value::Null,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["namespaces"], json!([]));
    assert_eq!(
        call(
            &app,
            TOKEN,
            "POST",
            "/v1/start",
            json!({"project":"demo"}),
            Some("invented"),
            None,
            Some("unknown")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let id = create(&app, "Work").await;
    assert!(id.starts_with("ns-"));
    uuid::Uuid::parse_str(&id[3..]).unwrap();
    let (s, retry) = call(
        &app,
        ADMIN,
        "POST",
        "/admin/api/namespaces",
        json!({"name":"Work","description":""}),
        None,
        None,
        Some("Work"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(retry["namespace"]["id"], id);
    assert_eq!(retry["idempotent"], true);
    assert_eq!(
        call(
            &app,
            ADMIN,
            "POST",
            "/admin/api/namespaces",
            json!({"name":"X","id":"client-id"}),
            None,
            None,
            Some("id-reject")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        overview(&app).await["namespaces"].as_array().unwrap().len(),
        2
    );
    grant(&app, &[&id, "shared"], &[&id], &[&id, "shared"], Some(&id)).await;
    assert_eq!(
        call(
            &app,
            TOKEN,
            "GET",
            "/v1/health",
            Value::Null,
            None,
            None,
            None
        )
        .await
        .1["identity"]["namespace"],
        id
    );
}
#[tokio::test]
async fn management_requires_separate_token_and_control_paths_cannot_bypass_it() {
    let (_dir, app) = setup();
    assert_eq!(
        call(
            &app,
            TOKEN,
            "GET",
            "/admin/api/overview",
            Value::Null,
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            ADMIN,
            "GET",
            "/v1/namespaces",
            Value::Null,
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    grant(&app, &["shared"], &["shared"], &["shared"], Some("shared")).await;
    for path in [
        "control/agents/alice.md",
        "namespaces/ns-x/control/agents/alice.md",
    ] {
        assert_eq!(
            call(
                &app,
                TOKEN,
                "GET",
                &format!("/v1/documents?path={path}"),
                Value::Null,
                None,
                None,
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        call(
            &app,
            TOKEN,
            "GET",
            "/v1/documents",
            Value::Null,
            None,
            None,
            None
        )
        .await
        .1["total"],
        0
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    let request = Request::builder()
        .method("POST")
        .uri("/admin/api/namespaces")
        .header("Authorization", format!("Bearer {ADMIN}"))
        .header("Host", "localhost:8080")
        .header("Origin", "https://evil.invalid")
        .header("Content-Type", "application/json")
        .header("Idempotency-Key", "cross-origin")
        .body(Body::from(r#"{"name":"No"}"#))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn cas_permissions_archive_and_audit_are_atomic_and_revocation_is_immediate() {
    let (_dir, app) = setup();
    let ns = create(&app, "Archive").await;
    grant(&app, &[&ns], &[&ns], &[&ns], Some(&ns)).await;
    put(&app, &ns, "memory/project/demo.md", project()).await;
    let o = overview(&app).await;
    let space = o["namespaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == ns)
        .unwrap();
    let payload = json!({"name":"Archive","description":"","status":"ARCHIVED","expected_sha256":space["sha256"]});
    let endpoint = format!("/admin/api/namespaces/{ns}");
    assert_eq!(
        call(
            &app,
            ADMIN,
            "PUT",
            &endpoint,
            payload.clone(),
            None,
            None,
            Some("archive")
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            ADMIN,
            "PUT",
            &endpoint,
            payload,
            None,
            None,
            Some("stale")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            TOKEN,
            "GET",
            "/v1/documents?path=memory/project/demo.md",
            Value::Null,
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            TOKEN,
            "POST",
            "/v1/start",
            json!({"project":"demo"}),
            None,
            None,
            Some("archived-write")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    grant(&app, &[], &[], &[], None).await;
    assert_eq!(
        call(
            &app,
            TOKEN,
            "GET",
            "/v1/documents",
            Value::Null,
            Some(&ns),
            None,
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let o = overview(&app).await;
    assert_eq!(o["audit"].as_array().unwrap().len(), 4);
    let shared = o["namespaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "shared")
        .unwrap();
    assert_eq!(call(&app,ADMIN,"PUT","/admin/api/namespaces/shared",json!({"name":"Shared","description":"","status":"ARCHIVED","expected_sha256":shared["sha256"]}),None,None,Some("archive-shared")).await.0,StatusCode::BAD_REQUEST);
    assert_eq!(overview(&app).await["audit"].as_array().unwrap().len(), 4);
}
#[tokio::test]
async fn persisted_grants_survive_restart_and_new_credentials_do_not_create_spaces() {
    let (dir, app) = setup();
    let ns = create(&app, "Persist").await;
    grant(&app, &[&ns], &[&ns], &[&ns], Some(&ns)).await;
    drop(app);
    let mut p = principals();
    p[0].read_namespaces = vec!["invented".into()];
    p[0].write_namespaces = vec!["invented".into()];
    p.push(Principal {
        agent_id: Some("new-agent".into()),
        token: "new-agent-0000000000000000000000000000".into(),
        read_namespaces: vec!["another-invented".into()],
        write_namespaces: vec![],
    });
    let app =
        api::router_with_admin(Store::open(dir.path()).unwrap(), p, Some(ADMIN.into())).unwrap();
    let o = overview(&app).await;
    assert_eq!(o["namespaces"].as_array().unwrap().len(), 2);
    let alice = o["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "alice")
        .unwrap();
    assert_eq!(alice["default_write_namespace"], ns);
    let other = o["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "new-agent")
        .unwrap();
    assert_eq!(other["read_namespaces"], json!([]));
    let cfg = harness_server::backup::Config {
        schema_version: 1,
        data_dir: dir.path().into(),
        output_dir: dir
            .path()
            .parent()
            .unwrap()
            .join(format!("registry-backup-{}", uuid::Uuid::new_v4())),
        oss: None,
    };
    let backup = harness_server::backup::create(&cfg, true).unwrap();
    harness_server::backup::verify(
        &cfg,
        std::path::Path::new(backup["manifest"].as_str().unwrap()),
    )
    .unwrap();
    std::fs::remove_dir_all(&cfg.output_dir).unwrap();
    drop(app);
    let store = Store::open(dir.path()).unwrap();
    assert!(
        store
            .state
            .documents
            .contains_key("control/agents/alice.md")
    );
    assert!(
        !store
            .state
            .documents
            .values()
            .any(|c| c.contains(TOKEN) || c.contains(ADMIN))
    );
}
#[tokio::test]
async fn multi_space_routes_share_budget_preserve_origins_and_reject_ambiguous_explicit_topics() {
    let (_dir, app) = setup();
    let a = create(&app, "A").await;
    let b = create(&app, "B").await;
    grant(&app, &[&a, &b], &[&a, &b], &[&a, &b], Some(&a)).await;
    for ns in [&a, &b] {
        put(&app, ns, "memory/project/demo.md", project()).await;
        put(
            &app,
            ns,
            "memory/project/demo/same.md",
            topic("same", json!(["routing"])),
        )
        .await;
        for i in 0..3 {
            let id = format!("coding-{i}");
            let c=document::render(json!({"schema_version":1,"memory_type":"process","process_id":id,"title":id,"triggers":["coding"],"verified_at":"unknown","status":"ACTIVE"}).as_object().unwrap(),"# Coding").unwrap();
            put(&app, ns, &format!("memory/process/{id}.md"), c).await;
        }
    }
    let (s, v) = call(
        &app,
        TOKEN,
        "POST",
        "/v1/route",
        json!({"project":"demo","scenario":"routing coding","content":true}),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["route_outcome"], "ambiguous");
    assert_eq!(v["process_memories"].as_array().unwrap().len(), 2);
    assert_eq!(v["content"].as_array().unwrap().len(), 3);
    assert_eq!(v["topic_candidates"].as_array().unwrap().len(), 2);
    assert_ne!(
        v["topic_candidates"][0]["namespace"],
        v["topic_candidates"][1]["namespace"]
    );
    assert_eq!(v["project_memory"]["namespace"], a);
    put(
        &app,
        &b,
        "memory/project/demo/winner.md",
        topic("winner", json!(["routing", "focused"])),
    )
    .await;
    let (code, strong) = call(
        &app,
        TOKEN,
        "POST",
        "/v1/route",
        json!({"project":"demo","scenario":"routing focused coding","content":true}),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(strong["route_outcome"], "auto");
    assert_eq!(strong["project_topics"][0]["namespace"], b);
    assert_eq!(strong["content"].as_array().unwrap().len(), 4);
    assert_eq!(
        call(
            &app,
            TOKEN,
            "POST",
            "/v1/route",
            json!({"project":"demo","topic":"same"}),
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (s, v) = call(
        &app,
        TOKEN,
        "POST",
        "/v1/route",
        json!({"project":"demo","topic":"same","topic_namespace":b,"content":true}),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["project_topics"][0]["namespace"], b);
    assert!(
        v["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["namespace"] == b)
    );
    assert_eq!(
        call(
            &app,
            TOKEN,
            "POST",
            "/v1/route",
            json!({"project":"demo"}),
            None,
            Some("shared"),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (s, v) = call(
        &app,
        TOKEN,
        "POST",
        "/v1/route",
        json!({"project":"demo","content":true}),
        Some(&a),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["read_namespaces"], json!([a]));
    assert_eq!(v["memory_sources"][0]["namespace"], a);
    assert_eq!(v["project_memory"], "memory/project/demo.md");
}
#[tokio::test]
async fn session_restores_and_finishes_original_space_after_defaults_change_and_ambiguous_ids_fail()
{
    let (_dir, app) = setup();
    let a = create(&app, "Original").await;
    let b = create(&app, "Next").await;
    grant(&app, &[&a, &b], &[&a, &b], &[&a, &b], Some(&a)).await;
    let payload = json!({"project":"demo","agent":"pi","native_session_id":"one","title":"Task","content":true});
    let (s, start) = call(
        &app,
        TOKEN,
        "POST",
        "/v1/start",
        payload.clone(),
        None,
        None,
        Some("start"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{start}");
    assert_eq!(start["identity"]["namespace"], a);
    grant(&app, &[&a, &b], &[&a, &b], &[&b, &a], Some(&b)).await;
    let (s, continued) = call(
        &app,
        TOKEN,
        "POST",
        "/v1/start",
        payload.clone(),
        None,
        None,
        Some("continue"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{continued}");
    assert_eq!(continued["identity"]["namespace"], a);
    let finish = json!({"session_id":start["session_id"],"expected_workstream_sha256":continued["workstream_sha256"],"result":{"current":"Done"}});
    let (s, v) = call(
        &app,
        TOKEN,
        "POST",
        "/v1/finish",
        finish.clone(),
        None,
        None,
        Some("finish"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["identity"]["namespace"], a);
    assert_eq!(
        call(
            &app,
            TOKEN,
            "POST",
            "/v1/finish",
            finish,
            None,
            None,
            Some("finish")
        )
        .await
        .1["idempotent"],
        true
    );
    assert_eq!(
        call(
            &app,
            TOKEN,
            "POST",
            "/v1/start",
            payload,
            Some(&b),
            None,
            Some("second")
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            TOKEN,
            "POST",
            "/v1/finish",
            json!({"session_id":start["session_id"]}),
            None,
            None,
            Some("ambiguous")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
#[tokio::test]
async fn cached_start_receipts_cannot_return_revoked_memory() {
    let (_dir, app) = setup();
    let a = create(&app, "Writer").await;
    let b = create(&app, "Reader").await;
    grant(&app, &[&a, &b], &[&a], &[&a, &b], Some(&a)).await;
    put(&app, &a, "memory/project/demo.md", project()).await;
    let start = json!({"agent":"pi","native_session_id":"receipt","project":"demo","title":"Task","content":true});
    assert_eq!(
        call(
            &app,
            TOKEN,
            "POST",
            "/v1/start",
            start.clone(),
            None,
            None,
            Some("cached")
        )
        .await
        .0,
        StatusCode::OK
    );
    grant(&app, &[&a], &[&a], &[&a], Some(&a)).await;
    assert_eq!(
        call(
            &app,
            TOKEN,
            "POST",
            "/v1/start",
            start,
            None,
            None,
            Some("cached")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
#[test]
fn registry_rejects_invalid_scopes_and_misplaced_control_records() {
    assert!(document::path_kind("namespaces/private/control/agents/alice.md").is_err());
    let docs=Documents::from([("control/agents/alice.md".into(),document::render(json!({"schema_version":1,"id":"alice","name":"Alice","read_namespaces":["missing"],"write_namespaces":[],"default_read_namespaces":["missing"],"default_write_namespace":null}).as_object().unwrap(),"# Policy").unwrap())]);
    assert!(document::validate_all(&docs).is_err());
    let dir = tempfile::tempdir().unwrap();
    assert!(
        api::router_with_admin(
            Store::open(dir.path()).unwrap(),
            principals(),
            Some(TOKEN.into())
        )
        .is_err()
    );
}
