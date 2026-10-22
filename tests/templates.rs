use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use harness_server::{
    api,
    auth::Principal,
    backup,
    document::{self, Documents},
    engine, namespace,
    store::{State, Store, import_directory},
};
use serde_json::{Value, json};
use std::{fs, path::Path};
use tower::ServiceExt;

const TOKEN: &str = "template-test-agent-0000000000000000000000";
const OTHER: &str = "template-test-other-0000000000000000000000";
const TEMPLATE: &str = "memory/templates/release-doc-template.md";
fn raw() -> String {
    format!(
        "# 发布文档模板\r\n\r\n{}\r\n\r\n# 八、异常预案\r\n",
        "完整骨架；".repeat(500)
    )
}
fn project() -> String {
    document::render(json!({"schema_version":1,"memory_type":"project","project_id":"demo","title":"Demo","status":"ACTIVE","verified_at":"unknown"}).as_object().unwrap(), "# Demo").unwrap()
}
fn principals() -> Vec<Principal> {
    vec![
        Principal {
            agent_id: Some("owner".into()),
            token: TOKEN.into(),
            read_namespaces: vec!["device".into(), "shared".into()],
            write_namespaces: vec!["device".into(), "shared".into()],
        },
        Principal {
            agent_id: Some("other".into()),
            token: OTHER.into(),
            read_namespaces: vec!["other".into()],
            write_namespaces: vec![],
        },
    ]
}
async fn call(
    app: &Router,
    token: &str,
    ns: &str,
    method: &str,
    path: &str,
    value: Value,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("X-Harness-Namespace", ns)
        .header("Content-Type", "application/json");
    if let Some(key) = key {
        r = r.header("Idempotency-Key", key);
    }
    let response = app
        .clone()
        .oneshot(r.body(Body::from(value.to_string())).unwrap())
        .await
        .unwrap();
    (
        response.status(),
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap(),
    )
}

