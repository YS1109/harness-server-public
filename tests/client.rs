use harness_server::{api, store::Store};
use serde_json::{Value, json};
use std::{path::Path, process::Command};

fn client(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_harness"));
    command.arg("--config").arg(config);
    command.env_remove("HARNESS_CLIENT_CONFIG");
    command.env_remove("HARNESS_URL");
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
            .env("HARNESS_TOKEN", token)
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
