#![cfg(feature = "service")]

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use harness_server::{
    admin_session::{LoginConfig, hash_password},
    api,
    auth::Principal,
    document::{self, Documents},
    store::Store,
    tenancy::{self, Tenant},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tower::ServiceExt;

const ALPHA: &str = "tenant-alpha-agent-000000000000000000000";
const BETA: &str = "tenant-beta-agent-0000000000000000000000";
const ADMIN_ALPHA: &str = "tenant-alpha-admin-000000000000000000000";
const ADMIN_BETA: &str = "tenant-beta-admin-0000000000000000000000";

fn tenants() -> Vec<Tenant> {
    [("alpha", ALPHA, ADMIN_ALPHA), ("beta", BETA, ADMIN_BETA)]
        .into_iter()
        .map(|(id, token, admin)| Tenant {
            id: id.into(),
            principals: vec![Principal {
                agent_id: Some("alice".into()),
                token: token.into(),
                read_namespaces: vec!["workspace".into(), "shared".into()],
                write_namespaces: vec!["workspace".into(), "shared".into()],
            }],
            admin_token: admin.into(),
            admin_login: None,
            secure_cookie: true,
        })
        .collect()
}

fn project(body: &str) -> String {
    document::render(
        json!({"schema_version":1,"memory_type":"project","project_id":"demo","title":"Demo","status":"ACTIVE","verified_at":"unknown"})
            .as_object().unwrap(),
        body,
    ).unwrap()
}

fn process(id: &str, body: &str) -> String {
    document::render(
        json!({"schema_version":1,"memory_type":"process","process_id":id,"title":"Coding","status":"ACTIVE","scope":"global","verified_at":"unknown","triggers":["coding"]})
            .as_object().unwrap(),
        body,
    ).unwrap()
}

fn seed(root: &Path) {
    for id in ["alpha", "beta"] {
        let mut store = Store::open(root.join("tenants").join(id)).unwrap();
        let mut docs = Documents::new();
        docs.insert(
            "memory/project/demo.md".into(),
            project(&format!("# {id} shared project")),
        );
        docs.insert(
            "memory/process/coding.md".into(),
            process("coding", &format!("# {id} shared coding")),
        );
        docs.insert(
            "namespaces/workspace/memory/project/demo.md".into(),
            project(&format!("# {id} workspace project")),
        );
        store.import(docs).unwrap();
    }
}

fn root(dir: &tempfile::TempDir) -> PathBuf {
    // macOS temporary directory prefixes can themselves be symbolic links.
    dir.path().canonicalize().unwrap().join("data")
}

fn setup() -> (tempfile::TempDir, PathBuf, Router) {
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    seed(&root);
    let app = tenancy::router_with_tenants(&root, tenants()).unwrap();
    (dir, root, app)
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

async fn response(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 3 * 1024 * 1024)
        .await
        .unwrap();
    let body =
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
    Reply {
        status,
        headers,
        body,
    }
}

#[allow(clippy::too_many_arguments)]
async fn call(
    app: &Router,
    tenant: Option<&str>,
    token: Option<&str>,
    method: &str,
    path: &str,
    body: Value,
    key: Option<&str>,
) -> Reply {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("Content-Type", "application/json")
        .header("X-Harness-Namespace", "workspace")
        .header("X-Harness-Agent-Id", "alice");
    if let Some(tenant) = tenant {
        request = request.header("X-Harness-Tenant", tenant);
    }
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    response(app, request.body(Body::from(body.to_string())).unwrap()).await
}

fn start(tenant: &str) -> Value {
    json!({"agent":"codex","native_session_id":"same-session","project":"demo","workstream_id":"same-workstream","title":tenant,"scenario":"coding","content":true})
}

#[tokio::test]
async fn same_identifiers_documents_shared_memory_and_receipts_are_tenant_local() {
    let (_dir, root, app) = setup();
    let mut sessions = Vec::new();
    for (tenant, token, other) in [("alpha", ALPHA, "beta"), ("beta", BETA, "alpha")] {
        let started = call(
            &app,
            Some(tenant),
            Some(token),
            "POST",
            "/v3/tenant/v2/scoped/start",
            start(tenant),
            Some("same-start-key"),
        )
        .await;
        assert_eq!(started.status, StatusCode::OK, "{}", started.body);
        assert_eq!(started.headers["x-harness-tenant"], tenant);
        assert_eq!(started.body["idempotent"], false);
        assert_eq!(started.body["identity"]["namespace"], "workspace");
        assert_eq!(started.body["identity"]["agent_id"], "alice");
        assert!(
            started
                .body
                .to_string()
                .contains(&format!("{tenant} shared coding"))
        );
        assert!(
            !started
                .body
                .to_string()
                .contains(&format!("{other} shared coding"))
        );
        sessions.push(started.body.clone());
        let content = process("notes", &format!("# {tenant} private notes"));
        let put = call(
            &app,
            Some(tenant),
            Some(token),
            "PUT",
            "/v3/tenant/v1/documents",
            json!({"path":"memory/process/notes.md","content":content}),
            Some("same-document-key"),
        )
        .await;
        assert_eq!(put.status, StatusCode::OK, "{}", put.body);
        let read = call(
            &app,
            Some(tenant),
            Some(token),
            "GET",
            "/v1/documents?path=memory/process/notes.md",
            Value::Null,
            None,
        )
        .await;
        assert_eq!(read.status, StatusCode::OK);
        assert_eq!(read.headers["x-harness-tenant"], tenant);
        assert!(
            read.body["content"]
                .as_str()
                .unwrap()
                .contains(&format!("{tenant} private notes"))
        );
        assert!(
            !read
                .body
                .to_string()
                .contains(&format!("{other} private notes"))
        );
    }
    assert_eq!(sessions[0]["session_id"], sessions[1]["session_id"]);
    assert_ne!(
        sessions[0]["execution_group_id"],
        sessions[1]["execution_group_id"]
    );
    drop(app);
    let app = tenancy::router_with_tenants(&root, tenants()).unwrap();
    for ((tenant, token), first) in [("alpha", ALPHA), ("beta", BETA)].into_iter().zip(sessions) {
        let retry = call(
            &app,
            Some(tenant),
            Some(token),
            "POST",
            "/v3/tenant/v2/scoped/start",
            start(tenant),
            Some("same-start-key"),
        )
        .await;
        assert_eq!(retry.status, StatusCode::OK, "{}", retry.body);
        assert_eq!(retry.body["idempotent"], true);
        assert_eq!(
            retry.body["execution_group_id"],
            first["execution_group_id"]
        );
        let listed = call(
            &app,
            Some(tenant),
            Some(token),
            "GET",
            "/v1/documents?kind=session",
            Value::Null,
            None,
        )
        .await;
        assert_eq!(listed.body["total"], 1);
    }
}

#[tokio::test]
async fn selector_never_authorizes_cross_tenant_agent_or_admin_credentials() {
    let (_dir, _root, app) = setup();
    for (tenant, wrong_agent, wrong_admin) in
        [("alpha", BETA, ADMIN_BETA), ("beta", ALPHA, ADMIN_ALPHA)]
    {
        for path in [
            "/v1/health",
            "/v1/stats",
            "/v1/documents?path=memory/project/demo.md",
        ] {
            let denied = call(
                &app,
                Some(tenant),
                Some(wrong_agent),
                "GET",
                path,
                Value::Null,
                None,
            )
            .await;
            assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
            assert_eq!(denied.headers["x-harness-tenant"], tenant);
        }
        for path in ["/v1/start", "/v3/tenant/v2/scoped/start"] {
            assert_eq!(
                call(
                    &app,
                    Some(tenant),
                    Some(wrong_agent),
                    "POST",
                    path,
                    start(tenant),
                    Some("denied-start")
                )
                .await
                .status,
                StatusCode::UNAUTHORIZED,
            );
        }
        assert_eq!(
            call(
                &app,
                Some(tenant),
                Some(wrong_agent),
                "PUT",
                "/v3/tenant/v1/documents",
                json!({"path":"memory/process/notes.md","content":process("notes","# Rejected")}),
                Some("denied-put")
            )
            .await
            .status,
            StatusCode::UNAUTHORIZED,
        );
        for credential in [wrong_agent, wrong_admin] {
            assert_eq!(
                call(
                    &app,
                    Some(tenant),
                    Some(credential),
                    "GET",
                    "/admin/api/overview",
                    Value::Null,
                    None
                )
                .await
                .status,
                StatusCode::UNAUTHORIZED,
            );
            assert_eq!(
                call(
                    &app,
                    Some(tenant),
                    Some(credential),
                    "POST",
                    "/admin/api/namespaces",
                    json!({"name":"Denied","description":""}),
                    Some("denied-admin")
                )
                .await
                .status,
                StatusCode::UNAUTHORIZED,
            );
        }
        let token = if tenant == "alpha" { ALPHA } else { BETA };
        let documents = call(
            &app,
            Some(tenant),
            Some(token),
            "GET",
            "/v1/documents?kind=session",
            Value::Null,
            None,
        )
        .await;
        assert_eq!(documents.body["total"], 0);
    }
    assert_eq!(
        call(
            &app,
            Some("alpha"),
            Some(ADMIN_ALPHA),
            "GET",
            "/v1/health",
            Value::Null,
            None
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED,
    );
    assert_eq!(
        call(
            &app,
            Some("alpha"),
            Some(ALPHA),
            "GET",
            "/admin/api/overview",
            Value::Null,
            None
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED,
    );
}

#[tokio::test]
async fn missing_unknown_malformed_and_repeated_selectors_never_fall_back() {
    let (_dir, _root, app) = setup();
    for path in [
        "/v1/health",
        "/v2/scoped/start",
        "/v3/tenant/v2/scoped/start",
        "/admin/api/overview",
    ] {
        let missing = call(
            &app,
            None,
            Some(ALPHA),
            "POST",
            path,
            start("alpha"),
            Some("no-selector"),
        )
        .await;
        let unknown = call(
            &app,
            Some("unlisted"),
            Some(ALPHA),
            "POST",
            path,
            start("alpha"),
            Some("no-selector"),
        )
        .await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        assert_eq!(unknown.status, missing.status);
        assert_eq!(unknown.body, missing.body);
        assert!(!unknown.headers.contains_key("x-harness-tenant"));
        for selector in [
            "Alpha",
            "../alpha",
            "alpha/beta",
            "",
            "alpha,beta",
            " alpha",
        ] {
            let invalid = call(
                &app,
                Some(selector),
                Some(ALPHA),
                "POST",
                path,
                start("alpha"),
                Some("invalid-selector"),
            )
            .await;
            assert_eq!(invalid.status, StatusCode::BAD_REQUEST, "{selector:?}");
            assert!(!invalid.headers.contains_key("x-harness-tenant"));
        }
    }
    let duplicate = Request::builder()
        .uri("/v1/health")
        .header("X-Harness-Tenant", "alpha")
        .header("X-Harness-Tenant", "beta")
        .header("Authorization", format!("Bearer {ALPHA}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        response(&app, duplicate).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn paths_queries_and_documents_cannot_override_the_header() {
    let (_dir, _root, app) = setup();
    for path in [
        "/v1/health?tenant=beta",
        "/v1/health?tenant_id=beta",
        "/v1/health?%74enant=beta",
        "/v1/health?X-Harness-Tenant=beta",
        "/v1/documents?path=tenants%2Fbeta%2Fmemory%2Fproject%2Fdemo.md",
    ] {
        assert_eq!(
            call(
                &app,
                Some("alpha"),
                Some(ALPHA),
                "GET",
                path,
                Value::Null,
                None
            )
            .await
            .status,
            StatusCode::BAD_REQUEST,
            "{path}",
        );
    }
    for body in [
        json!({"tenant":"beta"}),
        json!({"tenant_id":"beta"}),
        json!({"path":"tenants/beta/memory/project/demo.md","content":project("# Rejected")}),
    ] {
        assert_eq!(
            call(
                &app,
                Some("alpha"),
                Some(ALPHA),
                "PUT",
                "/v3/tenant/v1/documents",
                body,
                Some("override")
            )
            .await
            .status,
            StatusCode::BAD_REQUEST,
        );
    }
    for path in [
        "/tenants/beta/v1/health",
        "/v1/tenants/beta/health",
        "/v3/tenant/admin/api/namespaces",
        "/v3/tenant/admin/console.js",
    ] {
        assert_eq!(
            call(
                &app,
                Some("alpha"),
                Some(ALPHA),
                "POST",
                path,
                json!({}),
                Some("path-override")
            )
            .await
            .status,
            StatusCode::NOT_FOUND,
        );
    }
}

#[tokio::test]
async fn tenant_fields_inside_stored_documents_are_data_and_do_not_select_another_store() {
    let (_dir, _root, app) = setup();
    let mut content = process(
        "tenant-schema",
        "# Tenant schema\n\n```json\n{\"tenant_id\":\"beta\",\"tenant\":\"example\"}\n```",
    );
    content = content.replacen("---\n", "---\ntenant: beta\ntenant_id: beta\n", 1);
    let write = call(
        &app,
        Some("alpha"),
        Some(ALPHA),
        "PUT",
        "/v3/tenant/v1/documents",
        json!({"path":"memory/process/tenant-schema.md","content":content}),
        Some("tenant-fields-as-data"),
    )
    .await;
    assert_eq!(write.status, StatusCode::OK, "{}", write.body);
    assert_eq!(write.headers["x-harness-tenant"], "alpha");
    let alpha = call(
        &app,
        Some("alpha"),
        Some(ALPHA),
        "GET",
        "/v1/documents?path=memory/process/tenant-schema.md",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(alpha.status, StatusCode::OK, "{}", alpha.body);
    assert!(
        alpha.body["content"]
            .as_str()
            .unwrap()
            .contains("tenant_id: beta")
    );
    let beta = call(
        &app,
        Some("beta"),
        Some(BETA),
        "GET",
        "/v1/documents?path=memory/process/tenant-schema.md",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(beta.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stats_evaluations_and_control_documents_do_not_cross_tenants() {
    let (_dir, _root, app) = setup();
    for _ in 0..2 {
        let routed = call(
            &app,
            Some("alpha"),
            Some(ALPHA),
            "POST",
            "/v1/route",
            json!({"project":"demo","scenario":"coding","content":true}),
            None,
        )
        .await;
        assert_eq!(routed.status, StatusCode::OK, "{}", routed.body);
    }
    for (tenant, token, observed) in [("alpha", ALPHA, 2), ("beta", BETA, 0)] {
        let stats = call(
            &app,
            Some(tenant),
            Some(token),
            "GET",
            "/v1/stats",
            Value::Null,
            None,
        )
        .await;
        assert_eq!(stats.status, StatusCode::OK);
        assert_eq!(stats.body["metrics"]["observed_calls"], observed);
    }
    let suite = json!({"version":"alpha-only-evaluation","cases":[{"id":"same-case","project":"demo","query":"coding","expected_topic":null}]});
    let evaluation = call(
        &app,
        Some("alpha"),
        Some(ALPHA),
        "POST",
        "/v1/eval-routing",
        suite,
        None,
    )
    .await;
    assert_eq!(evaluation.status, StatusCode::OK, "{}", evaluation.body);
    for (tenant, admin) in [("alpha", ADMIN_ALPHA), ("beta", ADMIN_BETA)] {
        let report = call(
            &app,
            Some(tenant),
            Some(admin),
            "GET",
            "/admin/api/routing-evaluation",
            Value::Null,
            None,
        )
        .await;
        assert_eq!(report.status, StatusCode::OK, "{}", report.body);
        assert_eq!(
            report.body.to_string().contains("alpha-only-evaluation"),
            tenant == "alpha"
        );
    }
    let before = call(
        &app,
        Some("beta"),
        Some(ADMIN_BETA),
        "GET",
        "/admin/api/overview",
        Value::Null,
        None,
    )
    .await
    .body;
    let created = call(
        &app,
        Some("alpha"),
        Some(ADMIN_ALPHA),
        "POST",
        "/admin/api/namespaces",
        json!({"name":"Alpha-only namespace","description":""}),
        Some("same-admin-key"),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    let beta = call(
        &app,
        Some("beta"),
        Some(ADMIN_BETA),
        "GET",
        "/admin/api/overview",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(beta.body, before);
    assert!(!beta.body.to_string().contains("Alpha-only namespace"));
    let created_beta = call(
        &app,
        Some("beta"),
        Some(ADMIN_BETA),
        "POST",
        "/admin/api/namespaces",
        json!({"name":"Beta-only namespace","description":""}),
        Some("same-admin-key"),
    )
    .await;
    assert_eq!(created_beta.status, StatusCode::OK, "{}", created_beta.body);
    assert_ne!(
        created.body["namespace"]["id"],
        created_beta.body["namespace"]["id"]
    );
}

#[tokio::test]
async fn health_and_fixed_admin_assets_are_public_and_do_not_list_tenants_or_redirect() {
    let (_dir, _root, app) = setup();
    let health = call(&app, None, None, "GET", "/healthz", Value::Null, None).await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.body, json!({"status":"ok"}));
    assert!(!health.headers.contains_key("x-harness-tenant"));
    for path in [
        "/admin",
        "/admin/",
        "/admin/console.js",
        "/admin/console.css",
    ] {
        let asset = call(&app, None, None, "GET", path, Value::Null, None).await;
        assert_eq!(asset.status, StatusCode::OK, "{path}");
        assert!(!asset.headers.contains_key("location"));
        assert_eq!(asset.headers["cache-control"], "no-store");
        assert_eq!(asset.headers["x-content-type-options"], "nosniff");
    }
    for path in [
        "/",
        "/tenants",
        "/admin/unknown",
        "//external.example/admin",
    ] {
        assert_eq!(
            call(&app, None, None, "GET", path, Value::Null, None)
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn guarded_writes_and_consumed_selectors_preserve_legacy_safety() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(root(&dir)).unwrap();
    store
        .import(Documents::from([(
            "memory/project/demo.md".into(),
            project("# Legacy"),
        )]))
        .unwrap();
    let legacy = api::router(store, vec![ALPHA.into()]).unwrap();
    let head = fs::read(root(&dir).join("HEAD")).unwrap();
    let guarded = call(
        &legacy,
        Some("alpha"),
        Some(ALPHA),
        "POST",
        "/v3/tenant/v1/start",
        start("alpha"),
        Some("legacy-denied"),
    )
    .await;
    assert!(matches!(
        guarded.status,
        StatusCode::NOT_FOUND | StatusCode::FORBIDDEN
    ));
    let direct = call(
        &legacy,
        Some("alpha"),
        Some(ALPHA),
        "POST",
        "/v1/start",
        start("alpha"),
        Some("legacy-denied"),
    )
    .await;
    assert_eq!(direct.status, StatusCode::FORBIDDEN);
    let old_server_route = Request::builder()
        .method("POST")
        .uri("/v3/tenant/v1/start")
        .header("Authorization", format!("Bearer {ALPHA}"))
        .header("Content-Type", "application/json")
        .header("Idempotency-Key", "old-server-guard")
        .body(Body::from(start("legacy").to_string()))
        .unwrap();
    assert_eq!(
        response(&legacy, old_server_route).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(fs::read(root(&dir).join("HEAD")).unwrap(), head);
    let direct = Request::builder()
        .method("POST")
        .uri("/v1/start")
        .header("Authorization", format!("Bearer {ALPHA}"))
        .header("Content-Type", "application/json")
        .header("Idempotency-Key", "legacy-ok")
        .body(Body::from(start("legacy").to_string()))
        .unwrap();
    let allowed = response(&legacy, direct).await;
    assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.body);
    assert!(!allowed.headers.contains_key("x-harness-tenant"));
    let (_dir, _root, gateway) = setup();
    let consumed = call(
        &gateway,
        Some("alpha"),
        Some(ALPHA),
        "GET",
        "/v1/health",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(consumed.status, StatusCode::OK);
    assert_eq!(consumed.headers["x-harness-tenant"], "alpha");
    assert_eq!(
        call(
            &gateway,
            Some("alpha"),
            Some(ALPHA),
            "GET",
            "/v3/tenant/v1/health",
            Value::Null,
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND,
    );
}

#[tokio::test]
async fn token_only_tenants_apply_their_own_cookie_transport_policy() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = tenants();
    config[0].secure_cookie = false;
    let app = tenancy::router_with_tenants(&root(&dir), config).unwrap();
    for (tenant, token, origin, secure) in [
        ("alpha", ADMIN_ALPHA, "http://localhost", false),
        ("beta", ADMIN_BETA, "https://localhost", true),
    ] {
        let request = Request::builder()
            .method("POST")
            .uri("/admin/api/login")
            .header("X-Harness-Tenant", tenant)
            .header("Host", "localhost")
            .header("Origin", origin)
            .header("X-Harness-Admin-Request", "1")
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .body(Body::from(json!({"remember":true}).to_string()))
            .unwrap();
        let login = response(&app, request).await;
        assert_eq!(login.status, StatusCode::OK, "{}", login.body);
        assert_eq!(
            login.headers["set-cookie"]
                .to_str()
                .unwrap()
                .contains("; Secure"),
            secure,
        );
    }
    let downgraded = Request::builder()
        .method("POST")
        .uri("/admin/api/login")
        .header("X-Harness-Tenant", "beta")
        .header("Host", "localhost")
        .header("Origin", "http://localhost")
        .header("X-Harness-Admin-Request", "1")
        .header("Authorization", format!("Bearer {ADMIN_BETA}"))
        .header("Content-Type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(
        response(&app, downgraded).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn browser_session_cookies_belong_only_to_the_selected_tenant() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = tenants();
    for (tenant, password) in config
        .iter_mut()
        .zip(["alpha-admin-password", "beta-admin-password"])
    {
        tenant.admin_login = Some(LoginConfig {
            username: "administrator".into(),
            password_hash: hash_password(password).unwrap(),
            secure_cookie: false,
        });
        tenant.secure_cookie = false;
    }
    let app = tenancy::router_with_tenants(&root(&dir), config).unwrap();
    let login = Request::builder()
        .method("POST")
        .uri("/admin/api/login")
        .header("X-Harness-Tenant", "alpha")
        .header("Host", "localhost")
        .header("Origin", "http://localhost")
        .header("X-Harness-Admin-Request", "1")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({"username":"administrator","password":"alpha-admin-password","remember":false})
                .to_string(),
        ))
        .unwrap();
    let login = response(&app, login).await;
    assert_eq!(login.status, StatusCode::OK, "{}", login.body);
    let cookie = login.headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    for (tenant, expected) in [
        ("alpha", StatusCode::OK),
        ("beta", StatusCode::UNAUTHORIZED),
    ] {
        let request = Request::builder()
            .uri("/admin/api/overview")
            .header("X-Harness-Tenant", tenant)
            .header("Cookie", cookie)
            .body(Body::empty())
            .unwrap();
        let read = response(&app, request).await;
        assert_eq!(read.status, expected);
        assert_eq!(read.headers["x-harness-tenant"], tenant);
    }
}

#[test]
fn duplicate_ids_all_token_roles_and_admin_hashes_fail_before_creating_storage() {
    for mutation in 0..5 {
        let dir = tempfile::tempdir().unwrap();
        let root = root(&dir);
        let mut config = tenants();
        match mutation {
            0 => config[1].id = config[0].id.clone(),
            1 => config[1].principals[0].token = ALPHA.into(),
            2 => config[1].admin_token = ADMIN_ALPHA.into(),
            3 => config[1].admin_token = ALPHA.into(),
            4 => config[0].admin_token = ALPHA.into(),
            _ => unreachable!(),
        }
        let error = tenancy::router_with_tenants(&root, config)
            .err()
            .unwrap()
            .to_string();
        assert!(!error.contains(ALPHA));
        assert!(!error.contains(ADMIN_ALPHA));
        assert!(!root.exists());
    }
    let dir = tempfile::tempdir().unwrap();
    let mut config = tenants();
    let hash = hash_password("never-reuse-admin-password").unwrap();
    for tenant in &mut config {
        tenant.admin_login = Some(LoginConfig {
            username: "administrator".into(),
            password_hash: hash.clone(),
            secure_cookie: true,
        });
    }
    assert!(tenancy::router_with_tenants(&root(&dir), config).is_err());
    assert!(!root(&dir).exists());
}

#[test]
fn all_store_locks_release_on_failure_and_mode_marker_prevents_legacy_open() {
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    let held = Store::open(root.join("tenants/beta")).unwrap();
    assert!(tenancy::router_with_tenants(&root, tenants()).is_err());
    Store::open(root.join("tenants/alpha"))
        .expect("failed tenant startup must release previously acquired Store locks");
    assert!(!root.join("tenants/alpha/HEAD").exists());
    drop(held);
    let gateway = tenancy::router_with_tenants(&root, tenants()).unwrap();
    assert!(tenancy::router_with_tenants(&root, tenants()).is_err());
    assert!(tenancy::ensure_single_tenant(&root).is_err());
    drop(gateway);
    assert!(Store::open(root.join("tenants/alpha")).is_ok());
    assert!(Store::open(root.join("tenants/beta")).is_ok());
    let app = tenancy::router_with_tenants(&root, tenants()).unwrap();
    drop(app);
}

#[test]
fn existing_single_tenant_data_requires_explicit_migration() {
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    let store = Store::open(&root).unwrap();
    drop(store);
    assert!(tenancy::router_with_tenants(&root, tenants()).is_err());
    assert!(!root.join("tenants").exists());
    assert!(!root.join(".tenancy-mode.json").exists());
    assert!(tenancy::ensure_single_tenant(&root).is_ok());
}

#[cfg(unix)]
#[test]
fn storage_symlinks_in_roots_ancestors_and_tenant_directories_are_rejected() {
    use std::os::unix::fs::symlink;
    for target in ["root", "ancestor", "tenants", "tenant"] {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let real = base.join("real");
        fs::create_dir_all(&real).unwrap();
        let root = match target {
            "root" => {
                symlink(&real, base.join("data")).unwrap();
                base.join("data")
            }
            "ancestor" => {
                symlink(&real, base.join("parent")).unwrap();
                base.join("parent/data")
            }
            "tenants" => {
                fs::create_dir_all(base.join("data")).unwrap();
                symlink(&real, base.join("data/tenants")).unwrap();
                base.join("data")
            }
            "tenant" => {
                fs::create_dir_all(base.join("data/tenants")).unwrap();
                symlink(&real, base.join("data/tenants/alpha")).unwrap();
                base.join("data")
            }
            _ => unreachable!(),
        };
        assert!(
            tenancy::router_with_tenants(&root, tenants()).is_err(),
            "{target}"
        );
        assert!(fs::read_dir(&real).unwrap().next().is_none(), "{target}");
    }
}

fn server_command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_harness-server"));
    command
        .arg("--data-dir")
        .arg(root)
        .env_remove("HARNESS_AUTH_CONFIG")
        .env_remove("HARNESS_TENANTS_CONFIG")
        .env_remove("HARNESS_TOKENS_JSON")
        .env_remove("HARNESS_ADMIN_TOKEN")
        .env_remove("HARNESS_ADMIN_USERNAME")
        .env_remove("HARNESS_ADMIN_PASSWORD_HASH")
        .env_remove("HARNESS_ADMIN_SECURE_COOKIE")
        .env("HARNESS_TOKEN", ALPHA)
        .stdin(Stdio::null());
    command
}

#[test]
fn offline_hashing_and_invalid_single_tenant_environment_have_no_store_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    let output = server_command(&root).arg("hash-password").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!root.exists());
    for (username, hash, secure) in [
        ("administrator", "", "true"),
        ("", "not-a-phc-hash", "true"),
        ("administrator", "not-a-phc-hash", "true"),
        ("", "", "1"),
        ("", "", "TRUE"),
        ("", "", ""),
    ] {
        let output = server_command(&root)
            .arg("serve")
            .arg("--bind")
            .arg("127.0.0.1:0")
            .env("HARNESS_ADMIN_USERNAME", username)
            .env("HARNESS_ADMIN_PASSWORD_HASH", hash)
            .env("HARNESS_ADMIN_SECURE_COOKIE", secure)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!root.exists());
    }
}

#[test]
fn config_rejects_missing_environment_plaintext_and_ambiguous_explicit_auth() {
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    let config = dir.path().join("tenants.json");
    for value in [
        json!({"tenants":[]}),
        json!({"tenants":[{"id":"alpha","token_env":"HARNESS_TEST_MISSING_TENANT_CREDENTIAL","admin_token_env":"HARNESS_TEST_MISSING_TENANT_ADMIN"}]}),
        json!({"tenants":[{"id":"alpha","token":"plaintext-credential","admin_token_env":"ADMIN"}]}),
    ] {
        fs::write(&config, value.to_string()).unwrap();
        let output = server_command(&root)
            .arg("serve")
            .arg("--tenants-config")
            .arg(&config)
            .env_remove("HARNESS_TEST_MISSING_TENANT_CREDENTIAL")
            .env_remove("HARNESS_TEST_MISSING_TENANT_ADMIN")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!root.exists());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("plaintext-credential"));
    }
    let output = server_command(&root)
        .arg("serve")
        .arg("--tenants-config")
        .arg(&config)
        .arg("--auth-config")
        .arg("ambiguous.json")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!root.exists());
}
