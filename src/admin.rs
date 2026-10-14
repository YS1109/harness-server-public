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
        let mut v=engine::list(&docs,&json!({"kind":q.kind.unwrap_or_default(),"project":q.project.unwrap_or_default(),"limit":q.limit.unwrap_or(20),"offset":q.offset.unwrap_or(0)}))?;
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
