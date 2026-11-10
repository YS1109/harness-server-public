//! A tenant selects an entire independently authenticated application and Store.
use crate::{
    admin_session::{AdminSessions, LoginConfig},
    api,
    auth::{self, Principal},
    document::{self, sha},
    error::{Result, conflict, invalid},
    file_lock::FileLock,
    store::Store,
};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderValue, Method, Request, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tower::ServiceExt;

const MODE_MARKER: &str = ".tenancy-mode.json";
const MODE_CONTENT: &[u8] = b"{\"version\":1}\n";
const BODY_LIMIT: usize = 1024 * 1024;
const MAX_TENANTS: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    tenants: Vec<FileTenant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTenant {
    id: String,
    auth_config: Option<PathBuf>,
    token_env: Option<String>,
    admin_token_env: String,
    admin_username: Option<String>,
    admin_password_hash_env: Option<String>,
    #[serde(default = "secure_by_default")]
    secure_cookie: bool,
}

fn secure_by_default() -> bool {
    true
}

/// Programmatic construction also validates credentials and uses fixed Store roots.
/// Deployment files use environment references rather than these in-memory secrets.
pub struct Tenant {
    pub id: String,
    pub principals: Vec<Principal>,
    pub admin_token: String,
    pub admin_login: Option<LoginConfig>,
    /// Applies to password and token-only sessions; never inherits global settings.
    pub secure_cookie: bool,
}

struct Gateway {
    routers: BTreeMap<String, Router>,
    _lock: FileLock,
}

fn environment(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| invalid("Tenant credential environment is missing or invalid"))
}

fn credential(name: &str, env: &impl Fn(&str) -> Result<String>) -> Result<String> {
    let mut chars = name.bytes();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        return Err(invalid("Invalid tenant credential environment reference"));
    }
    let value = env(name)?;
    if value.is_empty() {
        return Err(invalid(
            "Tenant credential environment is missing or invalid",
        ));
    }
    Ok(value)
}

fn resolve(
    config: Config,
    base: &Path,
    env: impl Fn(&str) -> Result<String>,
) -> Result<Vec<Tenant>> {
    if config.tenants.is_empty() || config.tenants.len() > MAX_TENANTS {
        return Err(invalid("Tenant configuration must contain 1-64 tenants"));
    }
    config
        .tenants
        .into_iter()
        .map(|config| {
            document::slug(&config.id).map_err(|_| invalid("Invalid tenant configuration"))?;
            let principals = match (config.auth_config, config.token_env) {
                (Some(path), None) => {
                    if path.as_os_str().is_empty() {
                        return Err(invalid("Invalid tenant authentication configuration"));
                    }
                    auth::load(&base.join(path))?
                }
                (None, Some(name)) => auth::legacy(vec![credential(&name, &env)?]),
                _ => return Err(invalid("Each tenant requires auth_config or token_env")),
            };
            let admin_login = match (config.admin_username, config.admin_password_hash_env) {
                (None, None) => None,
                (Some(username), Some(name)) => {
                    let mut login = LoginConfig::new(username, credential(&name, &env)?)?;
                    login.secure_cookie = config.secure_cookie;
                    Some(login)
                }
                _ => {
                    return Err(invalid(
                        "Tenant administrator login configuration is incomplete",
                    ));
                }
            };
            Ok(Tenant {
                id: config.id,
                principals,
                admin_token: credential(&config.admin_token_env, &env)?,
                admin_login,
                secure_cookie: config.secure_cookie,
            })
        })
        .collect()
}

