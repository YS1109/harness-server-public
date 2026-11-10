use harness_server::{api, store::Store};
use serde_json::{Value, json};
use std::{path::Path, process::Command};

fn client(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_harness"));
    command.arg("--config").arg(config);
    command.env_remove("HARNESS_CLIENT_CONFIG");
    command.env_remove("HARNESS_URL");
    command.env_remove("HARNESS_TENANT");
    command.env_remove("HARNESS_AGENT_ID");
    command.env_remove("HARNESS_NAMESPACE");
    command.env_remove("HARNESS_READ_NAMESPACES");
    command
}

#[tokio::test]
async fn loopback_bypasses_proxies_and_cli_preserves_binding_and_retry_identity() {
    let temp = tempfile::tempdir().unwrap();
    let nested = temp.path().join("code/nested");
    std::fs::create_dir_all(&nested).unwrap();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let app = api::router(
        Store::open(temp.path().join("store")).unwrap(),
        vec![token.clone()],
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = temp.path().join("client.json");
    std::fs::write(
        &config,
        json!({"url":url,"projects":{"parent":temp.path().join("code"),"demo":nested}}).to_string(),
    )
    .unwrap();
    tokio::task::spawn_blocking(move || {
        let run = |args: &[&str]| {
            let output = client(&config)
                .current_dir(&nested)
                .env("HARNESS_TOKEN", &token)
                .env("HTTP_PROXY", "http://127.0.0.1:1")
                .env("http_proxy", "http://127.0.0.1:1")
                .env("ALL_PROXY", "http://127.0.0.1:1")
                .env("all_proxy", "http://127.0.0.1:1")
                .env("NO_PROXY", "")
                .env("no_proxy", "")
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            (value, String::from_utf8(output.stderr).unwrap())
        };
        let (route, stderr) = run(&["route", "--scenario", "coding"]);
        assert_eq!(route["project"], "demo");
        assert!(!stderr.contains("Idempotency-Key"));
        let args = [
            "start",
            "--agent",
            "test",
            "--native-session-id",
            "one",
            "--title",
            "Proxy regression",
            "--idempotency-key",
            "client-retry",
        ];
        let (first, stderr) = run(&args);
        assert!(stderr.contains("client-retry"));
        let (retry, _) = run(&args);
        assert_eq!(retry["idempotent"], true);
        assert_eq!(retry["execution_group_id"], first["execution_group_id"]);
    })
    .await
    .unwrap();
    server.abort();
}

#[test]
fn client_rejects_remote_plaintext_before_network_or_token_access() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("client.json");
    std::fs::write(&config, "{}").unwrap();
    let output = client(&config)
        .env_remove("HARNESS_TOKEN")
        .args(["--url", "http://192.0.2.1:8080", "health"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires HTTPS"));
}

#[tokio::test]
async fn daily_skips_discovery_credentials_payloads_and_configuration_writes() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let app = axum::Router::new().fallback(move || {
        count.fetch_add(1, Ordering::SeqCst);
        async { axum::Json(json!({"unexpected":true})) }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("client.json");
        let broken = temp.path().join("broken.json");
        std::fs::write(&broken, "not JSON").unwrap();
        for args in [
            vec!["route", "--project", "daily"],
            vec![
                "start",
                "--project",
                "daily",
                "--agent",
                "test",
                "--native-session-id",
                "daily-test",
                "--title",
                "Daily",
            ],
            vec![
                "finish",
                "--project",
                "daily",
                "--mode",
                "log",
                "--result-file",
                "missing-result.json",
            ],
            vec!["stats", "--project", "daily"],
            vec!["list", "--project", "daily"],
        ] {
            for path in [&config, &broken] {
                let output = client(path)
                    .env_remove("HARNESS_TOKEN")
                    .args(["--url", &url, "--timings"])
                    .args(&args)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(
                    serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                    json!({"mode":"SKIP"})
                );
                assert!(String::from_utf8_lossy(&output.stderr).contains("\"http_requests\":0"));
            }
        }
        assert!(!config.exists());
        assert_eq!(std::fs::read_to_string(&broken).unwrap(), "not JSON");
        let data = json!({"url":url,"projects":{"daily":temp.path()}}).to_string();
        std::fs::write(&config, &data).unwrap();
        for args in [
            vec!["route"],
            vec![
                "start",
                "--agent",
                "test",
                "--native-session-id",
                "daily-test",
                "--title",
                "Daily",
            ],
            vec![
                "finish",
                "--mode",
                "log",
                "--result-file",
                "missing-result.json",
            ],
        ] {
            let output = client(&config)
                .current_dir(temp.path())
                .env_remove("HARNESS_TOKEN")
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                json!({"mode":"SKIP"})
            );
        }
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), data);
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 2);
    })
    .await
    .unwrap();
    server.abort();
}

