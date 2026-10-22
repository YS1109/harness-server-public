use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    middleware::{self, Next},
};
use harness_server::{api, auth::Principal, document, store::Store};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;
const TOKEN: &str = "fixture-user-0000000000000000000000000";
const ADMIN: &str = "fixture-admin-000000000000000000000000";
fn app(path: &Path) -> Router {
    api::router_with_admin(
        Store::open(path).unwrap(),
        vec![Principal {
            agent_id: Some("test-agent".into()),
            token: TOKEN.into(),
            read_namespaces: vec!["private".into(), "shared".into()],
            write_namespaces: vec!["private".into(), "shared".into()],
        }],
        Some(ADMIN.into()),
    )
    .unwrap()
}
async fn call(
    app: &Router,
    token: &str,
    method: &str,
    path: &str,
    value: Value,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .uri(path)
        .method(method)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json");
    if let Some(k) = key {
        b = b.header("Idempotency-Key", k);
    }
    let r = app
        .clone()
        .oneshot(b.body(Body::from(value.to_string())).unwrap())
        .await
        .unwrap();
    let status = r.status();
    (
        status,
        serde_json::from_slice(&to_bytes(r.into_body(), 4 * 1024 * 1024).await.unwrap()).unwrap(),
    )
}
fn project() -> String {
    document::render(json!({"schema_version":1,"memory_type":"project","project_id":"demo","title":"Demo","status":"ACTIVE","verified_at":"unknown"}).as_object().unwrap(),"# Demo").unwrap()
}
#[tokio::test]
async fn cli_connect_negotiates_single_request_writes_and_stdin_with_safe_legacy_default() {
    let temp = tempfile::tempdir().unwrap();
    let paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = paths.clone();
    let app = app(&temp.path().join("store")).layer(middleware::from_fn(
        move |req: Request<Body>, next: Next| {
            let p = observed.clone();
            async move {
                p.lock().unwrap().push(req.uri().path().to_owned());
                next.run(req).await
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = temp.path().join("client.json");
    std::fs::write(
        &config,
        json!({"url":url,"agent_id":"test-agent","namespace":"private"}).to_string(),
    )
    .unwrap();
    tokio::task::spawn_blocking(move||{
  use std::{process::{Command,Stdio},io::Write};
  let run=|args:&[&str],stdin:Option<&str>|{
   paths.lock().unwrap().clear();
   let mut c=Command::new(env!("CARGO_BIN_EXE_harness"));c.arg("--config").arg(&config).args(args).env("HARNESS_TOKEN",TOKEN);
   for name in ["HARNESS_URL","HARNESS_AGENT_ID","HARNESS_NAMESPACE","HARNESS_READ_NAMESPACES","HARNESS_CLIENT_CONFIG"]{c.env_remove(name);}
   c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());let mut child=c.spawn().unwrap();
   if let Some(s)=stdin{child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();}else{drop(child.stdin.take());}
   let o=child.wait_with_output().unwrap();assert!(o.status.success(),"{}",String::from_utf8_lossy(&o.stderr));
   (serde_json::from_slice::<Value>(&o.stdout).unwrap(),paths.lock().unwrap().clone(),String::from_utf8(o.stderr).unwrap())
  };
  let (_,p,_)=run(&["--timings","start","--agent","test","--native-session-id","old","--project","demo","--title","Old path"],None);assert_eq!(p,vec!["/v1/health","/v1/start"]);
  let original=std::fs::read(&config).unwrap();
  let root=config.parent().unwrap().to_str().unwrap();
  let (_,p,_)=run(&["setup","--token-env","HARNESS_TOKEN","--project","demo","--project-root",root],None); assert_eq!(p,vec!["/v1/namespaces"]);
  let saved:Value=serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
  assert_eq!(saved["agent_id"],"test-agent"); assert_eq!(saved["namespace"],"private");assert_eq!(saved["projects"]["demo"],config.parent().unwrap().canonicalize().unwrap().to_str().unwrap());assert!(!saved.to_string().contains(TOKEN));
  let backups:Vec<_>=std::fs::read_dir(config.parent().unwrap()).unwrap().filter_map(Result::ok).filter(|e|e.path().extension().is_some_and(|s|s=="bak")).collect(); assert_eq!(backups.len(),1);assert_eq!(std::fs::read(backups[0].path()).unwrap(),original);
  let fresh=config.parent().unwrap().join("fresh-client.json");
  let fresh_setup=std::process::Command::new(env!("CARGO_BIN_EXE_harness")).arg("--config").arg(&fresh).args(["--url",saved["url"].as_str().unwrap(),"--namespace","private","setup"]).env("HARNESS_TOKEN",TOKEN).output().unwrap();
  assert!(fresh_setup.status.success(),"{}",String::from_utf8_lossy(&fresh_setup.stderr));assert_eq!(serde_json::from_slice::<Value>(&std::fs::read(&fresh).unwrap()).unwrap()["write_protocol"],"scoped-v2");
  let before=std::fs::read(&config).unwrap();
  let failed=std::process::Command::new(env!("CARGO_BIN_EXE_harness")).arg("--config").arg(&config).args(["--agent-id","wrong-agent","setup"]).env("HARNESS_TOKEN",TOKEN).env_remove("HARNESS_URL").output().unwrap();assert!(!failed.status.success()); assert_eq!(std::fs::read(&config).unwrap(),before);
  let (_,p,_)=run(&["connect"],None);assert_eq!(p,vec!["/v1/namespaces"]);
  assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(&config).unwrap()).unwrap()["write_protocol"],"scoped-v2");
  let (started,p,stderr)=run(&["--timings","start","--agent","test","--native-session-id","new","--project","demo","--title","Fast path"],None);assert_eq!(p,vec!["/v2/scoped/start"]);assert!(stderr.contains("\"http_requests\":1"));
  let input=json!({"current":"Done","status":"COMPLETED"}).to_string();
  let (_,p,_)=run(&["finish","--session-id",started["session_id"].as_str().unwrap(),"--expected-workstream-sha256",started["workstream_sha256"].as_str().unwrap(),"--result-file","-"],Some(&input));assert_eq!(p,vec!["/v2/scoped/finish"]);
  let before=std::fs::read(&config).unwrap();let (doctor,p,_)=run(&["doctor"],None);assert_eq!(p,vec!["/v1/namespaces"]);assert_eq!(doctor["connection"],"verified");assert!(!doctor.to_string().contains(TOKEN));assert_eq!(std::fs::read(&config).unwrap(),before);
 }).await.unwrap();
    server.abort();
}
#[tokio::test]
async fn admin_runtime_persists_metrics_and_is_read_only_with_isolated_execution_browsing() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("store");
    let service = app(&path);
    let (_, r) = call(
        &service,
        TOKEN,
        "POST",
        "/v1/route",
        json!({"project":"demo","scenario":"coding"}),
        None,
    )
    .await;
    assert_ne!(r["mode"], "SKIP");
    let (_, r) = call(
        &service,
        ADMIN,
        "GET",
        "/admin/api/runtime?days=7",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(r["metrics"]["observed_calls"], 1);
    assert_eq!(r["storage"]["document_limit"], 4096);
    assert!(r["metrics"]["handler_ms_p50"].is_number());
    let revision = r["storage"]["revision"].clone();
    let (_, again) = call(
        &service,
        ADMIN,
        "GET",
        "/admin/api/runtime?days=7",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(again["storage"]["revision"], revision);
    assert_eq!(again["metrics"]["observed_calls"], 1);
    assert_eq!(
        call(
            &service,
            TOKEN,
            "GET",
            "/admin/api/runtime",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    drop(service);
    let service = app(&path);
    let (_, r) = call(
        &service,
        ADMIN,
        "GET",
        "/admin/api/runtime?days=7",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(r["metrics"]["observed_calls"], 1);
    assert_eq!(
        call(
            &service,
            ADMIN,
            "GET",
            "/admin/api/runtime?days=8",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (_,created)=call(&service,TOKEN,"POST","/v1/executions",json!({"task_id":"task-a","conversation_id":"qq-a","source_key":"msg-a","project":"demo","title":"Review build","goal":"Check result"}),Some("execution-a")).await;
    assert_eq!(created["task"]["task_id"], "task-a");
    let (_, list) = call(
        &service,
        ADMIN,
        "GET",
        "/admin/api/executions?namespace=private&q=Review",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(list["total"], 1);
    let (_, list) = call(
        &service,
        ADMIN,
        "GET",
        "/admin/api/executions?namespace=shared",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(list["total"], 0);
    let (_, detail) = call(
        &service,
        ADMIN,
        "GET",
        "/admin/api/execution?namespace=private&task_id=task-a",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(detail["task"]["task_id"], "task-a");
    assert_eq!(detail["attempts"]["total"], 0);
}
#[tokio::test]
async fn oversized_document_has_actionable_error_and_leaves_head_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let service = app(temp.path());
    let body = project();
    let (status, _) = call(
        &service,
        TOKEN,
        "PUT",
        "/v2/scoped/documents",
        json!({"path":"memory/project/demo.md","content":body}),
        Some("create-project"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, replay) = call(
        &service,
        TOKEN,
        "PUT",
        "/v2/scoped/documents",
        json!({"path":"memory/project/demo.md","content":body}),
        Some("create-project"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["idempotent"], true);
    let (status, _) = call(
        &service,
        TOKEN,
        "PUT",
        "/v2/scoped/documents",
        json!({"path":"memory/project/demo.md","content":body,"expected_sha256":"0".repeat(64)}),
        Some("bad-cas"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, before) = call(
        &service,
        ADMIN,
        "GET",
        "/admin/api/runtime",
        Value::Null,
        None,
    )
    .await;
    let oversized = project().replace("# Demo", &"中".repeat(2001));
    let (status,error)=call(&service,TOKEN,"PUT","/v2/scoped/documents",json!({"path":"memory/project/demo.md","content":oversized,"expected_sha256":document::sha(&body)}),Some("too-large")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["details"]["limit"], 2000);
    assert!(error["error"]["details"]["actual"].as_u64().unwrap() > 2000);
    assert_eq!(error["error"]["details"]["saved"], false);
    let (_, after) = call(
        &service,
        ADMIN,
        "GET",
        "/admin/api/runtime",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(after["storage"]["revision"], before["storage"]["revision"]);
}
#[test]
fn project_binding_does_not_change_identity_and_missing_unrelated_roots_do_not_block() {
    use std::process::Command;
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("client.json");
    std::fs::write(
        &config,
        json!({"namespace":"shared","projects":{"obsolete":temp.path().join("gone")}}).to_string(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_harness"))
        .arg("--config")
        .arg(&config)
        .args(["bind", "demo"])
        .arg(temp.path())
        .env_remove("HARNESS_CLIENT_CONFIG")
        .output()
        .unwrap();
    assert!(output.status.success());
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert!(cfg["agent_id"].is_null());
    assert_eq!(cfg["namespace"], "shared");
    let output = Command::new(env!("CARGO_BIN_EXE_harness"))
        .arg("--config")
        .arg(&config)
        .arg("doctor")
        .current_dir(temp.path())
        .env_remove("HARNESS_URL")
        .env_remove("HARNESS_TOKEN")
        .env_remove("HARNESS_CLIENT_CONFIG")
        .output()
        .unwrap();
    assert!(output.status.success());
    let r: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(r["project_match"], "demo");
    assert_eq!(r["connection"], "missing_credential");
}

#[test]
fn recent_document_filtering_precedes_pagination_and_orders_timestamp_offsets() {
    let mut docs = document::Documents::new();
    for (id, date, status, title) in [
        ("a", "2026-10-15T09:00:00+08:00", "ACTIVE", "Build old"),
        ("b", "2026-10-15T02:00:00Z", "ACTIVE", "Build new"),
        ("c", "2026-10-15T03:00:00Z", "ARCHIVED", "Build archive"),
    ] {
        let content=document::render(json!({"schema_version":1,"memory_type":"project","project_id":id,"title":title,"status":status,"updated_at":date,"verified_at":"unknown"}).as_object().unwrap(),"# Fixture").unwrap();
        docs.insert(format!("memory/project/{id}.md"), content);
    }
    let page=harness_server::engine::list(&docs,&json!({"kind":"project","status":"ACTIVE","q":"build","sort":"updated_desc","limit":1,"offset":1})).unwrap();
    assert_eq!(page["total"], 2);
    assert_eq!(page["remaining"], 0);
    assert_eq!(page["documents"][0]["project"], "a");
    let old = harness_server::engine::list(&docs, &json!({"kind":"project"})).unwrap();
    assert_eq!(old["documents"][0]["project"], "a");
}
