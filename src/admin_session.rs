//! In-memory administrator sessions. Credentials and cookie values never enter the store.
use crate::{
    document::sha,
    error::{Result, invalid},
};
use argon2::{
    Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version,
    password_hash::SaltString,
};
use axum::http::{HeaderMap, header};
use rand_core::OsRng;
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const REMEMBER_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const MAX_SESSIONS: usize = 32;
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const MAX_ATTEMPTS: usize = 10;
static VERIFICATIONS: AtomicUsize = AtomicUsize::new(0);

/// Process-wide admission, acquired before scheduling any expensive password work.
pub(crate) struct VerificationPermit;

pub(crate) fn try_verification() -> Option<VerificationPermit> {
    VERIFICATIONS
        .fetch_update(Ordering::Acquire, Ordering::Relaxed, |count| {
            (count < 2).then_some(count + 1)
        })
        .ok()
        .map(|_| VerificationPermit)
}

impl Drop for VerificationPermit {
    fn drop(&mut self) {
        VERIFICATIONS.fetch_sub(1, Ordering::Release);
    }
}

/// Configuration is intentionally not Debug/Serialize: the PHC hash is a credential.
#[derive(Clone)]
pub struct LoginConfig {
    pub username: String,
    pub password_hash: String,
    /// Explicit transport policy, including HTTPS terminated by a trusted reverse proxy.
    pub secure_cookie: bool,
}

