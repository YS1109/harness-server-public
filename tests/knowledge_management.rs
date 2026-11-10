#![cfg(feature = "service")]

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use harness_server::{
    api,
    document::{self, Documents},
    store::Store,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "fixture-agent-000000000000000000000000";
const ADMIN: &str = "fixture-admin-000000000000000000000000";
fn card(kind: &str, id: &str, body: &str) -> String {
    let key = if kind == "project" {
        "project_id"
    } else {
        "topic_id"
    };
    let mut m = json!({"schema_version":1,"memory_type":kind,"title":id,"status":"ACTIVE","verified_at":"unknown"});
    m[key] = json!(id);
    if kind == "project_topic" {
        m["project_id"] = json!("demo");
        m["summary"] = json!("Stable fixture");
        m["triggers"] = json!([id]);
    }
    document::render(m.as_object().unwrap(), body).unwrap()
}
fn fixture() -> Documents {
    Documents::from([
        (
            "memory/project/demo.md".into(),
            card("project", "demo", "# Demo"),
        ),
        (
            "memory/project/demo/client-rules.md".into(),
            card("project_topic", "client-rules", "# Client")
                .replace("- client-rules", "- 客户端规则"),
        ),
        (
            "memory/project/demo/http-service.md".into(),
            card("project_topic", "http-service", "# HTTP").replace("- http-service", "- HTTP服务"),
        ),
        (
            "memory/templates/release.md".into(),
            "# Release\n## Rollout\n| Stage | Owner |\n| --- | --- |\n- [ ] Confirm\n".into(),
        ),
        (
            "namespaces/device/memory/templates/release.md".into(),
            "# Device release\n".into(),
        ),
    ])
}
fn setup() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.import(fixture()).unwrap();
    let app = api::router_with_admin(
        store,
        harness_server::auth::legacy(vec![TOKEN.into()]),
        Some(ADMIN.into()),
    )
    .unwrap();
    (dir, app)
}
async fn call(
    app: &Router,
    admin: bool,
    method: &str,
    path: &str,
    value: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(
            "Authorization",
            format!("Bearer {}", if admin { ADMIN } else { TOKEN }),
        )
        .header("Content-Type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(value.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn get(app: &Router, path: &str) -> Value {
    let (status, result) = call(
        app,
        true,
        "GET",
        &format!("/admin/api/document?namespace=shared&path={path}"),
        Value::Null,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result
}
fn change(v: &Value, content: Value) -> Value {
    json!({"namespace":"shared","path":v["path"],"expected_sha256":v["sha256"],"action":"edit","content":content})
}

#[tokio::test]
async fn unified_list_is_bounded_and_keeps_duplicate_paths_in_distinct_spaces() {
    let (_dir, app) = setup();
    let (s, v) = call(
        &app,
        true,
        "GET",
        "/admin/api/documents?kind=template&limit=1",
        Value::Null,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 2);
    assert_eq!(v["remaining"], 1);
    let other = call(
        &app,
        true,
        "GET",
        "/admin/api/documents?kind=template&limit=1&offset=1",
        Value::Null,
        &[],
    )
    .await
    .1;
    assert_ne!(
        v["documents"][0]["namespace"],
        other["documents"][0]["namespace"]
    );
    assert_eq!(v["documents"][0]["path"], other["documents"][0]["path"]);
    assert!(v["documents"][0].get("content").is_none());
    assert_eq!(
        call(&app, false, "GET", "/admin/api/documents", Value::Null, &[])
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            true,
            "GET",
            "/admin/api/documents?limit=51",
            Value::Null,
            &[]
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn preview_is_disposable_and_save_replays_without_rewriting_or_duplicate_audit() {
    let (_dir, app) = setup();
    let before = get(&app, "memory/project/demo/client-rules.md").await;
    let value = change(
        &before,
        json!(
            before["content"]
                .as_str()
                .unwrap()
                .replace("# Client", "# Client\nUpdated knowledge")
        ),
    );
    let (s, preview) = call(
        &app,
        true,
        "POST",
        "/admin/api/document/preview",
        value.clone(),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{preview}");
    assert_eq!(preview["written"], false);
    assert!(
        preview["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["path"] == "memory/project/demo.md")
    );
    assert_eq!(
        get(&app, "memory/project/demo/client-rules.md").await["sha256"],
        before["sha256"]
    );
    let (s, saved) = call(
        &app,
        true,
        "PUT",
        "/admin/api/document",
        value.clone(),
        &[("Idempotency-Key", "save-doc")],
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    assert_eq!(saved["idempotent"], false);
    let replay = call(
        &app,
        true,
        "PUT",
        "/admin/api/document",
        value.clone(),
        &[("Idempotency-Key", "save-doc")],
    )
    .await
    .1;
    assert_eq!(replay["idempotent"], true);
    assert_eq!(replay["sha256"], saved["sha256"]);
    assert_eq!(
        call(
            &app,
            true,
            "PUT",
            "/admin/api/document",
            value,
            &[("Idempotency-Key", "stale-doc")]
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let audit = call(&app, true, "GET", "/admin/api/overview", Value::Null, &[])
        .await
        .1;
    assert_eq!(
        audit["audit"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["action"] == "document-update")
            .count(),
        1
    );
    let stats = call(
        &app,
        true,
        "GET",
        "/admin/api/runtime?purpose=maintenance",
        Value::Null,
        &[],
    )
    .await
    .1;
    assert_eq!(stats["metrics"]["observed_calls"], 3);
    assert_eq!(stats["metrics"]["retries"], 1);
    assert_eq!(stats["metrics"]["commit_observed_calls"], 1);
}

#[tokio::test]
async fn sensitive_identity_and_over_budget_changes_leave_the_store_unchanged() {
    let (_dir, app) = setup();
    let old = get(&app, "memory/project/demo.md").await;
    for (i, body) in [
        old["content"]
            .as_str()
            .unwrap()
            .replace("verified_at: unknown", "verified_at: 2026-01-01"),
        old["content"]
            .as_str()
            .unwrap()
            .replace("# Demo", &"x".repeat(2001)),
        old["content"].as_str().unwrap().replace(
            "# Demo",
            &format!("Authorization: Bearer {}", "a".repeat(40)),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (s, result) = call(
            &app,
            true,
            "PUT",
            "/admin/api/document",
            change(&old, json!(body)),
            &[("Idempotency-Key", &format!("invalid-{i}"))],
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{result}");
        assert_eq!(
            get(&app, "memory/project/demo.md").await["sha256"],
            old["sha256"]
        );
    }
    let mut bad = change(&old, old["content"].clone());
    bad["path"] = json!("control/agents/agent.md");
    assert_eq!(
        call(&app, true, "POST", "/admin/api/document/preview", bad, &[])
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn archive_restore_changes_routing_without_changing_identity_or_verification() {
    let (_dir, app) = setup();
    let path = "memory/project/demo/client-rules.md";
    let before = get(&app, path).await;
    let archive = json!({"namespace":"shared","path":path,"expected_sha256":before["sha256"],"action":"archive"});
    let (s, archived) = call(
        &app,
        true,
        "PUT",
        "/admin/api/document",
        archive,
        &[("Idempotency-Key", "archive")],
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{archived}");
    let route = call(
        &app,
        false,
        "POST",
        "/v1/route",
        json!({"project":"demo","scenario":"客户端规则"}),
        &[],
    )
    .await
    .1;
    assert!(route["project_topics"].as_array().unwrap().is_empty());
    let restore = json!({"namespace":"shared","path":path,"expected_sha256":archived["sha256"],"action":"restore"});
    assert_eq!(
        call(
            &app,
            true,
            "PUT",
            "/admin/api/document",
            restore,
            &[("Idempotency-Key", "restore")]
        )
        .await
        .0,
        StatusCode::OK
    );
    let after = get(&app, path).await;
    assert_eq!(after["verified_at"], before["verified_at"]);
    assert_eq!(after["status"], "ACTIVE");
    let template = get(&app, "memory/templates/release.md").await;
    let unsupported = json!({"namespace":"shared","path":template["path"],"expected_sha256":template["sha256"],"action":"archive"});
    assert_eq!(
        call(
            &app,
            true,
            "PUT",
            "/admin/api/document",
            unsupported,
            &[("Idempotency-Key", "archive-template")]
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn route_diagnosis_shares_engine_and_does_not_write_or_count_admin_diagnostic_reads() {
    let (dir, app) = setup();
    let head = std::fs::read(dir.path().join("HEAD")).unwrap();
    let query = json!({"read_namespaces":["shared"],"project":"demo","scenario":"优化客户端规则，HTTP服务只是背景"});
    let (s, r) = call(&app, true, "POST", "/admin/api/route", query, &[]).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(
        r["project_topics"][0],
        "memory/project/demo/client-rules.md"
    );
    assert_eq!(r["explanation"]["ignored_background_clauses"], 1);
    assert_eq!(r["explanation"]["candidates"][0]["namespace"], "shared");
    assert!(r.get("content").is_none());
    assert_eq!(std::fs::read(dir.path().join("HEAD")).unwrap(), head);
    let stats = call(&app, false, "GET", "/v1/stats", Value::Null, &[])
        .await
        .1;
    assert_eq!(stats["metrics"]["observed_calls"], 0);
    let invalid = json!({"read_namespaces":["missing"],"project":"demo","scenario":"x"});
    assert!(
        !call(&app, true, "POST", "/admin/api/route", invalid, &[])
            .await
            .0
            .is_success()
    );
}

#[tokio::test]
async fn purposes_partition_calls_and_unlabelled_history_is_not_inferred() {
    let (_dir, app) = setup();
    for purpose in [None, Some("task"), Some("maintenance"), Some("diagnostic")] {
        let headers: Vec<_> = purpose
            .map(|p| vec![("X-Harness-Purpose", p)])
            .unwrap_or_default();
        assert_eq!(
            call(
                &app,
                false,
                "GET",
                "/v1/documents?path=memory/project/demo.md",
                Value::Null,
                &headers
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let stats = call(&app, false, "GET", "/v1/stats", Value::Null, &[])
        .await
        .1;
    assert_eq!(stats["metrics"]["observed_calls"], 4);
    for purpose in ["unlabelled", "task", "maintenance", "diagnostic"] {
        assert_eq!(stats["purpose_metrics"][purpose]["observed_calls"], 1);
        let filtered = call(
            &app,
            false,
            "GET",
            &format!("/v1/stats?purpose={purpose}"),
            Value::Null,
            &[],
        )
        .await
        .1;
        assert_eq!(filtered["metrics"]["observed_calls"], 1);
    }
    assert_eq!(
        call(
            &app,
            false,
            "GET",
            "/v1/stats?purpose=guessed",
            Value::Null,
            &[]
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn template_checks_are_readonly_and_keep_exact_origin() {
    let (dir, app) = setup();
    let head = std::fs::read(dir.path().join("HEAD")).unwrap();
    let doc = "# Release\n## Rollout\n| Stage | Owner |\n| --- | --- |\n- [x] Confirmed\n";
    let (s, r) = call(
        &app,
        false,
        "POST",
        "/v1/check-template",
        json!({"path":"memory/templates/release.md","document":doc}),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["passed"], true);
    assert_eq!(r["template"]["namespace"], "shared");
    let r=call(&app,true,"POST","/admin/api/check-template",json!({"namespace":"device","path":"memory/templates/release.md","document":"# Device release"}),&[]).await.1;
    assert_eq!(r["passed"], true);
    assert_eq!(r["namespace"], "device");
    assert_eq!(std::fs::read(dir.path().join("HEAD")).unwrap(), head);
    assert_eq!(
        call(&app, false, "GET", "/v1/stats", Value::Null, &[])
            .await
            .1["metrics"]["observed_calls"],
        0
    );
}

#[tokio::test]
async fn reindexed_parent_overflow_rejects_preview_and_save_without_persisting() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = harness_server::store::State {
        documents: fixture(),
        ..Default::default()
    };
    let path = "memory/project/demo.md";
    let original_sha = document::sha(&state.documents[path]);
    harness_server::namespace::transact(&mut state, "shared", false, None, |local| {
        harness_server::engine::put(
            local,
            &json!({
                "path":path,
                "expected_sha256":original_sha,
                "content":card("project","demo",&"x".repeat(1750))
            }),
        )
    })
    .unwrap();
    document::validate_all(&state.documents).unwrap();
    let parent_body = document::parse(&state.documents[path]).unwrap().1;
    assert!((1850..2000).contains(&parent_body.chars().count()));
    let mut store = Store::open(dir.path()).unwrap();
    store.import(state.documents).unwrap();
    let app = api::router_with_admin(
        store,
        harness_server::auth::legacy(vec![TOKEN.into()]),
        Some(ADMIN.into()),
    )
    .unwrap();
    let before = get(&app, "memory/project/demo/client-rules.md").await;
    let head = std::fs::read(dir.path().join("HEAD")).unwrap();
    let value = change(
        &before,
        json!(
            before["content"]
                .as_str()
                .unwrap()
                .replace("Stable fixture", &"长".repeat(160))
        ),
    );
    for (method, path, headers) in [
        ("POST", "/admin/api/document/preview", vec![]),
        (
            "PUT",
            "/admin/api/document",
            vec![("Idempotency-Key", "overflow-parent")],
        ),
    ] {
        let (status, result) = call(&app, true, method, path, value.clone(), &headers).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
        assert_eq!(std::fs::read(dir.path().join("HEAD")).unwrap(), head);
        assert_eq!(
            get(&app, "memory/project/demo/client-rules.md").await["sha256"],
            before["sha256"]
        );
    }
}

#[tokio::test]
async fn document_mutations_require_admin_same_origin_and_an_existing_source() {
    let (dir, app) = setup();
    let before = get(&app, "memory/project/demo.md").await;
    let value = change(&before, before["content"].clone());
    let head = std::fs::read(dir.path().join("HEAD")).unwrap();
    for path in ["/admin/api/document/preview", "/admin/api/document"] {
        let method = if path.ends_with("preview") {
            "POST"
        } else {
            "PUT"
        };
        assert_eq!(
            call(
                &app,
                false,
                method,
                path,
                value.clone(),
                &[("Idempotency-Key", "agent-denied")]
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(
                &app,
                true,
                method,
                path,
                value.clone(),
                &[
                    ("Host", "harness.example"),
                    ("Origin", "https://other.example"),
                    ("Idempotency-Key", "origin-denied")
                ]
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let mut inherited = value.clone();
        inherited["namespace"] = json!("device");
        assert_eq!(
            call(
                &app,
                true,
                method,
                path,
                inherited,
                &[("Idempotency-Key", "source-denied")]
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(std::fs::read(dir.path().join("HEAD")).unwrap(), head);
}
