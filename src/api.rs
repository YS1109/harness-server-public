use crate::{
    auth::{self, Access, Principal},
    document::validate_all,
    engine,
    error::{Error, Result, invalid},
    execution, namespace, registry,
    store::Store,
};
use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Clone)]
pub(crate) struct App {
    pub(crate) store: Arc<Mutex<Store>>,
    pub(crate) principals: Arc<Vec<Principal>>,
    pub(crate) admin_hash: Option<String>,
    pub(crate) metrics: Arc<crate::metrics::Ledger>,
    pub(crate) capacity: Arc<crate::admin::StorageCache>,
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            Self::Invalid(_) | Self::Field { .. } | Self::Budget { .. } => {
                (StatusCode::BAD_REQUEST, "validation")
            }
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::Storage(_) => (StatusCode::INTERNAL_SERVER_ERROR, "storage"),
        };
        (
            status,
            Json(
                json!({"error":{"code":code,"message":self.to_string(),"details":self.details()}}),
            ),
        )
            .into_response()
    }
}
pub fn tokens_from_environment() -> Result<Vec<String>> {
    if let Ok(raw) = std::env::var("HARNESS_TOKENS_JSON") {
        let values: BTreeMap<String, String> = serde_json::from_str(&raw)
            .map_err(|_| invalid("HARNESS_TOKENS_JSON必须是设备ID到凭据的JSON对象"))?;
        if values.is_empty() {
            return Err(invalid("没有设备凭据"));
        }
        for device in values.keys() {
            crate::document::slug(device)?;
        }
        Ok(values.into_values().collect())
    } else {
        Ok(vec![std::env::var("HARNESS_TOKEN").map_err(|_| {
            invalid("需要HARNESS_TOKEN或HARNESS_TOKENS_JSON")
        })?])
    }
}
pub fn router(store: Store, tokens: Vec<String>) -> Result<Router> {
    router_with_principals(store, auth::legacy(tokens))
}
pub fn router_with_principals(store: Store, principals: Vec<Principal>) -> Result<Router> {
    router_with_admin(store, principals, None)
}
pub fn router_with_admin(
    mut store: Store,
    mut principals: Vec<Principal>,
    admin_token: Option<String>,
) -> Result<Router> {
    auth::validate(&mut principals)?;
    let admin_hash = admin_token
        .map(|token| {
            if token.len() < 32 || token.len() > 512 || token.chars().any(char::is_whitespace) {
                return Err(invalid("管理员凭据必须32–512字且无空白"));
            }
            let hash = crate::document::sha(&token);
            if principals.iter().any(|p| p.token == hash) {
                return Err(invalid("管理员与Agent凭据必须分离"));
            }
            Ok(hash)
        })
        .transpose()?;
    registry::initialize(&mut store, &principals, admin_hash.is_some())?;
    let ledger = crate::metrics::Ledger::new(store.root());
    let app = App {
        store: Arc::new(Mutex::new(store)),
        principals: Arc::new(principals),
        admin_hash,
        metrics: Arc::new(ledger),
        capacity: Arc::new(crate::admin::StorageCache::default()),
    };
    let admin = crate::admin::router(app.clone());
    let private = Router::new()
        .route("/v2/scoped/start", post(start))
        .route("/v2/scoped/finish", post(finish))
        .route("/v2/scoped/abort", post(abort))
        .route("/v2/scoped/documents", put(document_put))
        .route("/v1/namespaces", get(discovery))
        .route("/v1/route", post(route))
        .route("/v1/eval-routing", post(eval_routing))
        .route("/v1/start", post(start))
        .route("/v1/finish", post(finish))
        .route("/v1/abort", post(abort))
        .route("/v1/documents", get(document).merge(put(document_put)))
        .route("/v1/health", get(health))
        .route("/v1/stats", get(stats))
        .route("/v1/executions", get(execution_list).post(execution_create))
        .route("/v1/executions/{task_id}", get(execution_get))
        .route(
            "/v1/executions/{task_id}/attempts",
            get(attempt_list).post(attempt_begin),
        )
        .route(
            "/v1/executions/{task_id}/attempts/{attempt_id}",
            get(attempt_get),
        )
        .route(
            "/v1/executions/{task_id}/checkpoint",
            post(execution_checkpoint),
        )
        .route("/v1/executions/{task_id}/cancel", post(execution_cancel))
        .route(
            "/v1/executions/{task_id}/delivery",
            post(execution_delivery),
        )
        .route(
            "/v1/execution-contexts/{conversation_id}",
            get(context_get).put(context_put),
        )
        .layer(middleware::from_fn_with_state(app.clone(), authenticate))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(app);
    Ok(Router::new()
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .merge(private)
        .merge(admin))
}
async fn authenticate(State(app): State<App>, mut req: Request<Body>, next: Next) -> Response {
    let scoped_v2 = req.uri().path().starts_with("/v2/scoped/");
    let logical_path = req.uri().path().replacen("/v2/scoped/", "/v1/", 1);
    let result = (|| -> Result<Access> {
        let bearer = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .unwrap_or("");
        let read_header = |name: &str| -> Result<Option<&str>> {
            req.headers()
                .get(name)
                .map(|v| v.to_str().map_err(|_| invalid("身份/空间请求头无效")))
                .transpose()
        };
        let id = read_header("X-Harness-Agent-Id")?;
        let principal = auth::principal(&app.principals, bearer, id)?
            .ok_or_else(|| Error::Forbidden("需要有效设备凭据".into()))?;
        let store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
        let policy = registry::agent(&store.state.documents, principal)?;
        let requested = read_header("X-Harness-Namespace")?;
        let ns = requested
            .map(str::to_owned)
            .or(policy.default_write_namespace.clone())
            .or_else(|| policy.default_read_namespaces.first().cloned())
            .unwrap_or_else(|| "shared".into());
        crate::document::slug(&ns)?;
        let routing = matches!(
            logical_path.as_str(),
            "/v1/route" | "/v1/start" | "/v1/eval-routing"
        );
        let scopes = if let Some(header) = read_header("X-Harness-Read-Namespaces")? {
            if !routing {
                return Err(invalid("读取范围只用于route/start"));
            }
            header.split(',').map(str::to_owned).collect::<Vec<_>>()
        } else if requested.is_some() {
            let mut scopes = vec![ns.clone()];
            // Preserve authorized v0.2 shared background inheritance.
            if ns != "shared" && policy.read_namespaces.iter().any(|s| s == "shared") {
                scopes.push("shared".into());
            }
            scopes
        } else {
            policy.default_read_namespaces.clone()
        };
        let mut seen = std::collections::BTreeSet::new();
        if scopes.len() > 8
            || scopes
                .iter()
                .any(|scope| crate::document::slug(scope).is_err() || !seen.insert(scope))
        {
            return Err(invalid("读取范围必须唯一，最多8个namespace"));
        }
        let access = Access {
            protocol: if scoped_v2 { "scoped-v2" } else { "v1" }.into(),
            agent_id: principal.agent_id.clone(),
            namespace: ns.clone(),
            namespace_explicit: requested.is_some(),
            agent_header: id.is_some(),
            legacy_shared: ns == "shared" && id.is_none(),
            inherit_shared: policy.read_namespaces.iter().any(|s| s == "shared"),
            read_namespaces: scopes,
        };
        if logical_path != "/v1/namespaces" {
            let write = req.method() != axum::http::Method::GET
                && !matches!(logical_path.as_str(), "/v1/route" | "/v1/eval-routing");
            let handoff = matches!(logical_path.as_str(), "/v1/finish" | "/v1/abort");
            if !(handoff && requested.is_none()) {
                registry::ensure(&store.state.documents, &access, write)?;
            }
        }
        Ok(access)
    })();
    match result {
        Ok(access) => {
            req.extensions_mut().insert(access);
        }
        Err(Error::Forbidden(message)) if message == "需要有效设备凭据" => {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
                Json(json!({"error":{"code":"unauthorized","message":message}})),
            )
                .into_response();
        }
        Err(e) => return e.into_response(),
    }
    let mut response = next.run(req).await;
    if scoped_v2 {
        response.headers_mut().insert(
            "X-Harness-Protocol",
            axum::http::HeaderValue::from_static("scoped-v2"),
        );
    }
    response
}
async fn discovery(
    State(app): State<App>,
    Extension(access): Extension<Access>,
) -> Result<Json<Value>> {
    let store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
    let p = app
        .principals
        .iter()
        .find(|p| p.agent_id == access.agent_id)
        .ok_or_else(|| Error::Forbidden("身份未注册".into()))?;
    Ok(Json(registry::discovery(&store.state.documents, p)?))
}
async fn execute(
    app: App,
    mut access: Access,
    command: &'static str,
    value: Value,
    key: Option<String>,
) -> Result<Json<Value>> {
    tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        let mut lock_wait_ms=None; let mut commit_ms=None; let mut operation_ms=None;
        let result = (|| {
            let mut store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
            lock_wait_ms=Some(start.elapsed().as_secs_f64()*1000.0);
            store.last_commit_ms=None;
            let operation_started=Instant::now();
            let result=(|| {
            if matches!(command,"start"|"finish"|"abort") && !access.namespace_explicit {
                let principal=app.principals.iter().find(|p|p.agent_id==access.agent_id).ok_or_else(||Error::Forbidden("身份不存在".into()))?;
                let policy=registry::agent(&store.state.documents,principal)?;
                let session_id=if command=="start" {format!("{}-{}",value["agent"].as_str().unwrap_or(""),value["native_session_id"].as_str().unwrap_or(""))}else{value["session_id"].as_str().unwrap_or("").into()};let id=session_id.as_str();let mut locations=vec![];
                for ns in &policy.read_namespaces {
                    if crate::document::find(&namespace::view(&store.state.documents,ns,false),"session_id",id)?.is_some() {locations.push(ns.clone());}
                }
                if locations.len()>1 {return Err(crate::error::conflict("Session存在于多个namespace；请显式选择start返回的namespace"));}
                if let Some(ns)=locations.pop() {access.namespace=ns;}
                access.legacy_shared=access.namespace=="shared" && !access.agent_header;
            }
            let write=!matches!(command,"route"|"get"|"list"|"execution-list"|"execution-get"|"attempt-list"|"attempt-get"|"context-get");
            registry::ensure(&store.state.documents,&access,write)?;
            if matches!(command, "get" | "put")
                && value["path"]
                    .as_str()
                    .is_some_and(|p| p.starts_with("namespaces/")||p.starts_with("control/"))
            {
                return Err(invalid("API使用逻辑路径；namespace只通过请求头选择"));
            }
            let documents = namespace::view(
                &store.state.documents,
                &access.namespace,
                access.inherit_shared,
            );
            let mut response = match command {
                "route" => engine::route_scoped(&store.state.documents, &access.read_namespaces, &value),
                "get" => engine::get(&documents, value["path"].as_str().unwrap_or("")),
                "list" => engine::list(&documents, &value),
                "execution-list" => execution::list(&documents, &value),
                "execution-get" => {
                    execution::get(&documents, value["task_id"].as_str().unwrap_or(""))
                }
                "attempt-list" => execution::list_attempts(
                    &documents,
                    value["task_id"].as_str().unwrap_or(""),
                    &value["payload"],
                ),
                "attempt-get" => execution::get_attempt(
                    &documents,
                    value["task_id"].as_str().unwrap_or(""),
                    value["attempt_id"].as_str().unwrap_or(""),
                ),
                "context-get" => execution::get_context(
                    &documents,
                    value["conversation_id"].as_str().unwrap_or(""),
                ),
                _ => {
                    let key = key
                        .as_deref()
                        .ok_or_else(|| invalid("写请求必须提供Idempotency-Key"))?;
                    let scoped_key = namespace::receipt_key(
                        &access.namespace,
                        if access.legacy_shared {
                            None
                        } else {
                            access.agent_id.as_deref()
                        },
                        key,
                    )?;
                    let mut request = json!({"command":command,"request":value});
                    if !access.legacy_shared {
                        request["identity"] =
                            json!({"agent_id":access.agent_id,"namespace":access.namespace});
                    }
                    if command=="start" && !access.legacy_shared && store.state.receipts.get(&scoped_key).is_none_or(|r|r.hash!=crate::document::sha(&request.to_string())) {request["read_namespaces"]=json!(access.read_namespaces);}
                    if let Some(receipt)=store.state.receipts.get(&scoped_key) {
                        let principal=app.principals.iter().find(|p|p.agent_id==access.agent_id).ok_or_else(||Error::Forbidden("身份不存在".into()))?;
                        let policy=registry::agent(&store.state.documents,principal)?;
                        fn visible(value:&Value,allowed:&[String])->bool {match value {Value::Object(o)=>o.get("namespace").and_then(Value::as_str).is_none_or(|ns|allowed.iter().any(|s|s==ns)) && o.get("read_namespaces").and_then(Value::as_array).is_none_or(|scopes|scopes.iter().all(|ns|ns.as_str().is_some_and(|ns|allowed.iter().any(|s|s==ns)))) && o.values().all(|v|visible(v,allowed)),Value::Array(a)=>a.iter().all(|v|visible(v,allowed)),_=>true}}
                        if !visible(&receipt.response,&policy.read_namespaces) {return Err(Error::Forbidden("原请求包含已撤销空间；拒绝重放历史内容".into()));}
                    }
                    store.transact(&scoped_key, &request, |state| {
                        let memories=if command=="start" {
                            Some(engine::route_scoped(&state.documents,&access.read_namespaces,&json!({"project":value["project"],"scenario":value["scenario"].as_str().unwrap_or(""),"content":value["content"].as_bool().unwrap_or(false)}))?)
                        } else {None};
                        let mut response=namespace::transact(
                            state,
                            &access.namespace,
                            access.inherit_shared,
                            access.agent_id.as_deref(),
                            |state| match command {
                                "start" => {let mut v=value.clone();if let Some(project)=memories.as_ref().and_then(|m|m["project"].as_str()) {v["project"]=json!(project);}engine::start(state,&v)},
                                "finish" => engine::finish(state, &value),
                                "abort" => engine::abort(state, &value),
                                "put" => engine::put(state, &value),
                                "execution-create" => execution::create(state, &value),
                                "attempt-begin" => execution::begin_attempt(
                                    state,
                                    value["task_id"].as_str().unwrap_or(""),
                                    &value["payload"],
                                ),
                                "execution-checkpoint" => execution::checkpoint(
                                    state,
                                    value["task_id"].as_str().unwrap_or(""),
                                    &value["payload"],
                                ),
                                "execution-cancel" => execution::cancel(
                                    state,
                                    value["task_id"].as_str().unwrap_or(""),
                                    &value["payload"],
                                ),
                                "execution-delivery" => execution::delivery(
                                    state,
                                    value["task_id"].as_str().unwrap_or(""),
                                    &value["payload"],
                                ),
                                "context-put" => execution::put_context(
                                    state,
                                    value["conversation_id"].as_str().unwrap_or(""),
                                    &value["payload"],
                                ),
                                _ => unreachable!(),
                            },
                        )?;
                        if let Some(memories)=memories && response["mode"]!="SKIP" {response["memories"]=memories;}
                        Ok(response)
                    })
                }
            }?;
            if response["mode"]!="SKIP" && command=="route" {response["read_namespaces"]=json!(access.read_namespaces);}
            if command=="start" && response["memories"]["read_namespaces"].is_array() {response["read_namespaces"]=response["memories"]["read_namespaces"].clone();}
            if access.agent_id.is_some() || access.namespace != "shared" {
                namespace::annotate(&mut response, &store.state.documents, &access.namespace);
                response["identity"] =
                    json!({"agent_id":access.agent_id,"namespace":access.namespace});
            }
            Ok(response)
            })();
            commit_ms=store.last_commit_ms;
            operation_ms=Some((operation_started.elapsed().as_secs_f64()*1000.0-commit_ms.unwrap_or(0.0)).max(0.0));
            result
        })();
        if !result.as_ref().is_ok_and(|v| v["mode"] == "SKIP") {
            let response = result.as_ref().ok();
            let count = |field: &str| response.and_then(|v| v[field].as_u64().or_else(|| v["memories"][field].as_u64()));
            let returned_characters = if command == "get" {
                response
                    .and_then(|v| v["content"].as_str())
                    .map(|s| s.chars().count() as u64)
            } else {
                count("returned_characters")
            };
            let route_result = matches!(command, "route" | "start").then(|| {
                response.map_or(
                    crate::metrics::RouteResult::Unknown,
                    |v| crate::metrics::RouteResult::from_response(if command == "start" {
                        &v["memories"]
                    } else {
                        v
                    }),
                )
            });
            app.metrics.record(&crate::metrics::Event {
                protocol:Some(access.protocol.clone()), execution_group_id:response.and_then(|v|v["execution_group_id"].as_str()).or_else(||value["execution_group_id"].as_str()).filter(|id|crate::document::slug(id).is_ok()).map(str::to_owned),
                lock_wait_ms, operation_ms, commit_ms,
                event_id:Some(uuid::Uuid::new_v4().to_string()), service_version:Some(env!("CARGO_PKG_VERSION").into()),
                timestamp:chrono::Utc::now(), namespace:access.namespace.clone(), command:command.into(),
                project:response.and_then(|v|v["project"].as_str()).or_else(||value["project"].as_str()).filter(|p|crate::document::slug(p).is_ok()).map(str::to_owned),
                handler_ms:start.elapsed().as_secs_f64()*1000.0, success:result.is_ok(),
                retry:response.is_some_and(|v|v["idempotent"]==true),
                error_code:result.as_ref().err().map(|e|e.code().to_owned()),
                validation_reason: result.as_ref().err().and_then(Error::validation_reason),
                route_result,
                returned_characters,
                handoff_characters:count("handoff_characters"),
            });
        }
        result.map(Json)
    })
    .await
    .map_err(|_| invalid("请求处理任务失败"))?
}
fn key(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("Idempotency-Key")
        .and_then(|s| s.to_str().ok())
        .map(str::to_owned)
}
async fn route(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    execute(app, access, "route", v, None).await
}
async fn start(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    h: axum::http::HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    execute(app, access, "start", v, key(&h)).await
}
async fn finish(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    h: axum::http::HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    execute(app, access, "finish", v, key(&h)).await
}
async fn abort(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    h: axum::http::HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    execute(app, access, "abort", v, key(&h)).await
}
async fn document_put(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    h: axum::http::HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    execute(app, access, "put", v, key(&h)).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathQuery {
    path: Option<String>,
    kind: Option<String>,
    q: Option<String>,
    status: Option<String>,
    sort: Option<String>,
    project: Option<String>,
    limit: Option<u64>,
    offset: Option<u64>,
}
async fn document(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Query(q): Query<PathQuery>,
) -> Result<Json<Value>> {
    if let Some(path) = q.path {
        execute(app, access, "get", json!({"path":path}), None).await
    } else {
        execute(app, access,"list",json!({"kind":q.kind.unwrap_or_default(),"project":q.project.unwrap_or_default(),"limit":q.limit.unwrap_or(20),"offset":q.offset.unwrap_or(0),"q":q.q.unwrap_or_default(),"status":q.status.unwrap_or_default(),"sort":q.sort.unwrap_or_default()}),None).await
    }
}
async fn health(
    State(app): State<App>,
    Extension(access): Extension<Access>,
) -> Result<Json<Value>> {
    tokio::task::spawn_blocking(move|| {
        let store=app.store.lock().map_err(|_|invalid("存储锁不可用"))?;
        registry::ensure(&store.state.documents,&access,false)?;
        let docs=namespace::view(&store.state.documents,&access.namespace,access.inherit_shared);
        validate_all(&docs)?;
        Ok(Json(json!({"registry_version":1,"write_protocols":["v1","scoped-v2"],"version":env!("CARGO_PKG_VERSION"),"build":crate::build_info::metadata(),"status":"healthy","revision":if access.namespace=="shared" {store.revision.clone()} else {None},"documents":docs.len(),"identity":{"agent_id":access.agent_id,"namespace":access.namespace},"check_scope":"当前namespace及获准继承的共享记忆；结构、身份与引用，不检查外部链接"})))
    }).await.map_err(|_|invalid("健康检查失败"))?
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StatsQuery {
    pub days: Option<u32>,
    pub project: Option<String>,
}
pub(crate) fn stats_days(days: Option<u32>) -> Result<u32> {
    let days = days.unwrap_or(30);
    if !matches!(days, 7 | 30 | 90) {
        return Err(invalid("days必须为7、30或90"));
    }
    Ok(days)
}
async fn stats(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Query(q): Query<StatsQuery>,
) -> Result<Json<Value>> {
    let days = stats_days(q.days)?;
    let q = StatsQuery {
        days: q.days,
        project: q.project.filter(|p| !p.is_empty()),
    };
    if let Some(p) = &q.project {
        crate::document::slug(p)?;
    }
    tokio::task::spawn_blocking(move || {
        {
            let store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
            registry::ensure(&store.state.documents, &access, false)?;
        }
        let mut report = app.metrics.report(
            days,
            Some(&access.namespace),
            q.project.as_deref(),
            chrono::Utc::now(),
        );
        report["identity"] = json!({"agent_id":access.agent_id,"namespace":access.namespace});
        report["since"] = json!("persisted-events");
        report["scope"] = report["metrics_scope"].clone();
        Ok(Json(report))
    })
    .await
    .map_err(|_| invalid("统计读取失败"))?
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct ExecutionQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    active_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<usize>,
}
async fn execution_list(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Query(q): Query<ExecutionQuery>,
) -> Result<Json<Value>> {
    execute(app, access, "execution-list", json!(q), None).await
}
async fn execution_get(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    execute(app, access, "execution-get", json!({"task_id":id}), None).await
}
async fn execution_create(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    h: axum::http::HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    execute(app, access, "execution-create", v, key(&h)).await
}
#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Pagination {
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<usize>,
}
async fn attempt_list(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Path(id): Path<String>,
    Query(q): Query<Pagination>,
) -> Result<Json<Value>> {
    execute(
        app,
        access,
        "attempt-list",
        json!({"task_id":id,"payload":q}),
        None,
    )
    .await
}
async fn attempt_get(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Path((task_id, attempt_id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    execute(
        app,
        access,
        "attempt-get",
        json!({"task_id":task_id,"attempt_id":attempt_id}),
        None,
    )
    .await
}
async fn attempt_begin(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Path(id): Path<String>,
    h: axum::http::HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    execute(
        app,
        access,
        "attempt-begin",
        json!({"task_id":id,"payload":v}),
        key(&h),
    )
    .await
}
async fn execution_checkpoint(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Path(id): Path<String>,
    h: axum::http::HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    execute(
        app,
        access,
        "execution-checkpoint",
        json!({"task_id":id,"payload":v}),
        key(&h),
    )
    .await
}
async fn execution_cancel(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Path(id): Path<String>,
    h: axum::http::HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    execute(
        app,
        access,
        "execution-cancel",
        json!({"task_id":id,"payload":v}),
        key(&h),
    )
    .await
}
async fn execution_delivery(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Path(id): Path<String>,
    h: axum::http::HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    execute(
        app,
        access,
        "execution-delivery",
        json!({"task_id":id,"payload":v}),
        key(&h),
    )
    .await
}
async fn context_get(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    execute(
        app,
        access,
        "context-get",
        json!({"conversation_id":id}),
        None,
    )
    .await
}
async fn context_put(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Path(id): Path<String>,
    h: axum::http::HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    execute(
        app,
        access,
        "context-put",
        json!({"conversation_id":id,"payload":v}),
        key(&h),
    )
    .await
}

async fn eval_routing(
    State(app): State<App>,
    Extension(access): Extension<Access>,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    tokio::task::spawn_blocking(move || {
        let (docs, root) = {
            let store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
            registry::ensure(&store.state.documents, &access, false)?;
            // Authorize every scope at snapshot time, not from cached discovery.
            for ns in &access.read_namespaces {
                let mut scoped = access.clone();
                scoped.namespace = ns.clone();
                registry::ensure(&store.state.documents, &scoped, false)?;
            }
            (store.state.documents.clone(), store.root().to_owned())
        };
        let mut report =
            crate::routing_eval::evaluate(&docs, &access.read_namespaces, &access.namespace, &v)?;
        crate::routing_eval::save(&root, &access.namespace, &mut report);
        report["identity"] = json!({"agent_id":access.agent_id,"namespace":access.namespace});
        Ok(Json(report))
    })
    .await
    .map_err(|_| invalid("路由评测失败"))?
}