impl LoginConfig {
    pub fn new(username: impl Into<String>, password_hash: impl Into<String>) -> Result<Self> {
        let config = Self {
            username: username.into(),
            password_hash: password_hash.into(),
            secure_cookie: true,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn from_environment() -> Result<Option<Self>> {
        Self::from_values(
            environment_value("HARNESS_ADMIN_USERNAME")?,
            environment_value("HARNESS_ADMIN_PASSWORD_HASH")?,
            environment_value("HARNESS_ADMIN_SECURE_COOKIE")?.as_deref(),
        )
    }

    fn from_values(
        username: Option<String>,
        hash: Option<String>,
        secure_cookie: Option<&str>,
    ) -> Result<Option<Self>> {
        let secure_cookie = parse_secure_cookie(secure_cookie)?;
        match (
            username.filter(|value| !value.is_empty()),
            hash.filter(|value| !value.is_empty()),
        ) {
            (None, None) => Ok(None),
            (Some(username), Some(hash)) => {
                let mut config = Self::new(username, hash)?;
                config.secure_cookie = secure_cookie;
                Ok(Some(config))
            }
            _ => Err(invalid(
                "HARNESS_ADMIN_USERNAME与HARNESS_ADMIN_PASSWORD_HASH必须同时配置",
            )),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.username.is_empty()
            || self.username.len() > 128
            || self.username.trim() != self.username
            || self.username.chars().any(char::is_control)
            || self.password_hash.len() > 512
        {
            return Err(invalid("管理员用户名或密码哈希配置无效"));
        }
        let hash = PasswordHash::new(&self.password_hash)
            .map_err(|_| invalid("管理员密码必须为Argon2id PHC哈希"))?;
        let params = Params::try_from(&hash).map_err(|_| invalid("管理员密码Argon2参数无效"))?;
        if hash.algorithm.as_str() != "argon2id"
            || hash.version != Some(19)
            || !(19 * 1024..=64 * 1024).contains(&params.m_cost())
            || !(2..=4).contains(&params.t_cost())
            || params.p_cost() != 1
            || hash.hash.is_none_or(|output| output.len() != 32)
            || !hash
                .salt
                .is_some_and(|salt| (22..=64).contains(&salt.as_str().len()))
        {
            return Err(invalid(
                "Argon2id要求v19、19–64MiB、2–4轮、p=1、32字节输出及至少16字节随机盐",
            ));
        }
        Ok(())
    }
}

fn secure_cookie_from_environment() -> Result<bool> {
    parse_secure_cookie(environment_value("HARNESS_ADMIN_SECURE_COOKIE")?.as_deref())
}

fn environment_value(name: &str) -> Result<Option<String>> {
    std::env::var(name).map(Some).or_else(|error| match error {
        std::env::VarError::NotPresent => Ok(None),
        _ => Err(invalid("管理员登录环境变量必须是有效文本")),
    })
}

fn parse_secure_cookie(value: Option<&str>) -> Result<bool> {
    match value {
        None | Some("true") => Ok(true),
        Some("false") => Ok(false),
        _ => Err(invalid("HARNESS_ADMIN_SECURE_COOKIE必须为true或false")),
    }
}

/// Offline helper. The CLI owns hidden input and must never log the password.
pub fn hash_password(password: &str) -> Result<String> {
    if !(12..=1024).contains(&password.len()) {
        return Err(invalid("管理员密码必须为12–1024 UTF-8字节"));
    }
    let salt = SaltString::generate(&mut OsRng);
    let params = Params::new(19 * 1024, 2, 1, Some(32)).map_err(|_| invalid("密码哈希参数无效"))?;
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| invalid("无法生成管理员密码哈希"))
}

#[derive(Clone, Serialize)]
pub(crate) struct SessionInfo {
    pub authenticated: bool,
    pub expires_at: String,
    pub remember: bool,
}

struct Session {
    issued: Instant,
    expires: Instant,
    info: SessionInfo,
}

#[derive(Default)]
struct State {
    sessions: BTreeMap<String, Session>,
    attempts: VecDeque<Instant>,
}

/// One instance per service/tenant. Restarting the service invalidates all sessions.
pub struct AdminSessions {
    config: Option<LoginConfig>,
    secure_cookie: bool,
    cookie_name: String,
    state: Mutex<State>,
}

impl Default for AdminSessions {
    fn default() -> Self {
        Self::disabled()
    }
}

impl AdminSessions {
    pub fn disabled() -> Self {
        Self {
            config: None,
            secure_cookie: true,
            // Distinct routers never overwrite or consume each other's cookies.
            cookie_name: format!("harness_admin_{}", uuid::Uuid::new_v4().simple()),
            state: Mutex::new(State::default()),
        }
    }

    pub fn new(config: Option<LoginConfig>) -> Result<Self> {
        let secure_cookie = match &config {
            Some(config) => config.secure_cookie,
            None => secure_cookie_from_environment()?,
        };
        Self::with_policy(config, secure_cookie)
    }

    /// Explicit tenant policy; never reads process-wide administrator settings.
    pub fn with_policy(config: Option<LoginConfig>, secure_cookie: bool) -> Result<Self> {
        if let Some(config) = &config {
            config.validate()?;
        }
        Ok(Self {
            config,
            secure_cookie,
            ..Self::disabled()
        })
    }

    pub(crate) fn password_enabled(&self) -> bool {
        self.config.is_some()
    }

    pub(crate) fn secure_cookie(&self) -> bool {
        self.secure_cookie
    }

    pub(crate) fn allow_login(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let now = Instant::now();
        while state
            .attempts
            .front()
            .is_some_and(|attempt| now.duration_since(*attempt) >= LOGIN_WINDOW)
        {
            state.attempts.pop_front();
        }
        if state.attempts.len() >= MAX_ATTEMPTS {
            return false;
        }
        state.attempts.push_back(now);
        true
    }

    pub(crate) fn verify_password(&self, username: &str, password: &str) -> bool {
        use subtle::ConstantTimeEq;
        let Some(config) = &self.config else {
            return false;
        };
        let Ok(hash) = PasswordHash::new(&config.password_hash) else {
            return false;
        };
        // Always perform the configured password verification, including unknown users.
        let password_ok = Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok();
        let username_ok = sha(username)
            .as_bytes()
            .ct_eq(sha(&config.username).as_bytes())
            .unwrap_u8()
            == 1;
        password_ok & username_ok
    }

    fn cookie_token<'a>(&self, headers: &'a HeaderMap) -> Option<&'a str> {
        let mut found = None;
        for header in headers.get_all(header::COOKIE) {
            for cookie in header.to_str().ok()?.split(';') {
                let Some((name, value)) = cookie.trim().split_once('=') else {
                    continue;
                };
                if name == self.cookie_name {
                    if found.is_some()
                        || value.len() != 64
                        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
                    {
                        return None;
                    }
                    found = Some(value);
                }
            }
        }
        found
    }

    pub(crate) fn session(&self, headers: &HeaderMap) -> Option<SessionInfo> {
        self.session_at(headers, Instant::now())
    }

    fn session_at(&self, headers: &HeaderMap, now: Instant) -> Option<SessionInfo> {
        let token = self.cookie_token(headers)?;
        let mut state = self.state.lock().ok()?;
        state.sessions.retain(|_, session| session.expires > now);
        state
            .sessions
            .get(&sha(token))
            .map(|session| session.info.clone())
    }

