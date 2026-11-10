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
        .route("/admin/api/document", get(document).put(update_document))
        .route("/admin/api/document/preview", post(preview_document))
        .route("/admin/api/route", post(diagnose_route))
        .route("/admin/api/check-template", post(check_template))
        .layer(middleware::from_fn_with_state(app.clone(), authenticate))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(app.clone());
    let sessions = Router::new()
        .route("/admin/api/login", post(login))
        .route("/admin/api/session", get(session))
        .route("/admin/api/logout", post(logout))
        .layer(DefaultBodyLimit::max(4096))
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
        .merge(sessions)
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
    if app.admin_hash.is_none() && !app.admin_sessions.password_enabled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"message":"管理员入口尚未配置登录凭据"}})),
        )
            .into_response();
    }
    // An explicit bearer always takes precedence; an invalid bearer cannot borrow a cookie.
    let bearer_auth = req.headers().contains_key(header::AUTHORIZATION);
    let authorized = if bearer_auth {
        valid_bearer(&app, req.headers())
    } else {
        app.admin_sessions.session(req.headers()).is_some()
    };
    if !authorized {
        return unauthorized();
    }
    if !matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) {
        let valid = if bearer_auth {
            same_origin(req.headers(), None, false)
        } else {
            browser_write(req.headers(), app.admin_sessions.secure_cookie())
        };
        if !valid {
            return Error::Forbidden("管理写入要求同源及有效的浏览器请求头".into()).into_response();
        }
    }
    next.run(req).await
}

fn valid_bearer(app: &App, headers: &axum::http::HeaderMap) -> bool {
    let Some(expected) = &app.admin_hash else {
        return false;
    };
    let Some(bearer) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    sha(bearer)
        .as_bytes()
        .ct_eq(expected.as_bytes())
        .unwrap_u8()
        == 1
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error":{"message":"登录凭据无效或会话已过期，请重新登录"}})),
    )
        .into_response()
}

fn same_origin(headers: &axum::http::HeaderMap, secure: Option<bool>, required: bool) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return !required;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    origin
        .to_str()
        .ok()
        .and_then(|value| url::Url::parse(value).ok().map(|url| (value, url)))
        .is_some_and(|(value, url)| {
            let authority = &url[url::Position::BeforeHost..url::Position::AfterPort];
            matches!(url.scheme(), "http" | "https")
                && secure.is_none_or(|https| url.scheme() == if https { "https" } else { "http" })
                && host == Some(authority)
                && url.username().is_empty()
                && url.password().is_none()
                && value == url.origin().ascii_serialization()
        })
}

fn browser_write(headers: &axum::http::HeaderMap, secure: bool) -> bool {
    same_origin(headers, Some(secure), true)
        && headers
            .get("X-Harness-Admin-Request")
            .is_some_and(|value| value == "1")
        && headers
            .get("Sec-Fetch-Site")
            .is_none_or(|value| value == "same-origin")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    username: Option<String>,
    password: Option<String>,
    #[serde(default)]
    remember: bool,
}

async fn login(
    State(app): State<App>,
    headers: axum::http::HeaderMap,
    Json(input): Json<Login>,
) -> Response {
    if !browser_write(&headers, app.admin_sessions.secure_cookie()) {
        return Error::Forbidden("登录要求同源及有效的浏览器请求头".into()).into_response();
    }
    if !app.admin_sessions.allow_login() {
        return login_busy();
    }
    let verified = if headers.contains_key(header::AUTHORIZATION) {
        input.username.is_none() && input.password.is_none() && valid_bearer(&app, &headers)
    } else {
        let (Some(username), Some(password)) = (input.username, input.password) else {
            return unauthorized();
        };
        if username.len() > 128 || password.is_empty() || password.len() > 1024 {
            return unauthorized();
        }
        let Some(permit) = crate::admin_session::try_verification() else {
            return login_busy();
        };
        let sessions = app.admin_sessions.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            sessions.verify_password(&username, &password)
        })
        .await
        .unwrap_or(false)
    };
    if !verified {
        return unauthorized();
    }
    match app.admin_sessions.issue(&headers, input.remember) {
        Ok((cookie, info)) => ([(header::SET_COOKIE, cookie)], Json(info)).into_response(),
        Err(error) => error.into_response(),
    }
}

fn login_busy() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, "60")],
        Json(json!({"error":{"message":"登录繁忙或尝试过于频繁，请一分钟后重试"}})),
    )
        .into_response()
}

async fn session(State(app): State<App>, headers: axum::http::HeaderMap) -> Response {
    match app.admin_sessions.session(&headers) {
        Some(info) => Json(info).into_response(),
        None => unauthorized(),
    }
}