#[test]
fn local_template_check_ignores_unreadable_config_and_needs_no_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let template = temp.path().join("template.md");
    let document = temp.path().join("document.md");
    let config = temp.path().join("broken.json");
    std::fs::write(&config, "not valid JSON").unwrap();
    std::fs::write(&template, "# {project} release\n## Rollback\n- [ ] Confirm").unwrap();
    std::fs::write(&document, "# Demo release\n## Rollback\n- [x] Checked").unwrap();
    let run = || {
        client(&config)
            .env_remove("HARNESS_TOKEN")
            .args(["check-template", "--template-file"])
            .arg(&template)
            .arg("--document-file")
            .arg(&document)
            .output()
            .unwrap()
    };
    let pass = run();
    assert!(
        pass.status.success(),
        "{}",
        String::from_utf8_lossy(&pass.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&pass.stdout).unwrap()["passed"],
        true
    );
    std::fs::write(&document, "# Demo release").unwrap();
    let missing = run();
    assert!(!missing.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&missing.stdout).unwrap()["passed"],
        false
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), "not valid JSON");
}

#[tokio::test]
async fn template_and_explain_cli_requests_are_single_reads_with_explicit_purpose() {
    use axum::{
        Json, Router,
        extract::Request,
        middleware::{self, Next},
        response::Response,
    };
    use std::sync::{Arc, Mutex};
    let captured = Arc::new(Mutex::new(
        Vec::<(String, axum::http::HeaderMap, Value)>::new(),
    ));
    let entries = captured.clone();
    let app = Router::new()
        .fallback(|| async { Json(json!({"passed":true})) })
        .layer(middleware::from_fn(move |request: Request, next: Next| {
            let entries = entries.clone();
            async move {
                let (parts, body) = request.into_parts();
                let bytes = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
                entries.lock().unwrap().push((
                    parts.uri.path().into(),
                    parts.headers.clone(),
                    serde_json::from_slice(&bytes).unwrap(),
                ));
                let response: Response = next.run(Request::from_parts(parts, bytes.into())).await;
                response
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("client.json");
        let document = temp.path().join("output.md");
        std::fs::write(
            &config,
            json!({"url":url,"namespace":"shared","token_env":"HARNESS_TOKEN"}).to_string(),
        )
        .unwrap();
        std::fs::write(&document, "# Release").unwrap();
        for args in [
            vec![
                "--purpose",
                "diagnostic",
                "check-template",
                "--template-path",
                "memory/templates/release.md",
                "--document-file",
                document.to_str().unwrap(),
            ],
            vec!["route", "--project", "demo", "--scenario", "coding"],
            vec![
                "--purpose",
                "task",
                "route",
                "--project",
                "demo",
                "--scenario",
                "coding",
                "--explain",
            ],
        ] {
            let output = client(&config)
                .env("HARNESS_TOKEN", "fixture-client-000000000000000000000000")
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    })
    .await
    .unwrap();
    server.abort();
    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 3, "no write preflight or extra request");
    assert_eq!(requests[0].0, "/v1/check-template");
    assert_eq!(requests[0].1["X-Harness-Purpose"], "diagnostic");
    assert_eq!(requests[0].2["document"], "# Release");
    assert!(
        requests
            .iter()
            .all(|(_, headers, _)| !headers.contains_key("Idempotency-Key"))
    );
    assert!(
        requests[1].2.get("explain").is_none(),
        "old services retain ordinary route compatibility"
    );
    assert_eq!(requests[2].2["explain"], true);
    assert_eq!(requests[2].1["X-Harness-Purpose"], "task");
}

#[test]
fn embedded_instructions_are_offline_read_only_and_do_not_expose_credentials() {
    use std::net::TcpListener;
    let temp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = temp.path().join("client.json");
    let original = json!({
        "url":format!("http://{}/?credential=fixture-url-secret",listener.local_addr().unwrap()),
        "agent_id":"device-a","namespace":"ns-device","read_namespaces":["ns-device","shared"],
        "token_env":"UNAVAILABLE_HARNESS_TOKEN","projects":{"demo":temp.path().join("nonexistent")}
    })
    .to_string();
    std::fs::write(&config, &original).unwrap();
    let output = client(&config)
        .env_remove("UNAVAILABLE_HARNESS_TOKEN")
        .env("HARNESS_TOKEN", "fixture-env-secret")
        .args(["instructions", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(v["source"], "embedded-client");
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["agent_id"], "device-a");
    assert_eq!(v["task_namespace"], "ns-device");
    assert_eq!(v["read_namespaces"], json!(["ns-device", "shared"]));
    let rules = v["instructions"].as_str().unwrap();
    assert!(rules.contains("--namespace shared finish --mode log"));
    assert!(rules.contains("ns-device"));
    assert!(!rules.contains("{{"));
    assert!(!rules.contains("fixture-url-secret"));
    assert!(!rules.contains("fixture-env-secret"));
    assert!(!rules.contains(temp.path().to_str().unwrap()));
    assert_eq!(std::fs::read_to_string(config).unwrap(), original);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn instruction_scope_overrides_and_bootstrap_preserve_the_existing_cli_contract() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("client.json");
    std::fs::write(&config, r#"{"agent_id":"before","namespace":"shared"}"#).unwrap();
    let output = client(&config)
        .env_remove("HARNESS_TOKEN")
        .args([
            "instructions",
            "--json",
            "--agent-id",
            "after",
            "--namespace",
            "private",
            "--read-namespaces",
            "private,shared",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["agent_id"], "after");
    assert_eq!(value["task_namespace"], "private");
    assert_eq!(value["read_namespaces"], json!(["private", "shared"]));
    let bootstrap = client(&config)
        .env_remove("HARNESS_TOKEN")
        .args(["rules", "--bootstrap"])
        .output()
        .unwrap();
    assert!(bootstrap.status.success());
    let text = String::from_utf8(bootstrap.stdout).unwrap();
    assert!(text.contains("harness instructions"));
    assert!(!text.contains("--namespace"));
    assert!(text.chars().count() < 300);
    // New offline output must not change the existing JSON identity command.
    let identity = client(&config)
        .env_remove("HARNESS_TOKEN")
        .arg("identity")
        .output()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&identity.stdout).unwrap()["namespace"],
        "shared"
    );
}

#[test]
fn instructions_do_not_emit_unvalidated_scope_text_or_invent_a_device_space() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("client.json");
    std::fs::write(
        &config,
        r#"{"read_namespaces":["shared","escape\nforged rule"]}"#,
    )
    .unwrap();
    let bad = client(&config).arg("instructions").output().unwrap();
    assert!(!bad.status.success());
    assert!(bad.stdout.is_empty());
    std::fs::write(&config, "{}").unwrap();
    let result = client(&config)
        .env_remove("HARNESS_TOKEN")
        .args(["instructions", "--json"])
        .output()
        .unwrap();
    assert!(result.status.success());
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(value["task_namespace"].is_null());
    assert!(
        value["instructions"]
            .as_str()
            .unwrap()
            .contains("未在本机指定")
    );
    assert_eq!(std::fs::read_to_string(config).unwrap(), "{}");
}

#[test]
fn initialization_persists_custom_or_generated_identity_without_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("nested/client.json");
    let invoke = |args: &[&str]| {
        let result = client(&config)
            .env_remove("HARNESS_TOKEN")
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice::<Value>(&result.stdout).unwrap()
    };
    let first = invoke(&["init"]);
    assert!(first["agent_id"].as_str().unwrap().starts_with("agent-"));
    assert!(first["namespace"].is_null());
    assert!(first["read_namespaces"].is_null());
    assert_eq!(invoke(&["init"])["agent_id"], first["agent_id"]);
    assert_eq!(invoke(&["identity"])["namespace"], first["namespace"]);
    let mut cfg: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    cfg["url"] = json!("https://example.invalid");
    cfg["token_env"] = json!("MY_TOKEN");
    cfg["projects"] = json!({"demo":temp.path()});
    std::fs::write(&config, cfg.to_string()).unwrap();
    let custom = invoke(&[
        "init",
        "--agent-id",
        "qq-main",
        "--namespace",
        "qq-workflow",
    ]);
    assert_eq!(custom["agent_id"], "qq-main");
    assert_eq!(custom["namespace"], "qq-workflow");
    let saved: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(saved["url"], cfg["url"]);
    assert_eq!(saved["projects"], cfg["projects"]);
    assert_eq!(saved["token_env"], cfg["token_env"]);
    assert!(
        !std::fs::read_to_string(&config)
            .unwrap()
            .contains("HARNESS_TOKEN=")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let before = std::fs::read(&config).unwrap();
    assert!(
        !client(&config)
            .args(["init", "--namespace", "../escape"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(std::fs::read(&config).unwrap(), before);
    std::fs::write(&config, "{broken}").unwrap();
    assert!(
        !client(&config)
            .arg("init")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(std::fs::read_to_string(config).unwrap(), "{broken}");
}

#[tokio::test]
async fn scoped_client_propagates_identity_and_rejects_unscoped_server_before_write() {
    use harness_server::auth::Principal;
    let temp = tempfile::tempdir().unwrap();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let app = api::router_with_principals(
        Store::open(temp.path().join("store")).unwrap(),
        vec![Principal {
            agent_id: Some("local-agent".into()),
            token: token.clone(),
            read_namespaces: vec!["private".into()],
            write_namespaces: vec!["private".into()],
        }],
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = temp.path().join("client.json");
    std::fs::write(
        &config,
        json!({"url":url,"agent_id":"local-agent","namespace":"private"}).to_string(),
    )
    .unwrap();
    let config_copy = config.clone();
    let token_copy = token.clone();
    tokio::task::spawn_blocking(move || {
        for args in [
            vec!["health"],
            vec![
                "start",
                "--agent",
                "codex",
                "--native-session-id",
                "native",
                "--project",
                "demo",
                "--title",
                "Test",
            ],
        ] {
            let o = client(&config_copy)
                .env("HARNESS_TOKEN", &token_copy)
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            let v: Value = serde_json::from_slice(&o.stdout).unwrap();
            assert_eq!(v["identity"]["namespace"], "private");
            assert_eq!(v["identity"]["agent_id"], "local-agent");
        }
    })
    .await
    .unwrap();
    server.abort();
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let writes = Arc::new(AtomicUsize::new(0));
    let count = writes.clone();
    let old = axum::Router::new()
        .route(
            "/v1/health",
            axum::routing::get(|| async { axum::Json(json!({"status":"healthy"})) }),
        )
        .route(
            "/v1/start",
            axum::routing::post(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({"saved":true}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, old).await.unwrap() });
    std::fs::write(
        &config,
        json!({"url":url,"agent_id":"local-agent","namespace":"private"}).to_string(),
    )
    .unwrap();
    tokio::task::spawn_blocking(move || {
        let o = client(&config)
            .env("HARNESS_TOKEN", &token)
            .args([
                "start",
                "--agent",
                "pi",
                "--native-session-id",
                "native",
                "--project",
                "demo",
                "--title",
                "Old",
            ])
            .output()
            .unwrap();
        assert!(!o.status.success());
        assert!(String::from_utf8_lossy(&o.stderr).contains("write not sent"));
        let o = client(&config)
            .env("HARNESS_TOKEN", &token)
            .args([
                "--timings",
                "--write-protocol",
                "scoped-v2",
                "start",
                "--agent",
                "pi",
                "--native-session-id",
                "native-fast",
                "--project",
                "demo",
                "--title",
                "Old server fast",
            ])
            .output()
            .unwrap();
        assert!(!o.status.success());
        assert!(String::from_utf8_lossy(&o.stderr).contains("404"));
        assert!(String::from_utf8_lossy(&o.stderr).contains("\"http_requests\":1"));
    })
    .await
    .unwrap();
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn connect_validates_existing_spaces_and_multi_read_client_uses_server_defaults() {
    use harness_server::auth::Principal;
    let temp = tempfile::tempdir().unwrap();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let app = api::router_with_principals(
        Store::open(temp.path().join("store")).unwrap(),
        vec![Principal {
            agent_id: Some("multi-agent".into()),
            token: token.clone(),
            read_namespaces: vec!["one".into(), "two".into()],
            write_namespaces: vec!["one".into()],
        }],
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = temp.path().join("client.json");
    std::fs::write(
        &config,
        json!({"url":url,"token_env":"HARNESS_TOKEN","projects":{}}).to_string(),
    )
    .unwrap();
    tokio::task::spawn_blocking(move || {
        let run = |args: &[&str]| {
            client(&config)
                .env("HARNESS_TOKEN", &token)
                .args(args)
                .output()
                .unwrap()
        };
        let o = run(&["namespaces"]);
        assert!(o.status.success());
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v["default_read_namespaces"], json!(["one", "two"]));
        let o = run(&[
            "connect",
            "--namespace",
            "one",
            "--read-namespaces",
            "one,two",
        ]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let cfg: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(cfg["agent_id"], "multi-agent");
        assert_eq!(cfg["read_namespaces"], json!(["one", "two"]));
        let before = std::fs::read(&config).unwrap();
        let o = run(&["connect", "--namespace", "invented"]);
        assert!(!o.status.success());
        assert_eq!(std::fs::read(&config).unwrap(), before);
        let o = run(&[
            "route",
            "--project",
            "demo",
            "--scenario",
            "coding",
            "--content",
        ]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v["read_namespaces"], json!(["one", "two"]));
        let o = run(&[
            "start",
            "--project",
            "demo",
            "--agent",
            "pi",
            "--native-session-id",
            "multi",
            "--title",
            "Task",
            "--content",
        ]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let start: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(start["identity"]["namespace"], "one");
        // A changed local selection must not move an existing tracked Session.
        let mut cfg = cfg;
        cfg["namespace"] = json!("two");
        std::fs::write(&config, cfg.to_string()).unwrap();
        let result = config.with_file_name("result.json");
        std::fs::write(&result, json!({"current":"Done"}).to_string()).unwrap();
        let denied_log = run(&[
            "finish",
            "--mode",
            "log",
            "--project",
            "demo",
            "--result-file",
            result.to_str().unwrap(),
        ]);
        assert!(
            !denied_log.status.success(),
            "log-only must retain the selected read-only namespace, not use server write default"
        );
        let o = run(&[
            "finish",
            "--session-id",
            start["session_id"].as_str().unwrap(),
            "--expected-workstream-sha256",
            start["workstream_sha256"].as_str().unwrap(),
            "--result-file",
            result.to_str().unwrap(),
        ]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let finish: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(finish["identity"]["namespace"], "one");
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test]
async fn first_use_discovery_selects_only_unique_write_and_never_overrides_or_guesses() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let catalog = Arc::new(Mutex::new(json!({
        "registry_version":1,"agent_id":"catalog-agent","write_protocols":["v1","scoped-v2"],
        "default_write_namespace":"shared","default_read_namespaces":["shared"],
        "namespaces":[
            {"id":"private","name":"Personal space","read":true,"write":true,"status":"ACTIVE"},
            {"id":"shared","name":"Team knowledge","read":true,"write":false,"status":"ACTIVE"}
        ]
    })));
    let requests = Arc::new(AtomicUsize::new(0));
    let info = catalog.clone();
    let count = requests.clone();
    let app = axum::Router::new().route(
        "/v1/namespaces",
        axum::routing::get(move || {
            count.fetch_add(1, Ordering::SeqCst);
            let info = info.lock().unwrap().clone();
            async move { axum::Json(info) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("fresh.json");
        let invoke = |config: &Path, args: &[&str]| {
            let before = requests.load(Ordering::SeqCst);
            let output = client(config)
                .env("HARNESS_TOKEN", "fixture")
                .args(["--url", &url, "--timings"])
                .args(args)
                .output()
                .unwrap();
            assert_eq!(
                requests.load(Ordering::SeqCst),
                before + 1,
                "discover exactly once"
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains("\"http_requests\":1"));
            output
        };
        let first = invoke(&config, &["connect"]);
        assert!(
            first.status.success(),
            "{}",
            String::from_utf8_lossy(&first.stderr)
        );
        let selected: Value = serde_json::from_slice(&first.stdout).unwrap();
        assert_eq!(selected["namespace"], "private");
        assert_eq!(selected["namespace_selection"], "single_authorized_write");
        let saved: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(saved["namespace"], "private");
        assert_eq!(saved["agent_id"], "catalog-agent");
        assert_eq!(saved["write_protocol"], "scoped-v2");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        catalog.lock().unwrap()["namespaces"][1]["write"] = json!(true);
        let mut saved = saved;
        saved["read_namespaces"] = json!(["private", "shared"]);
        saved["projects"] = json!({"demo":temp.path()});
        std::fs::write(&config, saved.to_string()).unwrap();
        let keep = invoke(&config, &["setup"]);
        assert!(
            keep.status.success(),
            "{}",
            String::from_utf8_lossy(&keep.stderr)
        );
        let kept: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(kept["namespace"], saved["namespace"]);
        assert_eq!(kept["read_namespaces"], saved["read_namespaces"]);
        assert_eq!(kept["projects"], saved["projects"]);

        let fresh = temp.path().join("ambiguous.json");
        std::fs::write(&fresh, "{}").unwrap();
        let ambiguous = invoke(&fresh, &["setup"]);
        assert!(!ambiguous.status.success());
        let error = String::from_utf8_lossy(&ambiguous.stderr);
        assert!(error.contains("namespace_choice_required"), "{error}");
        assert!(error.contains("Personal space") && error.contains("Team knowledge"));
        assert!(error.contains("--namespace") && error.contains("--read-namespaces"));
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "{}");
        assert!(!temp.path().join(".client.tmp").exists());
        let explicit = invoke(
            &fresh,
            &[
                "setup",
                "--namespace",
                "shared",
                "--read-namespaces",
                "shared,private",
            ],
        );
        assert!(
            explicit.status.success(),
            "{}",
            String::from_utf8_lossy(&explicit.stderr)
        );
        let before = std::fs::read(&fresh).unwrap();
        catalog.lock().unwrap()["namespaces"][1]["write"] = json!(false);
        let readonly = invoke(&fresh, &["connect"]);
        assert!(!readonly.status.success());
        assert!(String::from_utf8_lossy(&readonly.stderr).contains("not authorized for writing"));
        assert_eq!(std::fs::read(&fresh).unwrap(), before);

        let none = temp.path().join("no-write.json");
        catalog.lock().unwrap()["namespaces"][0]["write"] = json!(false);
        let denied = invoke(&none, &["connect"]);
        assert!(!denied.status.success());
        assert!(String::from_utf8_lossy(&denied.stderr).contains("namespace_choice_required"));
        assert!(!none.exists());
        catalog.lock().unwrap()["agent_id"] = Value::Null;
        catalog.lock().unwrap()["namespaces"][1]["write"] = json!(true);
        let legacy = invoke(&none, &["setup"]);
        assert!(
            legacy.status.success(),
            "{}",
            String::from_utf8_lossy(&legacy.stderr)
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(&none).unwrap()).unwrap();
        assert_eq!(saved["namespace"], "shared");
        assert!(
            saved["agent_id"].is_null(),
            "legacy credentials must not invent an identity"
        );
        assert!(
            std::fs::read_dir(temp.path())
                .unwrap()
                .all(|entry| { entry.unwrap().path().extension().is_none_or(|s| s != "tmp") })
        );
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test]
async fn unconfigured_business_calls_discover_once_with_choices_and_preserve_stdin_and_config() {
    use axum::{
        extract::Request,
        middleware::{self, Next},
    };
    use harness_server::auth::Principal;
    use std::io::Write;
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    let temp = tempfile::tempdir().unwrap();
    let paths = Arc::new(Mutex::new(Vec::new()));
    let entries = paths.clone();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let app = api::router_with_principals(
        Store::open(temp.path().join("store")).unwrap(),
        vec![Principal {
            agent_id: Some("known-agent".into()),
            token: token.clone(),
            read_namespaces: vec!["one".into(), "two".into()],
            write_namespaces: vec!["one".into(), "two".into()],
        }],
    )
    .unwrap()
    .layer(middleware::from_fn(move |request: Request, next: Next| {
        entries
            .lock()
            .unwrap()
            .push(request.uri().path().to_owned());
        async move { next.run(request).await }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = temp.path().join("client.json");
    let original = json!({"url":url}).to_string();
    std::fs::write(&config, &original).unwrap();
    tokio::task::spawn_blocking(move || {
        for args in [
            vec![
                "start",
                "--agent",
                "test",
                "--native-session-id",
                "fresh",
                "--project",
                "demo",
                "--title",
                "Task",
            ],
            vec!["put", "--path", "memory/project/demo.md", "--file", "-"],
            vec![
                "finish",
                "--mode",
                "log",
                "--project",
                "demo",
                "--result-file",
                "-",
            ],
            vec!["--agent-id", "known-agent", "route", "--project", "demo"],
        ] {
            let mut child = client(&config)
                .env("HARNESS_TOKEN", &token)
                .args(["--timings"])
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            if args.contains(&"-") {
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(br#"{"current":"Done"}"#)
                    .unwrap();
            } else {
                drop(child.stdin.take());
            }
            let output = child.wait_with_output().unwrap();
            assert!(!output.status.success());
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("namespace_choice_required"), "{error}");
            assert!(error.contains("\"http_requests\":1") && error.contains("setup"));
            assert!(error.contains("\"id\":\"one\"") && error.contains("\"id\":\"two\""));
            assert!(output.stdout.is_empty());
            assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
        }
        assert_eq!(*paths.lock().unwrap(), vec!["/v1/namespaces"; 4]);
    })
    .await
    .unwrap();
    server.abort();
}

#[test]
fn tenant_validation_offline_reports_precedence_and_cross_tenant_config_isolation() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("client.json");
    let first = client(&config)
        .env("HARNESS_TENANT", "team-red")
        .args(["init", "--namespace", "private"])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let original = std::fs::read(&config).unwrap();
    let saved: Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(saved["tenant"], "team-red");
    for args in [
        vec!["identity"],
        vec!["instructions", "--json"],
        vec!["doctor"],
    ] {
        let output = client(&config)
            .env_remove("HARNESS_TOKEN")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["tenant"], "team-red");
        if let Some(instructions) = report["instructions"].as_str() {
            assert!(instructions.contains("租户 `team-red`"));
            assert!(!instructions.contains("{{TENANT}}"));
            assert!(instructions.contains("实际授权以服务为准"));
        }
    }
    let precedence = client(&config)
        .env("HARNESS_TENANT", "team-blue")
        .args(["--tenant", "team-red", "identity"])
        .output()
        .unwrap();
    assert!(precedence.status.success());
    let mut bound = saved;
    bound["url"] = json!("https://old.example.invalid");
    bound["read_namespaces"] = json!(["private", "shared"]);
    bound["write_protocol"] = json!("scoped-v2");
    std::fs::write(&config, bound.to_string()).unwrap();
    let changed = client(&config)
        .env_remove("HARNESS_TOKEN")
        .args([
            "--url",
            "https://new.example.invalid",
            "instructions",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(changed.status.success());
    let report: Value = serde_json::from_slice(&changed.stdout).unwrap();
    assert_eq!(report["tenant"], "team-red");
    assert!(
        report["agent_id"].is_null()
            && report["task_namespace"].is_null()
            && report["read_namespaces"].is_null()
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&config).unwrap()).unwrap(),
        bound
    );
    std::fs::write(&config, &original).unwrap();
    for command in ["identity", "setup", "connect", "init", "doctor"] {
        let mismatch = client(&config)
            .args(["--tenant", "team-blue", command])
            .output()
            .unwrap();
        assert!(!mismatch.status.success());
        assert!(String::from_utf8_lossy(&mismatch.stderr).contains("separate --config"));
        assert_eq!(std::fs::read(&config).unwrap(), original);
    }
    for tenant in [
        "",
        "../escape",
        "Team",
        "bad\nheader",
        "with space",
        &"a".repeat(129),
    ] {
        let bad = client(&config)
            .args(["--tenant", tenant, "identity"])
            .output()
            .unwrap();
        assert!(!bad.status.success());
        assert!(String::from_utf8_lossy(&bad.stderr).contains("tenant must"));
    }
    std::fs::write(&config, r#"{"tenant":"invalid/slug"}"#).unwrap();
    assert!(
        !client(&config)
            .arg("identity")
            .output()
            .unwrap()
            .status
            .success()
    );
    std::fs::write(&config, r#"{"namespace":"shared"}"#).unwrap();
    let mismatch = client(&config)
        .args(["--tenant", "team-red", "setup"])
        .output()
        .unwrap();
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("separate --config"));
    let fresh = temp.path().join("origin.json");
    let invalid_url = client(&fresh)
        .args([
            "--tenant",
            "team-red",
            "--url",
            "https://example.invalid/tenants/team-red",
            "init",
        ])
        .output()
        .unwrap();
    assert!(!invalid_url.status.success());
    assert!(!fresh.exists());
}

#[tokio::test]
async fn tenant_header_covers_discovery_doctor_reads_fast_writes_and_v1_preflight() {
    use axum::{
        extract::Request,
        middleware::{self, Next},
    };
    use harness_server::auth::Principal;
    use std::{
        io::Write,
        process::Stdio,
        sync::{Arc, Mutex},
    };
    use tower::ServiceExt;
    let temp = tempfile::tempdir().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let entries = captured.clone();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let child = api::router_with_principals(
        Store::open(temp.path().join("store")).unwrap(),
        vec![Principal {
            agent_id: Some("tenant-agent".into()),
            token: token.clone(),
            read_namespaces: vec!["private".into(), "shared".into()],
            write_namespaces: vec!["private".into()],
        }],
    )
    .unwrap();
    // This fixture models gateway dispatch without relying on the server's gateway implementation.
    let app = axum::Router::new()
        .fallback(move |mut request: Request| {
            let child = child.clone();
            async move {
                if let Some(path) = request.uri().to_string().strip_prefix("/v3/tenant") {
                    *request.uri_mut() = path.parse().unwrap();
                }
                child.oneshot(request).await.unwrap()
            }
        })
        .layer(middleware::from_fn(
            move |mut request: Request, next: Next| {
                entries
                    .lock()
                    .unwrap()
                    .push((request.uri().path().to_owned(), request.headers().clone()));
                let tenant = request.headers_mut().remove("X-Harness-Tenant");
                async move {
                    let mut response = next.run(request).await;
                    if let Some(tenant) = tenant {
                        response.headers_mut().insert("X-Harness-Tenant", tenant);
                    }
                    response
                }
            },
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let config = temp.path().join("tenant.json");
        let run = |args: &[&str], input: Option<&str>, expected: &[&str]| {
            captured.lock().unwrap().clear();
            let mut child = client(&config)
                .env("HARNESS_TOKEN", &token)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            if let Some(input) = input {
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(input.as_bytes())
                    .unwrap();
            } else {
                drop(child.stdin.take());
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let requests = captured.lock().unwrap();
            assert_eq!(
                requests
                    .iter()
                    .map(|(path, _)| path.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
            for (_, headers) in requests.iter() {
                assert_eq!(headers["X-Harness-Tenant"], "team-red");
            }
            let report = serde_json::from_slice::<Value>(&output.stdout).unwrap();
            assert_eq!(report["tenant"], "team-red");
            if report["identity"].is_object() {
                assert_eq!(report["identity"]["tenant"], "team-red");
            }
            report
        };
        let first = run(
            &[
                "--url",
                &url,
                "--tenant",
                "team-red",
                "route",
                "--project",
                "demo",
            ],
            None,
            &["/v1/namespaces", "/v1/route"],
        );
        assert_eq!(first["tenant"], "team-red");
        let setup = run(
            &["--url", &url, "--tenant", "team-red", "setup"],
            None,
            &["/v1/namespaces"],
        );
        assert_eq!(setup["tenant"], "team-red");
        assert_eq!(setup["namespace"], "private");
        run(&["namespaces"], None, &["/v1/namespaces"]);
        let before = std::fs::read(&config).unwrap();
        let doctor = run(&["doctor"], None, &["/v1/namespaces"]);
        assert_eq!(doctor["tenant_source"], "config");
        assert_eq!(std::fs::read(&config).unwrap(), before);
        run(&["health"], None, &["/v1/health"]);
        run(&["list"], None, &["/v1/documents"]);
        run(&["route", "--project", "demo"], None, &["/v1/route"]);
        run(
            &[
                "--write-protocol",
                "v1",
                "put",
                "--path",
                "memory/templates/probe.md",
                "--file",
                "-",
            ],
            Some("# Probe\n## Section"),
            &["/v1/health", "/v3/tenant/v1/documents"],
        );
        run(
            &["get", "--path", "memory/templates/probe.md"],
            None,
            &["/v1/documents"],
        );
        run(
            &[
                "--write-protocol",
                "v1",
                "start",
                "--agent",
                "test",
                "--native-session-id",
                "old",
                "--project",
                "demo",
                "--title",
                "Old",
            ],
            None,
            &["/v1/health", "/v3/tenant/v1/start"],
        );
        let started = run(
            &[
                "start",
                "--agent",
                "test",
                "--native-session-id",
                "fast",
                "--project",
                "demo",
                "--title",
                "Fast",
            ],
            None,
            &["/v3/tenant/v2/scoped/start"],
        );
        let args = [
            "--idempotency-key",
            "tenant-retry",
            "finish",
            "--session-id",
            started["session_id"].as_str().unwrap(),
            "--expected-workstream-sha256",
            started["workstream_sha256"].as_str().unwrap(),
            "--result-file",
            "-",
        ];
        let first = run(
            &args,
            Some(r#"{"current":"Done"}"#),
            &["/v3/tenant/v2/scoped/finish"],
        );
        let retry = run(
            &args,
            Some(r#"{"current":"Done"}"#),
            &["/v3/tenant/v2/scoped/finish"],
        );
        assert_eq!(retry["idempotent"], true);
        assert_eq!(retry["execution_group_id"], first["execution_group_id"]);
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test]
async fn tenant_writes_never_reach_unguarded_old_server_and_discovery_requires_acknowledgement() {
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    let writes = Arc::new(AtomicUsize::new(0));
    let count = writes.clone();
    let echo_health = Arc::new(AtomicBool::new(false));
    let echo = echo_health.clone();
    let app = Router::new()
        .route("/v1/start", post(move || {
            count.fetch_add(1, Ordering::SeqCst);
            async { Json(json!({"saved":true})) }
        }))
        .route("/v1/health", get(move || {
            let acknowledged = echo.load(Ordering::SeqCst);
            async move {
                let mut headers = axum::http::HeaderMap::new();
                if acknowledged {
                    headers.insert("X-Harness-Tenant", axum::http::HeaderValue::from_static("team-red"));
                }
                (headers, Json(json!({"identity":{"agent_id":"old-agent","namespace":"private"},"registry_version":1})))
            }
        }))
        .route("/v1/namespaces", get(|| async {
            Json(json!({"registry_version":1,"agent_id":"old-agent","write_protocols":["v1","scoped-v2"],
                "namespaces":[{"id":"private","name":"Old private","read":true,"write":true,"status":"ACTIVE"}]}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("client.json");
        let original =
            json!({"url":url,"tenant":"team-red","agent_id":"old-agent","namespace":"private"})
                .to_string();
        std::fs::write(&config, &original).unwrap();
        let args = [
            "--timings",
            "start",
            "--agent",
            "test",
            "--native-session-id",
            "old",
            "--project",
            "demo",
            "--title",
            "Must not write",
        ];
        let v1 = client(&config)
            .env("HARNESS_TOKEN", "fixture")
            .args(args)
            .output()
            .unwrap();
        assert!(!v1.status.success());
        let error = String::from_utf8_lossy(&v1.stderr);
        assert!(
            error.contains("acknowledge X-Harness-Tenant") && error.contains("\"http_requests\":1"),
            "{error}"
        );
        let fast = client(&config)
            .env("HARNESS_TOKEN", "fixture")
            .args(["--write-protocol", "scoped-v2"])
            .args(args)
            .output()
            .unwrap();
        assert!(!fast.status.success());
        let error = String::from_utf8_lossy(&fast.stderr);
        assert!(
            error.contains("404") && error.contains("\"http_requests\":1"),
            "{error}"
        );
        echo_health.store(true, Ordering::SeqCst);
        let guarded_v1 = client(&config)
            .env("HARNESS_TOKEN", "fixture")
            .args(args)
            .output()
            .unwrap();
        assert!(!guarded_v1.status.success());
        let error = String::from_utf8_lossy(&guarded_v1.stderr);
        assert!(
            error.contains("404") && error.contains("\"http_requests\":2"),
            "{error}"
        );
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
        let fresh = temp.path().join("fresh.json");
        let setup = client(&fresh)
            .env("HARNESS_TOKEN", "fixture")
            .args(["--url", &url, "--tenant", "team-red", "setup"])
            .output()
            .unwrap();
        assert!(!setup.status.success());
        assert!(String::from_utf8_lossy(&setup.stderr).contains("acknowledge X-Harness-Tenant"));
        assert!(!fresh.exists());
        let read = client(&config)
            .env("HARNESS_TOKEN", "fixture")
            .arg("namespaces")
            .output()
            .unwrap();
        assert!(!read.status.success());
        assert!(
            read.stdout.is_empty(),
            "unacknowledged selectors cannot produce verified tenant provenance"
        );
        let doctor = client(&config)
            .env("HARNESS_TOKEN", "fixture")
            .arg("doctor")
            .output()
            .unwrap();
        assert!(doctor.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&doctor.stdout).unwrap()["connection"],
            "tenant_unconfirmed"
        );
    })
    .await
    .unwrap();
    server.abort();
}