    pub(crate) fn issue(
        &self,
        headers: &HeaderMap,
        remember: bool,
    ) -> Result<(String, SessionInfo)> {
        let now = Instant::now();
        let ttl = if remember { REMEMBER_TTL } else { SESSION_TTL };
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let info = SessionInfo {
            authenticated: true,
            expires_at: (chrono::Utc::now() + chrono::Duration::seconds(ttl.as_secs() as i64))
                .to_rfc3339(),
            remember,
        };
        let mut state = self.state.lock().map_err(|_| invalid("会话锁不可用"))?;
        state.sessions.retain(|_, session| session.expires > now);
        if let Some(previous) = self.cookie_token(headers) {
            state.sessions.remove(&sha(previous));
        }
        if state.sessions.len() >= MAX_SESSIONS
            && let Some(oldest) = state
                .sessions
                .iter()
                .min_by_key(|(_, session)| session.issued)
                .map(|(key, _)| key.clone())
        {
            state.sessions.remove(&oldest);
        }
        state.sessions.insert(
            sha(&token),
            Session {
                issued: now,
                expires: now + ttl,
                info: info.clone(),
            },
        );
        let mut cookie = format!(
            "{}={token}; Path=/admin; HttpOnly; SameSite=Strict{}",
            self.cookie_name,
            if self.secure_cookie() { "; Secure" } else { "" }
        );
        if remember {
            cookie.push_str(&format!("; Max-Age={}", ttl.as_secs()));
        }
        Ok((cookie, info))
    }

    pub(crate) fn revoke(&self, headers: &HeaderMap) -> Result<String> {
        let mut state = self.state.lock().map_err(|_| invalid("会话锁不可用"))?;
        if let Some(token) = self.cookie_token(headers) {
            state.sessions.remove(&sha(token));
        }
        Ok(format!(
            "{}=; Path=/admin; HttpOnly; SameSite=Strict; Max-Age=0{}",
            self.cookie_name,
            if self.secure_cookie() { "; Secure" } else { "" }
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookies(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            value.split(';').next().unwrap().parse().unwrap(),
        );
        headers
    }

    #[test]
    fn expiry_is_fixed_and_remembered_sessions_expire_too() {
        let sessions = AdminSessions::disabled();
        for (remember, ttl) in [(false, SESSION_TTL), (true, REMEMBER_TTL)] {
            let before = Instant::now();
            let (cookie, _) = sessions.issue(&HeaderMap::new(), remember).unwrap();
            let headers = cookies(&cookie);
            assert!(sessions.session_at(&headers, before + ttl / 2).is_some());
            assert!(
                sessions
                    .session_at(&headers, before + ttl + Duration::from_secs(1))
                    .is_none()
            );
        }
    }

    #[test]
    fn session_storage_and_attempts_are_bounded_and_restart_drops_sessions() {
        let sessions = AdminSessions::disabled();
        let first = sessions.issue(&HeaderMap::new(), false).unwrap().0;
        for _ in 0..MAX_SESSIONS {
            sessions.issue(&HeaderMap::new(), false).unwrap();
        }
        assert!(sessions.session(&cookies(&first)).is_none());
        assert_eq!(sessions.state.lock().unwrap().sessions.len(), MAX_SESSIONS);
        let next = sessions.issue(&HeaderMap::new(), false).unwrap().0;
        assert!(AdminSessions::disabled().session(&cookies(&next)).is_none());
        for _ in 0..MAX_ATTEMPTS {
            assert!(sessions.allow_login());
        }
        assert!(!sessions.allow_login());
        assert_eq!(sessions.state.lock().unwrap().attempts.len(), MAX_ATTEMPTS);
        for attempt in &mut sessions.state.lock().unwrap().attempts {
            *attempt -= LOGIN_WINDOW;
        }
        assert!(sessions.allow_login());
    }

    #[test]
    fn password_work_has_two_process_wide_permits_and_releases_on_drop() {
        let first = try_verification().unwrap();
        let second = try_verification().unwrap();
        assert!(try_verification().is_none());
        drop(first);
        let replacement = try_verification().unwrap();
        assert!(try_verification().is_none());
        drop(second);
        drop(replacement);
        assert_eq!(VERIFICATIONS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn environment_contract_accepts_empty_optional_credentials_and_strict_booleans() {
        assert!(
            LoginConfig::from_values(None, None, None)
                .unwrap()
                .is_none()
        );
        assert!(
            LoginConfig::from_values(Some(String::new()), Some(String::new()), Some("false"))
                .unwrap()
                .is_none()
        );
        for boolean in ["", "0", "1", "TRUE", " false "] {
            assert!(LoginConfig::from_values(None, None, Some(boolean)).is_err());
        }
        assert!(
            LoginConfig::from_values(Some("administrator".into()), Some(String::new()), None)
                .is_err()
        );
        assert!(
            LoginConfig::from_values(Some(String::new()), Some("present".into()), None).is_err()
        );
    }
}