async fn logout(State(app): State<App>, headers: axum::http::HeaderMap) -> Response {
    if !browser_write(&headers, app.admin_sessions.secure_cookie()) {
        return Error::Forbidden("退出要求同源及有效的浏览器请求头".into()).into_response();
    }
    match app.admin_sessions.revoke(&headers) {
        Ok(cookie) => (
            [(header::SET_COOKIE, cookie)],
            Json(json!({"authenticated":false})),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
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
    namespace: Option<String>,
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
    tokio::task::spawn_blocking(move || {
        let store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
        let limit = q.limit.unwrap_or(20);
        if !(1..=50).contains(&limit) { return Err(invalid("limit必须为1–50")); }
        let offset = q.offset.unwrap_or(0);
        let spaces = if let Some(ns) = q.namespace.filter(|s| !s.is_empty()) {
            registry::space(&store.state.documents, &ns)?;
            vec![ns]
        } else {
            registry::overview(&store.state.documents, &app.principals)?["namespaces"]
                .as_array().ok_or_else(||invalid("空间目录不可用"))?
                .iter().filter_map(|s|s["id"].as_str().map(str::to_owned)).collect()
        };
        let mut rows = Vec::new();
        for ns in &spaces {
            let docs = namespace::view(&store.state.documents, ns, false);
            let mut page_offset = 0;
            loop {
                let page = engine::list(&docs,&json!({"kind":q.kind.as_deref().unwrap_or(""),"project":q.project.as_deref().unwrap_or(""),"limit":50,"offset":page_offset,"q":q.q.as_deref().unwrap_or(""),"status":q.status.as_deref().unwrap_or(""),"sort":q.sort.as_deref().unwrap_or("")}))?;
                if page["mode"] == "SKIP" { return Ok(Json(page)); }
                for mut row in page["documents"].as_array().cloned().unwrap_or_default() {
                    row["namespace"] = json!(ns);
                    rows.push(row);
                }
                if page["remaining"].as_u64().unwrap_or(0) == 0 { break; }
                page_offset += 50;
            }
        }
        rows.sort_by(|a,b| {
            let updated = |v:&Value| chrono::DateTime::parse_from_rfc3339(v["updated_at"].as_str().unwrap_or("")).ok().map(|t|t.timestamp_millis());
            let order = if q.sort.as_deref() == Some("updated_desc") {updated(b).cmp(&updated(a))} else {std::cmp::Ordering::Equal};
            order.then(a["path"].as_str().cmp(&b["path"].as_str())).then(a["namespace"].as_str().cmp(&b["namespace"].as_str()))
        });
        let total = rows.len() as u64;
        let items:Vec<_> = rows.into_iter().skip(usize::try_from(offset).unwrap_or(usize::MAX)).take(limit as usize).collect();
        Ok(Json(json!({"documents":items,"total":total,"offset":offset,"remaining":total.saturating_sub(offset.saturating_add(items.len() as u64)),"namespaces":spaces})))
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
        let kind = crate::document::path_kind(&q.path)?;
        v["kind"] = json!(kind);
        v["editable"] = json!(
            matches!(kind, "project" | "project_topic" | "process" | "template")
                && registry::space(&store.state.documents, &q.namespace)?.status == "ACTIVE"
        );
        if matches!(kind, "project" | "project_topic" | "process" | "template") {
            v["budget"] =
                crate::document_management::budget(&q.path, v["content"].as_str().unwrap_or(""))?;
        }
        if kind != "template" && kind != "log" {
            let (metadata, _) = crate::document::parse(v["content"].as_str().unwrap_or(""))?;
            v["status"] = metadata.get("status").cloned().unwrap_or(Value::Null);
            v["verified_at"] = metadata.get("verified_at").cloned().unwrap_or(Value::Null);
        }
        Ok(Json(v))
    })
    .await
    .map_err(|_| invalid("文档读取失败"))?
}

async fn preview_document(
    State(app): State<App>,
    Json(change): Json<crate::document_management::Change>,
) -> Result<Json<Value>> {
    let documents = {
        let store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
        store.state.documents.clone()
    };
    tokio::task::spawn_blocking(move || {
        crate::document_management::preview(&documents, &change).map(Json)
    })
    .await
    .map_err(|_| invalid("文档预览失败"))?
}

async fn update_document(
    State(app): State<App>,
    headers: axum::http::HeaderMap,
    Json(change): Json<crate::document_management::Change>,
) -> Result<Json<Value>> {
    let original_key = headers
        .get("Idempotency-Key")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| invalid("管理写入必须提供Idempotency-Key"))?;
    let key = namespace::receipt_key("admin", Some("administrator"), original_key)?;
    tokio::task::spawn_blocking(move || {
        let started = std::time::Instant::now();
        let mut lock_wait_ms = None;
        let mut commit_ms = None;
        let result = (|| {
            let mut store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
            lock_wait_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
            let request = json!({"command":"document-update","request":change});
            let result = store.transact(&key, &request, |s| {
                crate::document_management::apply(s, &change)
            });
            commit_ms = store.last_commit_ms;
            result
        })();
        let project = result
            .as_ref()
            .ok()
            .and_then(|v| v["content"].as_str())
            .and_then(|c| crate::document::parse(c).ok())
            .and_then(|(m, _)| {
                m.get("project_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        app.metrics.record(&crate::metrics::Event {
            purpose: Some(crate::metrics::Purpose::Maintenance),
            event_id: Some(uuid::Uuid::new_v4().to_string()),
            service_version: Some(env!("CARGO_PKG_VERSION").into()),
            protocol: Some("admin".into()),
            execution_group_id: None,
            lock_wait_ms,
            commit_ms,
            operation_ms: None,
            timestamp: chrono::Utc::now(),
            namespace: if crate::document::slug(&change.namespace).is_ok() {
                change.namespace.clone()
            } else {
                "unknown".into()
            },
            command: "admin-document-put".into(),
            project,
            handler_ms: started.elapsed().as_secs_f64() * 1000.0,
            success: result.is_ok(),
            retry: result.as_ref().is_ok_and(|v| v["idempotent"] == true),
            error_code: result.as_ref().err().map(|e| e.code().into()),
            validation_reason: result.as_ref().err().and_then(Error::validation_reason),
            route_result: None,
            returned_characters: None,
            handoff_characters: None,
        });
        result.map(Json)
    })
    .await
    .map_err(|_| invalid("文档保存失败"))?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteQuery {
    read_namespaces: Vec<String>,
    project: String,
    scenario: String,
}
async fn diagnose_route(State(app): State<App>, Json(q): Json<RouteQuery>) -> Result<Json<Value>> {
    if q.read_namespaces.is_empty()
        || q.read_namespaces.len() > 8
        || q.read_namespaces
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != q.read_namespaces.len()
    {
        return Err(invalid("选择1–8个不重复空间"));
    }
    crate::document::slug(&q.project)?;
    if q.scenario.chars().count() > 2000 {
        return Err(invalid("诊断问题超过2000字"));
    }
    crate::document::reject_secrets(&q.scenario)?;
    let documents = {
        let store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
        for ns in &q.read_namespaces {
            registry::space(&store.state.documents, ns)?;
        }
        store.state.documents.clone()
    };
    tokio::task::spawn_blocking(move || {
        engine::route_scoped(
            &documents,
            &q.read_namespaces,
            &json!({
                "project":q.project,"scenario":q.scenario,"explain":true,"content":false
            }),
        )
        .map(Json)
    })
    .await
    .map_err(|_| invalid("检索诊断失败"))?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateQuery {
    namespace: String,
    path: String,
    document: String,
}
async fn check_template(
    State(app): State<App>,
    Json(q): Json<TemplateQuery>,
) -> Result<Json<Value>> {
    if q.path.starts_with("namespaces/") || crate::document::path_kind(&q.path)? != "template" {
        return Err(invalid("选择空间内模板路径"));
    }
    let template = {
        let store = app.store.lock().map_err(|_| invalid("存储锁不可用"))?;
        registry::space(&store.state.documents, &q.namespace)?;
        engine::get(
            &namespace::view(&store.state.documents, &q.namespace, false),
            &q.path,
        )?["content"]
            .as_str()
            .ok_or_else(|| invalid("模板正文缺失"))?
            .to_owned()
    };
    tokio::task::spawn_blocking(move || {
        let mut result = crate::template_check::check(&template, &q.document)?;
        result["namespace"] = json!(q.namespace);
        result["path"] = json!(q.path);
        result["template_sha256"] = json!(sha(&template));
        Ok(Json(result))
    })
    .await
    .map_err(|_| invalid("模板检查失败"))?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeQuery {
    namespace: Option<String>,
    days: Option<u32>,
    purpose: Option<String>,
}
async fn runtime(State(app): State<App>, Query(q): Query<RuntimeQuery>) -> Result<Json<Value>> {
    let days = crate::api::stats_days(q.days)?;
    let purpose = q
        .purpose
        .as_deref()
        .filter(|p| !p.is_empty())
        .map(crate::metrics::Purpose::parse)
        .transpose()?;
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
        let mut r = app.metrics.report_filtered(
            days,
            q.namespace.as_deref(),
            None,
            purpose,
            chrono::Utc::now(),
        );
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
        r["build"] = crate::build_info::metadata();
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
        if !q.fresh
            && let Ok(cache) = app.capacity.0.lock()
            && let Some((time, result)) = cache.as_ref().filter(|(t, _)| t.elapsed().as_secs() < 30)
        {
            let mut r = result.clone();
            r["cache_hit"] = json!(true);
            r["cache_age_seconds"] = json!(time.elapsed().as_secs());
            return Ok(Json(r));
        }
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
