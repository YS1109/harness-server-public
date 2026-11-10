#![cfg(feature = "service")]

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode, header},
};
use harness_server::{
    admin_session::{AdminSessions, LoginConfig, hash_password},
    api, auth,
    store::Store,
};
use serde_json::{Value, json};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};
use tower::ServiceExt;

const AGENT: &str = "test-session-agent-000000000000000000000";
const ADMIN: &str = "test-session-admin-000000000000000000000";
const ORIGIN: [(&str, &str); 2] = [
    ("Origin", "http://localhost:8080"),
    ("X-Harness-Admin-Request", "1"),
];
static PASSWORD_TEST: AtomicBool = AtomicBool::new(false);

struct PasswordTest;
impl Drop for PasswordTest {
    fn drop(&mut self) {
        PASSWORD_TEST.store(false, Ordering::Release);
    }
}
async fn password_test() -> PasswordTest {
    // Separate Tokio test runtimes share the process-wide password admission limit.
    while PASSWORD_TEST
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        tokio::task::yield_now().await;
    }
    PasswordTest
}

fn credentials() -> &'static (String, String) {
    static CREDENTIALS: OnceLock<(String, String)> = OnceLock::new();
    CREDENTIALS.get_or_init(|| {
        let password = uuid::Uuid::new_v4().to_string();
        let hash = hash_password(&password).unwrap();
        (password, hash)
    })
}

fn config(secure_cookie: bool) -> LoginConfig {
    LoginConfig {
        username: "administrator".into(),
        password_hash: credentials().1.clone(),
        secure_cookie,
    }
}

fn setup(login: Option<LoginConfig>, token: bool) -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let app = api::router_with_admin_login(
        Store::open(dir.path()).unwrap(),
        auth::legacy(vec![AGENT.into()]),
        token.then(|| ADMIN.into()),
        login,
    )
    .unwrap();
    (dir, app)
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "localhost:8080")
        .header("Content-Type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, body)
}