/// Loads all tenant secrets, validates the complete configuration, then opens Stores.
pub fn router(data_root: &Path, config_path: &Path) -> Result<Router> {
    let mut bytes = Vec::new();
    File::open(config_path)?
        .take(BODY_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > BODY_LIMIT {
        return Err(invalid("Tenant configuration exceeds its size limit"));
    }
    let config: Config = serde_json::from_slice(&bytes).map_err(|_| {
        invalid("Invalid tenant configuration; secrets must use environment references")
    })?;
    let tenants = resolve(
        config,
        config_path.parent().unwrap_or_else(|| Path::new(".")),
        environment,
    )?;
    router_with_tenants(data_root, tenants)
}

fn validate(tenants: &[Tenant]) -> Result<()> {
    if tenants.is_empty() || tenants.len() > MAX_TENANTS {
        return Err(invalid("Tenant configuration must contain 1-64 tenants"));
    }
    let mut ids = BTreeSet::new();
    let mut credentials = BTreeSet::new();
    let mut passwords = BTreeSet::new();
    for tenant in tenants {
        document::slug(&tenant.id).map_err(|_| invalid("Invalid tenant configuration"))?;
        if !ids.insert(&tenant.id) {
            return Err(invalid("Duplicate tenant configuration"));
        }
        // The child validates and hashes its own copy during application construction.
        auth::validate(&mut tenant.principals.clone())?;
        for token in tenant
            .principals
            .iter()
            .map(|p| p.token.as_str())
            .chain(std::iter::once(tenant.admin_token.as_str()))
        {
            if !(32..=512).contains(&token.len())
                || token.chars().any(char::is_whitespace)
                || !credentials.insert(sha(token))
            {
                return Err(invalid("Tenant credentials must be valid and distinct"));
            }
        }
        // Validate every policy before opening Stores, including token-only tenants.
        AdminSessions::with_policy(tenant.admin_login.clone(), tenant.secure_cookie)?;
        if let Some(login) = &tenant.admin_login
            && !passwords.insert(sha(&login.password_hash))
        {
            return Err(invalid("Tenant administrator credentials must be distinct"));
        }
    }
    Ok(())
}

fn reject_symlinks(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(invalid("Tenant storage does not accept symbolic links"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// A multi-tenant data root must never accidentally be opened as a legacy Store.
pub fn ensure_single_tenant(data_root: &Path) -> Result<()> {
    match fs::symlink_metadata(data_root.join(MODE_MARKER)) {
        Ok(_) => Err(invalid(
            "This is a multi-tenant data root; select a tenant Store explicitly",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn open_lock(root: &Path) -> Result<FileLock> {
    let path = root.join(".tenancy.lock");
    reject_symlinks(&path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    FileLock::acquire(file).map_err(|_| conflict("Tenant data root is already in use"))
}

fn mark_mode(root: &Path) -> Result<()> {
    let path = root.join(MODE_MARKER);
    reject_symlinks(&path)?;
    if path.exists() {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MODE_CONTENT.len() as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes != MODE_CONTENT {
            return Err(invalid("Invalid tenant data root mode marker"));
        }
        return Ok(());
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(MODE_CONTENT)?;
    file.sync_all()?;
    #[cfg(unix)]
    File::open(root)?.sync_all()?;
    Ok(())
}

pub fn router_with_tenants(data_root: &Path, mut tenants: Vec<Tenant>) -> Result<Router> {
    validate(&tenants)?;
    reject_symlinks(data_root)?;
    // Never silently ignore or import an existing root-level single-tenant store.
    for name in ["HEAD", "revisions", ".writer.lock", ".runtime"] {
        if fs::symlink_metadata(data_root.join(name)).is_ok() {
            return Err(invalid(
                "Legacy Store at data root; explicit per-tenant migration is required",
            ));
        }
    }
    fs::create_dir_all(data_root)?;
    let root = data_root.canonicalize()?;
    let lock = open_lock(&root)?;
    let stores_root = root.join("tenants");
    reject_symlinks(&stores_root)?;
    // Check all paths before creating a marker or opening the first Store.
    for tenant in &tenants {
        reject_symlinks(&stores_root.join(&tenant.id))?;
    }
    mark_mode(&root)?;
    tenants.sort_by(|a, b| a.id.cmp(&b.id));
    let mut stores = Vec::with_capacity(tenants.len());
    let mut roots = BTreeSet::new();
    // Acquire every Store lock in the same order before initializing any application.
    for tenant in tenants {
        let store = Store::open(stores_root.join(&tenant.id))?;
        if !roots.insert(store.root().to_path_buf()) {
            return Err(invalid("Tenant Store roots must be distinct"));
        }
        stores.push((tenant, store));
    }
    let mut routers = BTreeMap::new();
    for (tenant, store) in stores {
        let sessions = AdminSessions::with_policy(tenant.admin_login, tenant.secure_cookie)?;
        let app = api::router_with_admin_sessions(
            store,
            tenant.principals,
            Some(tenant.admin_token),
            sessions,
        )?;
        routers.insert(tenant.id, app);
    }
    Ok(Router::new()
        .fallback(dispatch)
        .with_state(Arc::new(Gateway {
            routers,
            _lock: lock,
        })))
}

fn error(status: StatusCode) -> Response {
    let (code, message) = match status {
        StatusCode::NOT_FOUND => ("not_found", "Not found"),
        StatusCode::PAYLOAD_TOO_LARGE => ("validation", "Request body exceeds its size limit"),
        _ => ("validation", "Invalid tenant selector"),
    };
    (
        status,
        Json(json!({"error":{"code":code,"message":message}})),
    )
        .into_response()
}

fn selector_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().replace(['-', '_'], "").as_str(),
        "tenant" | "tenantid" | "xharnesstenant"
    )
}

fn tenant_path(path: &str) -> bool {
    let path = path.trim_start_matches('/');
    path == "tenants" || path.starts_with("tenants/")
}

fn contains_selector(value: &Value) -> bool {
    value.as_object().is_some_and(|fields| {
        fields.iter().any(|(key, value)| {
            selector_key(key) || (key == "path" && value.as_str().is_some_and(tenant_path))
        })
    })
}

fn secure(mut response: Response) -> Response {
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
        ),
    ] {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static(name),
            axum::http::HeaderValue::from_static(value),
        );
    }
    response
}

async fn dispatch(State(gateway): State<Arc<Gateway>>, req: Request<Body>) -> Response {
    secure(dispatch_inner(&gateway, req).await)
}

async fn dispatch_inner(gateway: &Gateway, mut req: Request<Body>) -> Response {
    if let Some(path) = req.uri().path().strip_prefix("/v3/tenant/") {
        if !matches!(
            *req.method(),
            Method::POST | Method::PUT | Method::PATCH | Method::DELETE
        ) || !["v1/", "v2/"].iter().any(|prefix| path.starts_with(prefix))
        {
            return error(StatusCode::NOT_FOUND);
        }
        let mut parts = req.uri().clone().into_parts();
        let suffix = req
            .uri()
            .query()
            .map(|query| format!("?{query}"))
            .unwrap_or_default();
        let Ok(path) = format!("/{path}{suffix}").parse() else {
            return error(StatusCode::BAD_REQUEST);
        };
        parts.path_and_query = Some(path);
        let Ok(uri) = Uri::from_parts(parts) else {
            return error(StatusCode::BAD_REQUEST);
        };
        *req.uri_mut() = uri;
    }
    let path = req.uri().path();
    if matches!(*req.method(), Method::GET | Method::HEAD) {
        let common = match path {
            "/healthz" => Some(Json(json!({"status":"ok"})).into_response()),
            "/admin" | "/admin/" => Some(
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    include_str!("console.html"),
                )
                    .into_response(),
            ),
            "/admin/console.css" => Some(
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("console.css"),
                )
                    .into_response(),
            ),
            "/admin/console.js" => Some(
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("console.js"),
                )
                    .into_response(),
            ),
            _ => None,
        };
        if let Some(mut response) = common {
            if req.method() == Method::HEAD {
                *response.body_mut() = Body::empty();
            }
            return response;
        }
    }
    if !["/v1", "/v2", "/admin/api"]
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
    {
        return error(StatusCode::NOT_FOUND);
    }
    let mut selectors = req.headers().get_all("X-Harness-Tenant").iter();
    let Some(selector) = selectors.next() else {
        return error(StatusCode::NOT_FOUND);
    };
    let Ok(id) = selector.to_str() else {
        return error(StatusCode::BAD_REQUEST);
    };
    if selectors.next().is_some() || document::slug(id).is_err() {
        return error(StatusCode::BAD_REQUEST);
    }
    let Some(app) = gateway.routers.get(id) else {
        return error(StatusCode::NOT_FOUND);
    };
    let selected = HeaderValue::from_str(id).expect("validated ASCII tenant slug");
    if url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes())
        .any(|(key, value)| selector_key(&key) || (key == "path" && tenant_path(&value)))
    {
        return error(StatusCode::BAD_REQUEST);
    }
    let (mut parts, body) = req.into_parts();
    parts.headers.remove("X-Harness-Tenant");
    let bytes = match to_bytes(body, BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE),
    };
    if serde_json::from_slice::<Value>(&bytes).is_ok_and(|value| contains_selector(&value)) {
        return error(StatusCode::BAD_REQUEST);
    }
    // The selected application's own authentication remains the only authority.
    match app
        .clone()
        .oneshot(Request::from_parts(parts, Body::from(bytes)))
        .await
    {
        Ok(mut response) => {
            response.headers_mut().insert("x-harness-tenant", selected);
            response
        }
        Err(infallible) => match infallible {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(value: Value) -> Result<Vec<Tenant>> {
        let config = serde_json::from_value(value).map_err(|_| invalid("Invalid configuration"))?;
        resolve(config, Path::new("."), |name| match name {
            "AGENT_TOKEN" => Ok("test-agent-token-000000000000000000000".into()),
            "ADMIN_TOKEN" => Ok("test-admin-token-000000000000000000000".into()),
            "EMPTY" => Ok(String::new()),
            _ => Err(invalid("Missing environment")),
        })
    }

    #[test]
    fn configuration_fails_closed_without_echoing_secrets_or_environment_names() {
        let valid = json!({"tenants":[{"id":"acme","token_env":"AGENT_TOKEN","admin_token_env":"ADMIN_TOKEN"}]});
        assert!(config(valid.clone()).unwrap()[0].secure_cookie);
        let mut http = valid.clone();
        http["tenants"][0]["secure_cookie"] = json!(false);
        let http = config(http).unwrap();
        assert!(!http[0].secure_cookie && http[0].admin_login.is_none());
        for field in ["id", "token_env", "admin_token_env"] {
            let mut missing = valid.clone();
            missing["tenants"][0].as_object_mut().unwrap().remove(field);
            assert!(config(missing).is_err());
        }
        for (field, value) in [
            ("id", json!("../escape")),
            ("token_env", json!("MISSING_ENV")),
            ("token_env", json!("EMPTY")),
            ("token_env", json!("123INVALID")),
            ("token", json!("plaintext-secret")),
            ("data_root", json!("/other/root")),
            ("auth_config", json!("also-configured.json")),
            ("admin_username", json!("admin")),
            ("admin_password_hash_env", json!("HASH")),
            ("secure_cookie", json!("false")),
        ] {
            let mut invalid = valid.clone();
            invalid["tenants"][0][field] = value;
            let error = config(invalid).err().unwrap().to_string();
            assert!(!error.contains("MISSING_ENV"));
            assert!(!error.contains("plaintext-secret"));
        }
    }
}
