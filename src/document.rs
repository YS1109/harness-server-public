use crate::error::{Error, Result, invalid};
use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::LazyLock};

pub type Documents = BTreeMap<String, String>;
pub type Metadata = Map<String, Value>;
static SLUG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]{0,127}$").unwrap());
static MONTH_FILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}\.md$").unwrap());
static SECRETS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\bsk-[a-z0-9_-]{20,}|\bAKIA[A-Z0-9]{16}\b|\bgh[pousr]_[a-z0-9]{20,}|\bxox[baprs]-[a-z0-9-]{20,}|\beyJ[a-z0-9_-]{10,}\.[a-z0-9_-]{10,}\.[a-z0-9_-]{10,}|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----|authorization\s*:\s*bearer\s+[a-z0-9._~+/-]{12,})").unwrap()
});

pub fn slug(s: &str) -> Result<()> {
    if SLUG.is_match(s) {
        Ok(())
    } else {
        Err(invalid("ID必须是1–128字的小写slug，不接受路径"))
    }
}
pub fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
pub fn reject_secrets(s: &str) -> Result<()> {
    if SECRETS.is_match(s) {
        Err(invalid("内容包含疑似凭据；未保存"))
    } else {
        Ok(())
    }
}
pub fn path_kind(path: &str) -> Result<&'static str> {
    let (ns, path) = crate::namespace::split(path)?;
    if ns != "shared" && path.starts_with("control/") {
        return Err(invalid("管理目录不得嵌套namespace"));
    }
    let parts: Vec<_> = path.split('/').collect();
    let kind = match parts.as_slice() {
        ["control", "namespaces", _] => "control_namespace",
        ["control", "agents", _] => "control_agent",
        ["control", "audit", file] if MONTH_FILE.is_match(file) => "control_audit",
        ["memory", "project", _] => "project",
        ["memory", "project", _, _] => "project_topic",
        ["memory", "process", _] => "process",
        ["memory", "templates", _] => "template",
        ["memory", "logs", file] if MONTH_FILE.is_match(file) => "log",
        ["handoffs", "sessions", "active" | "completed" | "stale", _] => "session",
        ["handoffs", "workstreams", _] | ["handoffs", "workstreams", "archive", _] => "workstream",
        ["executions", "tasks", _] => "execution_task",
        ["executions", "attempts", _] => "execution_attempt",
        ["executions", "contexts", _] => "execution_context",
        _ => return Err(invalid("仅接受受管记忆、模板、交接和月度日志相对路径")),
    };
    if kind != "log" && kind != "control_audit" {
        slug(
            parts
                .last()
                .unwrap()
                .strip_suffix(".md")
                .ok_or_else(|| invalid("仅接受Markdown"))?,
        )?;
        if path.ends_with("/README.md") {
            return Err(invalid("派生索引不可写入"));
        }
        if kind == "project_topic" {
            slug(parts[2])?;
        }
    }
    Ok(kind)
}
pub fn parse(text: &str) -> Result<(Metadata, String)> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| invalid("缺少YAML front matter"))?;
    let (meta, body) = rest
        .split_once("\n---\n")
        .ok_or_else(|| invalid("front matter未闭合"))?;
    let meta: Metadata =
        serde_yaml::from_str(meta).map_err(|_| invalid("front matter必须是有效对象"))?;
    Ok((meta, body.trim_start_matches('\n').to_string()))
}
pub fn render(meta: &Metadata, body: &str) -> Result<String> {
    let yaml = serde_yaml::to_string(meta).map_err(|_| invalid("元数据无法序列化"))?;
    Ok(format!("---\n{yaml}---\n\n{}\n", body.trim_end()))
}
pub fn string<'a>(m: &'a Metadata, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn strings(m: &Metadata, key: &str) -> Vec<String> {
    m.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
pub fn validate(path: &str, content: &str) -> Result<()> {
    let kind = path_kind(path)?;
    let (_, path) = crate::namespace::split(path)?;
    reject_secrets(content)?;
    if content.len()
        > if kind == "log" || kind == "control_audit" {
            2 * 1024 * 1024
        } else {
            256 * 1024
        }
    {
        return Err(invalid("单文档超过容量限制（日志2MiB，其余256KiB）"));
    }
    if kind.starts_with("control_") {
        return crate::registry::validate_document(path, content);
    }
    if kind == "log" {
        return Ok(());
    }
    // Templates are raw Markdown resources, not YAML memory cards. Keep their
    // full skeleton intact under the existing 256KiB resource cap, without
    // inheriting the 2,000-character process-card budget.
    if kind == "template" {
        if content.trim().is_empty() || content.contains('\0') {
            return Err(invalid("模板必须是非空Markdown文本，不接受NUL"));
        }
        return Ok(());
    }
    if kind.starts_with("execution_") {
        return crate::execution::validate_document(path, content);
    }
    let (m, body) = parse(content)?;
    let budget = match kind {
        "session" => 600,
        "workstream" => 1200,
        "project_topic" => 6000,
        _ => 2000,
    };
    if body.chars().count() > budget {
        return Err(Error::Budget {
            kind: kind.to_owned(),
            path: path.to_owned(),
            actual: body.chars().count(),
            limit: budget,
        });
    }
    if m.get("schema_version").and_then(Value::as_u64) != Some(1) || string(&m, "title").is_empty()
    {
        return Err(invalid("需要schema_version=1和title"));
    }
    let id_key = match kind {
        "session" => "session_id",
        "workstream" => "workstream_id",
        "process" => "process_id",
        "project_topic" => "topic_id",
        _ => "project_id",
    };
    let id = string(&m, id_key);
    slug(id)?;
    if path.rsplit('/').next() != Some(&format!("{id}.md")) {
        return Err(invalid("文件名与身份不一致"));
    }
    if matches!(kind, "project" | "project_topic" | "process") {
        if string(&m, "memory_type") != kind
            || !matches!(string(&m, "status"), "ACTIVE" | "ARCHIVED")
        {
            return Err(invalid("记忆类型或状态无效"));
        }
        let verified = string(&m, "verified_at");
        if verified != "unknown" {
            let date = chrono::NaiveDate::parse_from_str(verified, "%Y-%m-%d")
                .map_err(|_| invalid("核验日期无效"))?;
            if date > chrono::Utc::now().date_naive() {
                return Err(invalid("核验日期不能在未来"));
            }
        }
    }
    if kind == "project_topic" {
        slug(string(&m, "project_id"))?;
        if path.split('/').nth(2) != Some(string(&m, "project_id"))
            || string(&m, "summary").chars().count() > 160
            || string(&m, "summary").is_empty()
            || !(1..=8).contains(&strings(&m, "triggers").len())
        {
            return Err(invalid("专题归属、摘要或触发词无效"));
        }
    }
    if kind == "session" {
        slug(string(&m, "agent"))?;
        slug(string(&m, "native_session_id"))?;
        slug(string(&m, "workstream_id"))?;
        slug(string(&m, "project"))?;
        if id
            != format!(
                "{}-{}",
                string(&m, "agent"),
                string(&m, "native_session_id")
            )
        {
            return Err(invalid("Session原生身份映射无效"));
        }
        let status = string(&m, "status");
        let bucket = path.split('/').nth(2).unwrap();
        if !matches!(
            (bucket, status),
            ("active", "ACTIVE" | "IDLE" | "WAITING_USER")
                | ("completed", "COMPLETED")
                | ("stale", "STALE")
        ) {
            return Err(invalid("Session目录和状态不一致"));
        }
    }
    if kind == "workstream" {
        slug(string(&m, "project"))?;
        if !matches!(
            string(&m, "status"),
            "ACTIVE" | "PAUSED" | "COMPLETED" | "ARCHIVED"
        ) {
            return Err(invalid("Workstream状态无效"));
        }
    }
    Ok(())
}
pub fn canonical(docs: &Documents, project: &str) -> Result<String> {
    slug(project)?;
    let mut matches = Vec::new();
    for (p, c) in docs {
        if path_kind(p)? == "project" {
            let (m, _) = parse(c)?;
            if string(&m, "project_id") == project
                || strings(&m, "aliases").iter().any(|s| s == project)
            {
                matches.push(string(&m, "project_id").to_owned());
            }
        }
    }
    if matches.len() > 1 {
        return Err(invalid("项目ID或别名存在歧义"));
    }
    Ok(matches.pop().unwrap_or_else(|| project.to_owned()))
}
pub fn validate_all(docs: &Documents) -> Result<()> {
    if docs.len() > 4096 || docs.values().map(String::len).sum::<usize>() > 32 * 1024 * 1024 {
        return Err(invalid("存储超过首版容量限制"));
    }
    for (p, c) in docs {
        validate(p, c)?;
    }
    crate::registry::validate_store(docs)?;
    let groups = crate::namespace::groups(docs)?;
    for namespace in groups.keys() {
        validate_namespace(&crate::namespace::view(docs, namespace, true))?;
    }
    Ok(())
}
fn validate_namespace(docs: &Documents) -> Result<()> {
    crate::execution::validate_links(docs)?;
    let mut identities = std::collections::BTreeSet::new();
    // Index only this validated namespace view. Rebuilding for every validation
    // avoids stale aliases and keeps cross-namespace inheritance unchanged.
    let mut parsed = BTreeMap::new();
    let mut projects = BTreeMap::new();
    let mut workstreams = BTreeMap::new();
    for (p, c) in docs {
        let kind = path_kind(p)?;
        if matches!(kind, "log" | "template") {
            continue;
        }
        let (m, _) = parse(c)?;
        if kind == "project" {
            let id = string(&m, "project_id");
            // canonical() counts a project once even if an alias repeats its ID.
            let mut names = strings(&m, "aliases");
            names.push(id.to_owned());
            names.sort();
            names.dedup();
            for name in names {
                slug(&name)?;
                if projects.insert(name, id.to_owned()).is_some() {
                    return Err(invalid("项目ID或别名存在歧义"));
                }
            }
        }
        if kind == "workstream"
            && workstreams
                .insert(string(&m, "workstream_id").to_owned(), p)
                .is_some()
        {
            return Err(invalid("重复Workstream身份"));
        }
        parsed.insert(p, (kind, m));
    }
    let resolve = |project: &str| -> Result<String> {
        slug(project)?;
        Ok(projects
            .get(project)
            .cloned()
            .unwrap_or_else(|| project.to_owned()))
    };
    for (kind, m) in parsed.values() {
        let kind = *kind;
        if kind == "session" {
            if !identities.insert(format!("session:{}", string(m, "session_id"))) {
                return Err(invalid("重复Session身份"));
            }
            let ws = workstreams
                .get(string(m, "workstream_id"))
                .ok_or_else(|| invalid("Session关联Workstream不存在"))?;
            let wm = &parsed[ws].1;
            if resolve(string(m, "project"))? != resolve(string(wm, "project"))? {
                return Err(invalid("Session与Workstream跨项目绑定"));
            }
        }
        if kind == "project_topic"
            && !docs.contains_key(&format!("memory/project/{}.md", string(m, "project_id")))
        {
            return Err(invalid("专题缺少项目入口"));
        }
    }
    Ok(())
}
pub fn find(docs: &Documents, key: &str, id: &str) -> Result<Option<String>> {
    let mut result = None;
    for (p, c) in docs {
        let kind = path_kind(p)?;
        if matches!(kind, "log" | "template")
            || (key == "workstream_id" && kind != "workstream")
            || (key == "session_id" && kind != "session")
        {
            continue;
        }
        let (m, _) = parse(c)?;
        if string(&m, key) == id {
            if result.is_some() {
                return Err(invalid("身份存在多个候选"));
            }
            result = Some(p.clone());
        }
    }
    Ok(result)
}
