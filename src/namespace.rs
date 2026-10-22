//! Logical views over a single atomic store. Legacy paths belong to `shared`.
use crate::{
    document::{Documents, path_kind, sha, slug},
    error::{Result, invalid},
    store::State,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn split(path: &str) -> Result<(&str, &str)> {
    if let Some(rest) = path.strip_prefix("namespaces/") {
        let (namespace, logical) = rest
            .split_once('/')
            .ok_or_else(|| invalid("namespace路径无效"))?;
        slug(namespace)?;
        if namespace == "shared" || logical.starts_with("namespaces/") {
            return Err(invalid("shared使用原路径；不允许嵌套namespace"));
        }
        Ok((namespace, logical))
    } else {
        Ok(("shared", path))
    }
}

pub fn physical(namespace: &str, path: &str) -> String {
    if namespace == "shared" {
        path.to_owned()
    } else {
        format!("namespaces/{namespace}/{path}")
    }
}

pub fn shared_memory(path: &str) -> bool {
    path.starts_with("memory/project/") || path.starts_with("memory/process/")
}

pub fn view(docs: &Documents, namespace: &str, inherit_shared: bool) -> Documents {
    let mut result = Documents::new();
    if namespace != "shared" && inherit_shared {
        result.extend(
            docs.iter()
                .filter(|(p, _)| shared_memory(p) || p.starts_with("memory/templates/"))
                .map(|(p, c)| (p.clone(), c.clone())),
        );
    }
    for (path, content) in docs {
        if path.starts_with("control/") {
            continue;
        }
        if let Ok((ns, logical)) = split(path)
            && ns == namespace
        {
            result.insert(logical.into(), content.clone());
        }
    }
    result
}

pub fn groups(docs: &Documents) -> Result<BTreeMap<String, Documents>> {
    let mut groups = BTreeMap::<String, Documents>::new();
    for (path, content) in docs {
        if path.starts_with("control/") {
            continue;
        }
        let (ns, logical) = split(path)?;
        groups
            .entry(ns.into())
            .or_default()
            .insert(logical.into(), content.clone());
    }
    Ok(groups)
}

pub fn transact(
    state: &mut State,
    namespace: &str,
    inherit_shared: bool,
    agent_id: Option<&str>,
    operation: impl FnOnce(&mut State) -> Result<Value>,
) -> Result<Value> {
    let before = view(&state.documents, namespace, inherit_shared);
    let mut local = State {
        documents: before.clone(),
        receipts: BTreeMap::new(),
    };
    let mut response = operation(&mut local)?;
    if response["mode"] == "SKIP" {
        return Ok(response);
    }
    // Only changes in this view enter the same original transaction. Inherited memory
    // is not duplicated by an ordinary start/finish; explicit changes create overrides.
    for path in before.keys() {
        if !local.documents.contains_key(path) {
            state.documents.remove(&physical(namespace, path));
        }
    }
    for (path, content) in &local.documents {
        if before.get(path) != Some(content) {
            path_kind(path)?;
            state
                .documents
                .insert(physical(namespace, path), content.clone());
        }
    }
    annotate(&mut response, &state.documents, namespace);
    if let Some(id) = agent_id {
        response["identity"] = json!({"agent_id":id,"namespace":namespace});
    }
    Ok(response)
}

pub fn annotate(value: &mut Value, docs: &Documents, namespace: &str) {
    match value {
        Value::Array(items) => {
            for item in items {
                annotate(item, docs, namespace);
            }
        }
        Value::Object(object) => {
            if let Some(path) = object
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_owned)
            {
                let origin = if let Some(origin) = object.get("namespace").and_then(Value::as_str) {
                    origin
                } else if docs.contains_key(&physical(namespace, &path)) {
                    namespace
                } else {
                    "shared"
                };
                object.insert("namespace".into(), json!(origin));
            }
            for child in object.values_mut() {
                annotate(child, docs, namespace);
            }
        }
        _ => {}
    }
}

pub fn receipt_key(namespace: &str, agent_id: Option<&str>, key: &str) -> Result<String> {
    // Validate before hashing so malformed original keys never become valid accidentally.
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
    {
        return Err(invalid("写请求需要有效Idempotency-Key（1–128字）"));
    }
    Ok(if namespace == "shared" && agent_id.is_none() {
        key.into()
    } else {
        format!("ns-{}", sha(&json!([namespace, agent_id, key]).to_string()))
    })
}
