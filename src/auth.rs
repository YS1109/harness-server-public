use crate::{
    document::{sha, slug},
    error::{Error, Result, invalid},
};
use serde::Deserialize;
use std::{collections::BTreeSet, path::Path};
use subtle::ConstantTimeEq;

#[derive(Clone)]
pub struct Principal {
    pub agent_id: Option<String>,
    pub token: String,
    pub read_namespaces: Vec<String>,
    pub write_namespaces: Vec<String>,
}

#[derive(Clone)]
pub struct Access {
    pub protocol: String,
    pub agent_id: Option<String>,
    pub namespace: String,
    pub inherit_shared: bool,
    pub legacy_shared: bool,
    pub read_namespaces: Vec<String>,
    pub namespace_explicit: bool,
    pub agent_header: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    principals: Vec<Grant>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    agent_id: String,
    token_env: String,
    #[serde(default)]
    read_namespaces: Vec<String>,
    #[serde(default)]
    write_namespaces: Vec<String>,
}

pub fn load(path: &Path) -> Result<Vec<Principal>> {
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|_| invalid("身份配置无效；配置仅保存token_env，不保存凭据"))?;
    config
        .principals
        .into_iter()
        .map(|g| {
            if g.token_env.is_empty()
                || !g
                    .token_env
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_')
            {
                return Err(invalid("token_env必须是环境变量名"));
            }
            Ok(Principal {
                agent_id: Some(g.agent_id),
                token: std::env::var(g.token_env)
                    .map_err(|_| invalid("缺少身份配置指定的凭据环境变量"))?,
                read_namespaces: g.read_namespaces,
                write_namespaces: g.write_namespaces,
            })
        })
        .collect()
}

pub fn legacy(tokens: Vec<String>) -> Vec<Principal> {
    tokens
        .into_iter()
        .map(|token| Principal {
            agent_id: None,
            token,
            read_namespaces: vec!["shared".into()],
            write_namespaces: vec!["shared".into()],
        })
        .collect()
}

pub fn validate(principals: &mut [Principal]) -> Result<()> {
    if principals.is_empty() {
        return Err(invalid("没有身份凭据"));
    }
    let mut tokens = BTreeSet::new();
    let mut agents = BTreeSet::new();
    for p in principals {
        if p.token.len() < 32
            || p.token.len() > 512
            || p.token.chars().any(char::is_whitespace)
            || !tokens.insert(sha(&p.token))
        {
            return Err(invalid("凭据必须唯一、32–512字且无空白"));
        }
        if let Some(id) = &p.agent_id {
            slug(id)?;
            if !agents.insert(id.clone()) {
                return Err(invalid("重复agent_id"));
            }
        }

        for ns in p.read_namespaces.iter().chain(&p.write_namespaces) {
            slug(ns)?;
        }
        if p.write_namespaces
            .iter()
            .any(|ns| !p.read_namespaces.contains(ns))
        {
            return Err(invalid("写namespace必须同时授权读取"));
        }
        p.token = sha(&p.token);
    }
    Ok(())
}

pub fn authorize(
    principals: &[Principal],
    bearer: &str,
    namespace: &str,
    agent_id: Option<&str>,
    write: bool,
) -> Result<Option<Access>> {
    let hash = sha(bearer);
    let mut matched = None;
    for p in principals {
        if hash.as_bytes().ct_eq(p.token.as_bytes()).unwrap_u8() == 1 {
            matched = Some(p);
        }
    }
    let Some(p) = matched else {
        return Ok(None);
    };
    slug(namespace)?;
    if let Some(id) = agent_id {
        slug(id)?;
        if p.agent_id.as_deref() != Some(id) {
            return Err(Error::Forbidden("agent_id与凭据绑定身份不一致".into()));
        }
    }
    if !p.read_namespaces.iter().any(|s| s == namespace)
        || (write && !p.write_namespaces.iter().any(|s| s == namespace))
    {
        return Err(Error::Forbidden("凭据未获当前namespace的访问权限".into()));
    }
    Ok(Some(Access {
        protocol: "v1".into(),
        legacy_shared: namespace == "shared" && agent_id.is_none(),
        read_namespaces: vec![namespace.into()],
        namespace_explicit: true,
        agent_header: agent_id.is_some(),
        agent_id: p.agent_id.clone(),
        namespace: namespace.into(),
        inherit_shared: p.read_namespaces.iter().any(|s| s == "shared"),
    }))
}

// All credentials are hashed at router construction; compare every configured hash.
pub fn principal<'a>(
    principals: &'a [Principal],
    bearer: &str,
    agent_id: Option<&str>,
) -> Result<Option<&'a Principal>> {
    let hash = sha(bearer);
    let mut matched = None;
    for p in principals {
        if hash.as_bytes().ct_eq(p.token.as_bytes()).unwrap_u8() == 1 {
            matched = Some(p);
        }
    }
    if let Some(p) = matched
        && let Some(id) = agent_id
    {
        slug(id)?;
        if p.agent_id.as_deref() != Some(id) {
            return Err(Error::Forbidden("agent_id与凭据绑定身份不一致".into()));
        }
    }
    Ok(matched)
}