fn cookie(headers: &HeaderMap) -> String {
    headers[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn login(app: &Router, remember: bool) -> (HeaderMap, Value) {
    let (status, headers, body) = call(
        app,
        "POST",
        "/admin/api/login",
        json!({"username":"administrator","password":credentials().0,"remember":remember}),
        &ORIGIN,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["authenticated"], true);
    assert!(!body.to_string().contains(&credentials().0));
    assert!(!body.to_string().contains(ADMIN));
    assert!(!body.to_string().contains("$argon2"));
    (headers, body)
}

#[test]
fn hashes_are_argon2id_and_configuration_rejects_weak_or_excessive_parameters() {
    let hash = &credentials().1;
    assert!(hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    assert!(hash_password("short").is_err());
    assert!(hash_password(&"x".repeat(1025)).is_err());
    assert!(AdminSessions::new(Some(config(false))).is_ok());
    for bad in [
        hash.replace("argon2id", "argon2i"),
        hash.replace("m=19456", "m=8192"),
        hash.replace("m=19456", "m=1048576"),
        hash.replace("t=2", "t=100"),
        hash.replace("p=1", "p=2"),
        "plaintext".into(),
    ] {
        let mut invalid = config(false);
        invalid.password_hash = bad;
        assert!(AdminSessions::new(Some(invalid)).is_err());
    }
    assert!(
        LoginConfig::new("administrator", hash)
            .unwrap()
            .secure_cookie
    );
}

#[tokio::test]
async fn password_only_login_resumes_rotates_and_revokes_without_returning_secrets() {
    let _password_test = password_test().await;
    let (_dir, app) = setup(Some(config(false)), false);
    let (headers, info) = login(&app, false).await;
    let first = cookie(&headers);
    let attributes = headers[header::SET_COOKIE].to_str().unwrap();
    for attribute in ["Path=/admin", "HttpOnly", "SameSite=Strict"] {
        assert!(attributes.contains(attribute));
    }
    assert!(!attributes.contains("Max-Age"));
    assert!(!attributes.contains("Secure"));
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let original_expiry =
        chrono::DateTime::parse_from_rfc3339(info["expires_at"].as_str().unwrap()).unwrap();
    let hours = original_expiry
        .signed_duration_since(chrono::Utc::now())
        .num_hours();
    assert!((11..=12).contains(&hours));
    for _ in 0..2 {
        let (status, _, resumed) = call(
            &app,
            "GET",
            "/admin/api/session",
            Value::Null,
            &[("Cookie", &first)],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(resumed, info, "reading a session never extends expiry");
    }
    assert_eq!(
        call(
            &app,
            "GET",
            "/admin/api/overview",
            Value::Null,
            &[("Cookie", &first)],
        )
        .await
        .0,
        StatusCode::OK
    );
    let headers = [ORIGIN[0], ORIGIN[1], ("Cookie", first.as_str())];
    let (status, rotated_headers, remembered) = call(
        &app,
        "POST",
        "/admin/api/login",
        json!({"username":"administrator","password":credentials().0,"remember":true}),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        rotated_headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=2592000")
    );
    let remembered_expiry =
        chrono::DateTime::parse_from_rfc3339(remembered["expires_at"].as_str().unwrap()).unwrap();
    assert!(remembered_expiry - original_expiry > chrono::Duration::days(29));
    assert_eq!(
        call(
            &app,
            "GET",
            "/admin/api/session",
            Value::Null,
            &[("Cookie", &first)],
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let current = cookie(&rotated_headers);
    let headers = [ORIGIN[0], ORIGIN[1], ("Cookie", current.as_str())];
    for _ in 0..2 {
        let (status, headers, body) =
            call(&app, "POST", "/admin/api/logout", json!({}), &headers).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"authenticated":false}));
        assert!(
            headers[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
    }
    for path in ["/admin/api/session", "/admin/api/overview"] {
        assert_eq!(
            call(&app, "GET", path, Value::Null, &[("Cookie", &current)],)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn credentials_fail_closed_and_login_attempts_are_bounded() {
    let _password_test = password_test().await;
    let (_dir, app) = setup(Some(config(false)), true);
    let mut failures = vec![];
    for body in [
        json!({"username":"unknown","password":credentials().0}),
        json!({"username":"administrator","password":"wrong"}),
    ] {
        let (status, headers, body) = call(&app, "POST", "/admin/api/login", body, &ORIGIN).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!headers.contains_key(header::SET_COOKIE));
        failures.push(body);
    }
    assert_eq!(
        failures[0], failures[1],
        "unknown users have the same failure response"
    );
    let agent = format!("Bearer {AGENT}");
    let headers = [ORIGIN[0], ORIGIN[1], ("Authorization", agent.as_str())];
    assert_eq!(
        call(&app, "POST", "/admin/api/login", json!({}), &headers,)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    for _ in 3..10 {
        assert_eq!(
            call(&app, "POST", "/admin/api/login", json!({}), &ORIGIN,)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    let (status, headers, _) = call(
        &app,
        "POST",
        "/admin/api/login",
        json!({"username":"administrator","password":credentials().0}),
        &ORIGIN,
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(headers[header::RETRY_AFTER], "60");
    assert_eq!(
        call(&app, "POST", "/admin/api/register", json!({}), &ORIGIN,)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn legacy_bearer_and_explicit_secure_token_exchange_remain_independent() {
    let (_dir, app) = setup(None, true);
    let bearer = format!("Bearer {ADMIN}");
    assert_eq!(
        call(
            &app,
            "GET",
            "/admin/api/overview",
            Value::Null,
            &[("Authorization", &bearer)],
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/admin/api/namespaces",
            json!({"name":"Bearer space"}),
            &[
                ("Authorization", &bearer),
                ("Idempotency-Key", "bearer-no-origin")
            ],
        )
        .await
        .0,
        StatusCode::OK
    );
    let headers = [
        ("Origin", "https://localhost:8080"),
        ORIGIN[1],
        ("Authorization", bearer.as_str()),
    ];
    let (status, headers, body) = call(
        &app,
        "POST",
        "/admin/api/login",
        json!({"remember":true}),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("; Secure")
    );
    assert!(!body.to_string().contains(ADMIN));
    let session_cookie = cookie(&headers);
    assert_eq!(
        call(
            &app,
            "GET",
            "/admin/api/session",
            Value::Null,
            &[("Cookie", &session_cookie)],
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/admin/api/overview",
            Value::Null,
            &[
                ("Cookie", &session_cookie),
                ("Authorization", "Bearer invalid")
            ],
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/admin/api/login",
            json!({}),
            &[
                ORIGIN[0],
                ORIGIN[1],
                ("Authorization", &bearer),
                ("X-Forwarded-Proto", "https")
            ],
        )
        .await
        .0,
        StatusCode::FORBIDDEN,
        "proxy headers cannot change trusted transport policy"
    );
    let (_dir, disabled) = setup(None, false);
    assert_eq!(
        call(&disabled, "GET", "/admin/api/overview", Value::Null, &[],)
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn cookie_writes_and_login_require_origin_custom_header_and_same_site_fetch() {
    let _password_test = password_test().await;
    let (_dir, app) = setup(Some(config(false)), false);
    let (headers, _) = login(&app, false).await;
    let session_cookie = cookie(&headers);
    let cases = [
        vec![],
        vec![ORIGIN[0]],
        vec![ORIGIN[1]],
        vec![("Origin", "null"), ORIGIN[1]],
        vec![("Origin", "http://evil.invalid"), ORIGIN[1]],
        vec![("Origin", "https://localhost:8080"), ORIGIN[1]],
        vec![("Origin", "http://localhost:8080/path"), ORIGIN[1]],
        vec![ORIGIN[0], ORIGIN[1], ("Sec-Fetch-Site", "cross-site")],
        vec![ORIGIN[0], ORIGIN[1], ("Sec-Fetch-Site", "same-site")],
    ];
    for mut headers in cases {
        headers.push(("Cookie", &session_cookie));
        for path in [
            "/admin/api/namespaces",
            "/admin/api/logout",
            "/admin/api/login",
        ] {
            assert_eq!(
                call(&app, "POST", path, json!({"remember":false}), &headers,)
                    .await
                    .0,
                StatusCode::FORBIDDEN,
                "{path} {headers:?}"
            );
        }
    }
    // A blocked logout must not revoke the session.
    assert_eq!(
        call(
            &app,
            "GET",
            "/admin/api/session",
            Value::Null,
            &[("Cookie", &session_cookie)],
        )
        .await
        .0,
        StatusCode::OK
    );
    let headers = [
        ORIGIN[0],
        ORIGIN[1],
        ("Cookie", session_cookie.as_str()),
        ("Sec-Fetch-Site", "same-origin"),
        ("Idempotency-Key", "cookie-create"),
    ];
    assert_eq!(
        call(
            &app,
            "POST",
            "/admin/api/namespaces",
            json!({"name":"Cookie space"}),
            &headers,
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn separate_routers_cannot_consume_or_overwrite_each_others_sessions() {
    let _password_test = password_test().await;
    let (_one, one) = setup(Some(config(false)), false);
    let (_two, two) = setup(Some(config(false)), false);
    let first = cookie(&login(&one, false).await.0);
    let second = cookie(&login(&two, false).await.0);
    assert_ne!(first.split('=').next(), second.split('=').next());
    assert_eq!(
        call(
            &two,
            "GET",
            "/admin/api/overview",
            Value::Null,
            &[("Cookie", &first)],
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &two,
            "GET",
            "/admin/api/overview",
            Value::Null,
            &[("Cookie", &first), ("X-Harness-Tenant", "two")],
        )
        .await
        .0,
        StatusCode::FORBIDDEN,
        "the standalone router rejects selectors not consumed by a gateway"
    );
    let both = format!("{first}; {second}");
    for app in [&one, &two] {
        assert_eq!(
            call(
                app,
                "GET",
                "/admin/api/session",
                Value::Null,
                &[("Cookie", &both)],
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let duplicate = format!("{first}; {first}");
    assert_eq!(
        call(
            &one,
            "GET",
            "/admin/api/session",
            Value::Null,
            &[("Cookie", &duplicate)],
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let headers = [ORIGIN[0], ORIGIN[1], ("Cookie", both.as_str())];
    assert_eq!(
        call(&one, "POST", "/admin/api/logout", json!({}), &headers,)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &two,
            "GET",
            "/admin/api/session",
            Value::Null,
            &[("Cookie", &both)],
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn tenant_gateway_routes_login_resume_and_logout_without_cross_tenant_authority() {
    let _password_test = password_test().await;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let mut second_login = config(false);
    // Independent salts, with identical usernames, still produce independent cookies.
    second_login.password_hash = hash_password(&credentials().0).unwrap();
    let app = harness_server::tenancy::router_with_tenants(
        &root,
        vec![
            harness_server::tenancy::Tenant {
                id: "one".into(),
                principals: auth::legacy(vec![AGENT.into()]),
                admin_token: ADMIN.into(),
                admin_login: Some(config(false)),
                secure_cookie: false,
            },
            harness_server::tenancy::Tenant {
                id: "two".into(),
                principals: auth::legacy(vec![format!("{AGENT}-two")]),
                admin_token: format!("{ADMIN}-two"),
                admin_login: Some(second_login),
                secure_cookie: false,
            },
        ],
    )
    .unwrap();
    let mut cookies = vec![];
    for tenant in ["one", "two"] {
        let headers = [ORIGIN[0], ORIGIN[1], ("X-Harness-Tenant", tenant)];
        let (status, headers, body) = call(
            &app,
            "POST",
            "/admin/api/login",
            json!({"username":"administrator","password":credentials().0,"remember":true}),
            &headers,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        cookies.push(cookie(&headers));
    }
    assert_ne!(cookies[0].split('=').next(), cookies[1].split('=').next());
    assert_eq!(
        call(
            &app,
            "GET",
            "/admin/api/session",
            Value::Null,
            &[("X-Harness-Tenant", "two"), ("Cookie", &cookies[0])],
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let both = cookies.join("; ");
    for tenant in ["one", "two"] {
        assert_eq!(
            call(
                &app,
                "GET",
                "/admin/api/session",
                Value::Null,
                &[("X-Harness-Tenant", tenant), ("Cookie", &both)],
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let headers = [
        ORIGIN[0],
        ORIGIN[1],
        ("X-Harness-Tenant", "one"),
        ("Cookie", both.as_str()),
    ];
    assert_eq!(
        call(&app, "POST", "/admin/api/logout", json!({}), &headers,)
            .await
            .0,
        StatusCode::OK
    );
    for (tenant, expected) in [("one", StatusCode::UNAUTHORIZED), ("two", StatusCode::OK)] {
        assert_eq!(
            call(
                &app,
                "GET",
                "/admin/api/session",
                Value::Null,
                &[("X-Harness-Tenant", tenant), ("Cookie", &both)],
            )
            .await
            .0,
            expected
        );
    }
}
