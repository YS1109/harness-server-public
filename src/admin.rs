//! Same-origin administration. Admin credentials are independent of Agent credentials.
use crate::{
    api::App,
    document::sha,
    engine,
    error::{Error, Result, invalid},
    namespace, registry,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;

pub(crate) fn router(app: App) -> Router {
    let private = Router::new()
        .route("/admin/api/overview", get(overview))
        .route("/admin/api/runtime", get(runtime))
        .route("/admin/api/storage", get(storage))
        .route("/admin/api/routing-evaluation", get(routing_evaluation))
        .route("/admin/api/executions", get(executions))
        .route("/admin/api/execution", get(execution))
        .route("/admin/api/namespaces", post(create))
        .route("/admin/api/namespaces/{id}", put(update_space))
        .route("/admin/api/agents/{id}", put(update_agent))
        .route("/admin/api/documents", get(documents))
        .route("/admin/api/document", get(document))
        .layer(middleware::from_fn_with_state(app.clone(), authenticate))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(app);
    Router::new()
        .route(
            "/admin",
            get(|| async { asset("text/html; charset=utf-8", include_str!("console.html")) }),
        )
        .route(
            "/admin/console.css",
            get(|| async { asset("text/css; charset=utf-8", include_str!("console.css")) }),
        )
        .route(
            "/admin/console.js",
            get(|| async { asset("text/javascript; charset=utf-8", include_str!("console.js")) }),
        )
        .merge(private)
        .layer(middleware::from_fn(security))
}
fn asset(kind: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, kind)], body).into_response()
}
async fn security(req: Request<Body>, next: Next) -> Response {
    let mut r = next.run(req).await;
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
        ),
    ] {
        r.headers_mut().insert(
            axum::http::HeaderName::from_static(name),
            axum::http::HeaderValue::from_static(value),
        );
    }
    r
}
async fn authenticate(State(app): State<App>, req: Request<Body>, next: Next) -> Response {
    let Some(expected) = &app.admin_hash else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"message":"管理员入口尚未配置HARNESS_ADMIN_TOKEN"}})),
        )
            .into_response();
    };
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    if sha(bearer)
        .as_bytes()
        .ct_eq(expected.as_bytes())
        .unwrap_u8()
        != 1
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"message":"需要独立管理员凭据"}})),
        )
            .into_response();
    }
    // Browsers send Origin on mutations. Reject cross-origin, including opaque origins.
    if req.method() != axum::http::Method::GET
        && let Some(origin) = req.headers().get(header::ORIGIN)
    {
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok());
        let valid = origin
            .to_str()
            .ok()
            .and_then(|s| url::Url::parse(s).ok())
            .is_some_and(|u| {
                let authority = &u[url::Position::BeforeHost..url::Position::AfterPort];
                matches!(u.scheme(), "http" | "https")
                    && host == Some(authority)
                    && u.username().is_empty()
                    && u.password().is_none()
            });
        if !valid {
            return Error::Forbidden("管理写入要求同源".into()).into_response();
        }
    }
    next.run(req).await
}
async fn overview(State(app): State<App>) -> Result<Json<Value>> {
    tokio::task::spawn_blocking(move || {
        let store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
        Ok(Json(registry::overview(
            &store.state.documents,
            &app.principals,
        )?))
    })
    .await
    .map_err(|_| invalid("管理读取失败"))?
}
async fn mutation(
    app: App,
    operation: &'static str,
    id: String,
    h: axum::http::HeaderMap,
    v: Value,
) -> Result<Json<Value>> {
    let key = h
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| invalid("管理写入必须提供Idempotency-Key"))?;
    let key = namespace::receipt_key("admin", Some("administrator"), key)?;
    tokio::task::spawn_blocking(move || {
        let mut store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
        let request = json!({"command":operation,"target":id,"request":v});
        store
            .transact(&key, &request, |state| {
                registry::mutate(&mut state.documents, operation, &id, &v)
            })
            .map(Json)
    })
    .await
    .map_err(|_| invalid("管理保存失败"))?
}
async fn create(
    State(app): State<App>,
    h: axum::http::HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    mutation(app, "namespace-create", String::new(), h, v).await
}
async fn update_space(
    State(app): State<App>,
    Path(id): Path<String>,
    h: axum::http::HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    crate::document::slug(&id)?;
    mutation(app, "namespace-update", id, h, v).await
}
async fn update_agent(
    State(app): State<App>,
    Path(id): Path<String>,
    h: axum::http::HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    crate::document::slug(&id)?;
    mutation(app, "agent-update", id, h, v).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    namespace: String,
    kind: Option<String>,
    q: Option<String>,
    status: Option<String>,
    sort: Option<String>,
    project: Option<String>,
    limit: Option<u64>,
    offset: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetQuery {
    namespace: String,
    path: String,
}
async fn documents(State(app): State<App>, Query(q): Query<ListQuery>) -> Result<Json<Value>> {
    tokio::task::spawn_blocking(move || {let store=app.store.lock().map_err(|_|invalid("存储锁不可用"))?;registry::space(&store.state.documents,&q.namespace)?;
        let docs=namespace::view(&store.state.documents,&q.namespace,false);
        let mut v=engine::list(&docs,&json!({"kind":q.kind.unwrap_or_default(),"project":q.project.unwrap_or_default(),"limit":q.limit.unwrap_or(20),"offset":q.offset.unwrap_or(0),"q":q.q.unwrap_or_default(),"status":q.status.unwrap_or_default(),"sort":q.sort.unwrap_or_default()}))?;
        namespace::annotate(&mut v,&store.state.documents,&q.namespace);Ok(Json(v))
    }).await.map_err(|_|invalid("文档读取失败"))?
}
async fn document(State(app): State<App>, Query(q): Query<GetQuery>) -> Result<Json<Value>> {
    tokio::task::spawn_blocking(move || {
        let store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
        registry::space(&store.state.documents, &q.namespace)?;
        if q.path.starts_with("namespaces/") || q.path.starts_with("control/") {
            return Err(invalid("只允许空间内逻辑路径"));
        }
        let mut v = engine::get(
            &namespace::view(&store.state.documents, &q.namespace, false),
            &q.path,
        )?;
        v["namespace"] = json!(q.namespace);
        Ok(Json(v))
    })
    .await
    .map_err(|_| invalid("文档读取失败"))?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeQuery {
    namespace: Option<String>,
    days: Option<u32>,
}
async fn runtime(State(app): State<App>, Query(q): Query<RuntimeQuery>) -> Result<Json<Value>> {
    let days = crate::api::stats_days(q.days)?;
    tokio::task::spawn_blocking(move || {
        let (storage, root, names) = {
            let store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
            if let Some(ns) = &q.namespace {
                registry::space(&store.state.documents, ns)?;
            }
            (
                store.usage_snapshot(),
                store.root().to_owned(),
                registry::overview(&store.state.documents, &app.principals)?["namespaces"].clone(),
            )
        };
        let mut r = app
            .metrics
            .report(days, q.namespace.as_deref(), None, chrono::Utc::now());
        if let Some(groups) = r["namespaces"].as_array_mut() {
            for g in groups {
                g["name"] = names
                    .as_array()
                    .and_then(|a| a.iter().find(|s| s["id"] == g["id"]))
                    .map(|s| s["name"].clone())
                    .unwrap_or_else(|| g["id"].clone());
            }
        }
        r["version"] = json!(env!("CARGO_PKG_VERSION"));
        r["uptime_seconds"] = json!(app.metrics.started.elapsed().as_secs());
        r["routing_evaluation"] = crate::routing_eval::latest(&root, q.namespace.as_deref());
        r["storage"] = storage;
        r["storage_scope"] = json!("存储容量为全局；namespace筛选仅过滤统计事件");
        Ok(Json(r))
    })
    .await
    .map_err(|_| invalid("运行概览读取失败"))?
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionQuery {
    namespace: String,
    status: Option<String>,
    project: Option<String>,
    q: Option<String>,
    limit: Option<u64>,
    offset: Option<u64>,
}
async fn executions(
    State(app): State<App>,
    Query(q): Query<ExecutionQuery>,
) -> Result<Json<Value>> {
    tokio::task::spawn_blocking(move|| {
        let store=app.store.lock().map_err(|_|invalid("存储锁不可用"))?;registry::space(&store.state.documents,&q.namespace)?;
        let docs=namespace::view(&store.state.documents,&q.namespace,false);
        let mut query=json!({"sort":"updated_desc","q":q.q.unwrap_or_default(),"limit":q.limit.unwrap_or(20),"offset":q.offset.unwrap_or(0)});
        if let Some(s)=q.status.filter(|s|!s.is_empty()){query["status"]=json!(s);}
        if let Some(p)=q.project.filter(|s|!s.is_empty()){query["project"]=json!(p);}
        crate::execution::list(&docs,&query).map(Json)
    }).await.map_err(|_|invalid("执行列表读取失败"))?
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionGetQuery {
    namespace: String,
    task_id: String,
}
async fn execution(
    State(app): State<App>,
    Query(q): Query<ExecutionGetQuery>,
) -> Result<Json<Value>> {
    tokio::task::spawn_blocking(move || {
        let store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
        registry::space(&store.state.documents, &q.namespace)?;
        let docs = namespace::view(&store.state.documents, &q.namespace, false);
        let mut response = crate::execution::get(&docs, &q.task_id)?;
        response["attempts"] =
            crate::execution::list_attempts(&docs, &q.task_id, &json!({"limit":20,"offset":0}))?;
        if let Some(rows) = response["attempts"]["attempts"].as_array_mut() {
            for row in rows {
                let attempt = crate::execution::get_attempt(
                    &docs,
                    &q.task_id,
                    row["attempt_id"].as_str().unwrap_or(""),
                )?;
                *row = attempt["attempt"].clone();
            }
        }
        Ok(Json(response))
    })
    .await
    .map_err(|_| invalid("执行详情读取失败"))?
}

#[derive(Default)]
pub(crate) struct StorageCache(std::sync::Mutex<Option<(std::time::Instant, Value)>>);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageQuery {
    #[serde(default)]
    fresh: bool,
}
async fn storage(State(app): State<App>, Query(q): Query<StorageQuery>) -> Result<Json<Value>> {
    tokio::task::spawn_blocking(move || {
        if !q.fresh {if let Ok(cache)=app.capacity.0.lock(){if let Some((time,result))=cache.as_ref().filter(|(t,_)|t.elapsed().as_secs()<30){let mut r=result.clone();r["cache_hit"]=json!(true);r["cache_age_seconds"]=json!(time.elapsed().as_secs());return Ok(Json(r));}}}
        let root={let store=app.store.lock().map_err(|_|invalid("存储锁不可用"))?;store.root().to_owned()};
        let result=json!({"storage":crate::store::Store::disk_usage(&root)?,"sample_at":chrono::Utc::now(),"cache_hit":false,"cache_age_seconds":0});
        if let Ok(mut cache)=app.capacity.0.lock(){*cache=Some((std::time::Instant::now(),result.clone()));}
        Ok(Json(result))
    }).await.map_err(|_|invalid("磁盘容量读取失败"))?
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvaluationQuery {
    namespace: Option<String>,
}
async fn routing_evaluation(
    State(app): State<App>,
    Query(q): Query<EvaluationQuery>,
) -> Result<Json<Value>> {
    let root = {
        let store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
        if let Some(ns) = &q.namespace {
            registry::space(&store.state.documents, ns)?;
        }
        store.root().to_owned()
    };
    tokio::task::spawn_blocking(move || {
        Ok(Json(
            json!({"latest":crate::routing_eval::latest(&root,q.namespace.as_deref())}),
        ))
    })
    .await
    .map_err(|_| invalid("评测读取失败"))?
}