#[test]
fn raw_templates_have_separate_budgets_and_strict_paths_without_session_identities() {
    let body = raw();
    assert!(body.chars().count() > 2000);
    document::validate(TEMPLATE, &body).unwrap();
    for path in [
        "memory/templates/../secret.md",
        "memory/templates/sub/file.md",
        "memory/templates/README.md",
        "memory/templates/release.txt",
        "memory/templates/a\\b.md",
        "/memory/templates/release.md",
    ] {
        assert!(document::path_kind(path).is_err(), "{path}");
    }
    for value in [
        " ".into(),
        "# NUL\0".into(),
        "x".repeat(256 * 1024 + 1),
        format!("# Example\n{}", "sk-".to_owned() + &"a".repeat(24)),
    ] {
        assert!(document::validate(TEMPLATE, &value).is_err());
    }
    let mut state = State::default();
    state.documents.insert(
        TEMPLATE.into(),
        "---\nsession_id: codex-task\nworkstream_id: demo-task\n---\n\n# Template".into(),
    );
    document::validate_all(&state.documents).unwrap();
    assert!(
        document::find(&state.documents, "session_id", "codex-task")
            .unwrap()
            .is_none()
    );
    let s=engine::start(&mut state,&json!({"agent":"codex","native_session_id":"task","project":"demo","workstream_id":"demo-task","title":"Task"})).unwrap();
    let hash = s["handoff"]["session"]["sha256"].clone();
    engine::abort(
        &mut state,
        &json!({"session_id":"codex-task","expected_sha256":hash}),
    )
    .unwrap();
    assert_eq!(
        state.documents.len(),
        1,
        "template metadata cannot keep a provisional Workstream alive"
    );
    let huge_title = format!("# {}\n\nBODY_MUST_NOT_BE_LISTED", "中".repeat(200));
    state.documents.insert(TEMPLATE.into(), huge_title);
    let list = engine::list(&state.documents, &json!({"kind":"template","limit":1})).unwrap();
    assert_eq!(
        list["documents"][0]["title"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        160
    );
    assert!(!list.to_string().contains("BODY_MUST_NOT_BE_LISTED"));
}

#[tokio::test]
async fn http_templates_keep_cas_idempotency_isolation_and_explicit_loading() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::open(tmp.path()).unwrap();
    store
        .import(Documents::from_iter([(
            "memory/project/demo.md".into(),
            project(),
        )]))
        .unwrap();
    let app = api::router_with_principals(store, principals()).unwrap();
    let payload = json!({"path":TEMPLATE,"content":raw()});
    let (s, saved) = call(
        &app,
        TOKEN,
        "shared",
        "PUT",
        "/v1/documents",
        payload.clone(),
        Some("create"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    let (_, replay) = call(
        &app,
        TOKEN,
        "shared",
        "PUT",
        "/v1/documents",
        payload,
        Some("create"),
    )
    .await;
    assert_eq!(replay["idempotent"], true);
    let get_path = format!("/v1/documents?path={TEMPLATE}");
    let (s, inherited) = call(&app, TOKEN, "device", "GET", &get_path, Value::Null, None).await;
    assert_eq!(s, StatusCode::OK, "{inherited}");
    assert_eq!(inherited["namespace"], "shared");
    assert_eq!(inherited["content"], raw());
    assert_eq!(inherited["sha256"], document::sha(&raw()));
    assert_eq!(
        call(&app, OTHER, "other", "GET", &get_path, Value::Null, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, OTHER, "shared", "GET", &get_path, Value::Null, None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            OTHER,
            "other",
            "PUT",
            "/v1/documents",
            json!({"path":TEMPLATE,"content":"# Denied"}),
            Some("denied")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            TOKEN,
            "device",
            "PUT",
            "/v1/documents",
            json!({"path":TEMPLATE,"content":"# Wrong","expected_sha256":"wrong"}),
            Some("wrong")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (s, updated) = call(
        &app,
        TOKEN,
        "device",
        "PUT",
        "/v1/documents",
        json!({"path":TEMPLATE,"content":"# Device template","expected_sha256":saved["sha256"]}),
        Some("override"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{updated}");
    assert_eq!(
        call(&app, TOKEN, "device", "GET", &get_path, Value::Null, None)
            .await
            .1["content"],
        "# Device template"
    );
    assert_eq!(
        call(&app, TOKEN, "shared", "GET", &get_path, Value::Null, None)
            .await
            .1["content"],
        raw()
    );
    let (_, list) = call(
        &app,
        TOKEN,
        "shared",
        "GET",
        "/v1/documents?kind=template&limit=1",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["documents"][0]["kind"], "template");
    assert!(!list.to_string().contains("完整骨架"));
    let (_, routed) = call(
        &app,
        TOKEN,
        "device",
        "POST",
        "/v1/route",
        json!({"project":"demo","scenario":"发布文档模板","content":true}),
        None,
    )
    .await;
    assert!(!routed.to_string().contains("Device template"));
    assert!(!routed.to_string().contains("完整骨架"));
    assert_eq!(
        call(
            &app,
            TOKEN,
            "device",
            "GET",
            "/v1/health",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[test]
fn directory_import_restart_and_online_backup_preserve_exact_resource_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir_all(source.join("memory/templates")).unwrap();
    fs::write(source.join(TEMPLATE), raw()).unwrap();
    fs::write(
        source.join("memory/templates/README.md"),
        "# Derived catalog",
    )
    .unwrap();
    let scoped = namespace::physical("device", TEMPLATE);
    fs::create_dir_all(source.join(Path::new(&scoped).parent().unwrap())).unwrap();
    fs::write(source.join(&scoped), "# Device template").unwrap();
    let docs = import_directory(&source).unwrap();
    assert_eq!(docs.len(), 2);
    let cfg = backup::Config {
        schema_version: 1,
        data_dir: tmp.path().join("store"),
        output_dir: tmp.path().join("backups"),
        oss: None,
    };
    let mut store = Store::open(&cfg.data_dir).unwrap();
    store.import(docs.clone()).unwrap();
    // A failed publication may leave a temporary revision but never change HEAD or its receipt.
    let revision = store.revision.clone();
    let revisions = cfg.data_dir.join("revisions");
    fs::rename(&revisions, cfg.data_dir.join("held")).unwrap();
    let request =
        json!({"path":TEMPLATE,"content":"# Updated","expected_sha256":document::sha(&raw())});
    assert!(
        store
            .transact("failed", &request, |s| engine::put(s, &request))
            .is_err()
    );
    assert_eq!(store.revision, revision);
    assert_eq!(store.state.documents, docs);
    assert!(store.state.receipts.is_empty());
    fs::rename(cfg.data_dir.join("held"), &revisions).unwrap();
    let b = backup::create(&cfg, true).unwrap();
    backup::verify(&cfg, Path::new(b["manifest"].as_str().unwrap())).unwrap();
    let restore = tmp.path().join("restored");
    fs::create_dir(&restore).unwrap();
    let archive = cfg
        .output_dir
        .join(b["backup"]["archive"].as_str().unwrap());
    tar::Archive::new(flate2::read::GzDecoder::new(
        fs::File::open(archive).unwrap(),
    ))
    .unpack(&restore)
    .unwrap();
    let restored = Store::open(&restore).unwrap();
    assert_eq!(restored.state.documents, docs);
    drop(store);
    assert_eq!(Store::open(&cfg.data_dir).unwrap().state.documents, docs);
}
