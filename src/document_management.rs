//! Explicit administrator edits. Preview runs the same engine on a disposable state.
use crate::{
    document::{Documents, parse, path_kind, render, sha, validate_all},
    engine,
    error::{Error, Result, conflict, invalid},
    namespace, registry,
    store::State,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Edit,
    Archive,
    Restore,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub namespace: String,
    pub path: String,
    pub expected_sha256: String,
    pub action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

fn editable(path: &str) -> Result<&'static str> {
    if path.starts_with("namespaces/") || path.starts_with("control/") {
        return Err(invalid("管理文档要求空间内逻辑路径"));
    }
    let kind = path_kind(path)?;
    if !matches!(kind, "project" | "project_topic" | "process" | "template") {
        return Err(invalid(
            "仅编辑项目、专题、流程和模板；交接、日志及执行记录使用专用流程",
        ));
    }
    Ok(kind)
}

pub fn budget(path: &str, content: &str) -> Result<Value> {
    let kind = path_kind(path)?;
    let (characters, limit, unit) = if kind == "template" {
        (content.len(), 256 * 1024, "UTF-8 bytes")
    } else {
        let (_, body) = parse(content)?;
        (
            body.chars().count(),
            if kind == "project_topic" { 6000 } else { 2000 },
            "Unicode characters",
        )
    };
    let index_characters = if kind == "project" {
        let (_, body) = parse(content)?;
        body.split_once("<!-- PROJECT-TOPICS-START -->")
            .and_then(|(_, tail)| tail.split_once("<!-- PROJECT-TOPICS-END -->"))
            .map(|(index, _)| index.chars().count())
    } else {
        None
    };
    Ok(json!({"used":characters,"limit":limit,"unit":unit,"index_characters":index_characters}))
}

/// Apply to the caller's transaction copy. All edits remain in the exact source space.
pub fn apply(state: &mut State, change: &Change) -> Result<Value> {
    let space = registry::space(&state.documents, &change.namespace)?;
    if space.status != "ACTIVE" {
        return Err(Error::Forbidden("空间已归档，恢复空间后再编辑文档".into()));
    }
    let kind = editable(&change.path)?;
    let physical = namespace::physical(&change.namespace, &change.path);
    let old = state.documents.get(&physical).ok_or_else(|| {
        Error::NotFound("原空间不存在该文档；不编辑继承版本或自动创建覆盖".into())
    })?;
    if change.expected_sha256.is_empty() || sha(old) != change.expected_sha256 {
        return Err(conflict("文档版本已变化；保留草稿，读取最新版本后合并"));
    }
    let content = match change.action {
        Action::Edit => change
            .content
            .clone()
            .ok_or_else(|| invalid("edit需要完整content"))?,
        Action::Archive | Action::Restore => {
            if change.content.is_some() || kind == "template" {
                return Err(invalid("状态操作不接受content；原文模板无状态，不能归档"));
            }
            let (mut metadata, body) = parse(old)?;
            metadata.insert(
                "status".into(),
                json!(if matches!(change.action, Action::Archive) {
                    "ARCHIVED"
                } else {
                    "ACTIVE"
                }),
            );
            render(&metadata, &body)?
        }
    };
    // Only update edit time. Never manufacture verification or change managed identity.
    let content = if kind != "template" {
        let (mut metadata, body) = parse(&content)?;
        metadata.insert("updated_at".into(), json!(chrono::Utc::now().to_rfc3339()));
        render(&metadata, &body)?
    } else {
        content
    };
    let before = state.documents.clone();
    namespace::transact(state, &change.namespace, false, None, |local| {
        engine::put(
            local,
            &json!({
                "path":change.path,"content":content,"expected_sha256":change.expected_sha256
            }),
        )
    })?;
    let mut changes = Vec::new();
    for (path, content) in &state.documents {
        if before.get(path) == Some(content) || path.starts_with("control/") {
            continue;
        }
        let (ns, logical) = namespace::split(path)?;
        let previous = before.get(path).cloned().unwrap_or_default();
        changes.push(json!({
            "namespace":ns,"path":logical,
            "before_sha256":before.get(path).map(|v|sha(v)),"sha256":sha(content),
            "budget":budget(logical,content)?,
            "before":previous,"after":content
        }));
    }
    registry::audit(
        &mut state.documents,
        "document-update",
        &format!("{}:{}", change.namespace, change.path),
    );
    Ok(json!({
        "namespace":change.namespace,"path":change.path,
        "sha256":sha(&state.documents[&physical]),
        "content":state.documents[&physical],
        "changes":changes,"written":true
    }))
}

pub fn preview(documents: &Documents, change: &Change) -> Result<Value> {
    let mut state = State {
        documents: documents.clone(),
        receipts: Default::default(),
    };
    let mut result = apply(&mut state, change)?;
    validate_all(&state.documents)?;
    result["written"] = json!(false);
    result["validated"] = json!(true);
    result["note"] = json!("预览未保存；保存时重新检查最新SHA、索引、预算及所有关联");
    Ok(result)
}
