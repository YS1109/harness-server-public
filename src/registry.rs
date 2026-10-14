#[cfg(feature = "service")]
use crate::auth::{Access, Principal};
// Server-owned namespace catalog and live grants, committed with the same atomic HEAD.
#[cfg(feature = "service")]
use crate::store::Store;
use crate::{
    document::{Documents, parse, render, sha, slug},
    error::{Error, Result, conflict, invalid},
    namespace,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(feature = "service")]
use std::collections::BTreeMap;
use std::collections::BTreeSet;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Space {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub read_namespaces: Vec<String>,
    pub write_namespaces: Vec<String>,
    pub default_read_namespaces: Vec<String>,
    pub default_write_namespace: Option<String>,
}
fn path(kind: &str, id: &str) -> String {
    format!("control/{kind}/{id}.md")
}
fn decode<T: serde::de::DeserializeOwned>(content: &str) -> Result<T> {
    serde_json::from_value(json!(parse(content)?.0)).map_err(|_| invalid("管理记录格式无效"))
}
fn markdown<T: Serialize>(record: &T) -> Result<String> {
    let v = serde_json::to_value(record).map_err(|_| invalid("管理记录无法序列化"))?;
    render(
        v.as_object().ok_or_else(|| invalid("管理记录不是对象"))?,
        "# Harness management record",
    )
}
fn label(s: &str, max: usize, required: bool) -> Result<()> {
    if s.chars().count() > max
        || (required && s.trim().is_empty())
        || s.chars().any(|c| c.is_control() && c != '\n')
    {
        return Err(invalid("名称或说明无效/超过预算"));
    }
    crate::document::reject_secrets(s)
}
fn unique(values: &[String]) -> Result<()> {
    if values.len() > 16 {
        return Err(invalid("单Agent最多接入16个namespace"));
    }
    let mut seen = BTreeSet::new();
    for v in values {
        slug(v)?;
        if !seen.insert(v) {
            return Err(invalid("namespace列表重复"));
        }
    }
    Ok(())
}
pub fn validate_document(p: &str, c: &str) -> Result<()> {
    let id = p.rsplit('/').next().unwrap().strip_suffix(".md").unwrap();
    if p.starts_with("control/namespaces/") {
        let s: Space = decode(c)?;
        slug(&s.id)?;
        label(&s.name, 80, true)?;
        label(&s.description, 400, false)?;
        if s.id != id
            || s.schema_version != 1
            || !matches!(s.status.as_str(), "ACTIVE" | "ARCHIVED")
            || (s.id == "shared" && s.status != "ACTIVE")
        {
            return Err(invalid("namespace管理身份/状态无效"));
        }
    } else if p.starts_with("control/agents/") {
        let a: Agent = decode(c)?;
        slug(&a.id)?;
        label(&a.name, 80, true)?;
        if a.schema_version != 1 || a.id != id {
            return Err(invalid("Agent管理身份无效"));
        }
        for list in [
            &a.read_namespaces,
            &a.write_namespaces,
            &a.default_read_namespaces,
        ] {
            unique(list)?;
        }
        if a.default_read_namespaces.len() > 8
            || a.write_namespaces
                .iter()
                .chain(&a.default_read_namespaces)
                .any(|ns| !a.read_namespaces.contains(ns))
            || a.default_write_namespace
                .as_ref()
                .is_some_and(|ns| !a.write_namespaces.contains(ns))
        {
            return Err(invalid("默认读取/写入必须属于授权范围，最多检索8个空间"));
        }
        if !a.read_namespaces.is_empty() && a.default_read_namespaces.is_empty() {
            return Err(invalid("有读取权限时至少选择一个默认读取空间"));
        }
    } else if p.starts_with("control/audit/") {
        for line in c
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let value: Value = serde_json::from_str(line).map_err(|_| invalid("审计记录损坏"))?;
            if value.get("time").and_then(Value::as_str).is_none()
                || value.get("action").and_then(Value::as_str).is_none()
                || value.get("target").and_then(Value::as_str).is_none()
            {
                return Err(invalid("审计记录缺少字段"));
            }
        }
    } else {
        return Err(invalid("未知管理记录"));
    }
    Ok(())
}
pub fn validate_store(docs: &Documents) -> Result<()> {
    if !docs.keys().any(|p| p.starts_with("control/")) {
        return Ok(());
    }
    if !docs.contains_key(&path("namespaces", "shared")) {
        return Err(invalid("管理目录缺少shared"));
    }
    for (p, c) in docs
        .iter()
        .filter(|(p, _)| p.starts_with("control/agents/"))
    {
        validate_document(p, c)?;
        let a: Agent = decode(c)?;
        for ns in &a.read_namespaces {
            if !docs.contains_key(&path("namespaces", ns)) {
                return Err(invalid("授权指向未创建的namespace"));
            }
        }
    }
    for p in docs.keys().filter(|p| p.starts_with("namespaces/")) {
        let (ns, _) = namespace::split(p)?;
        if !docs.contains_key(&path("namespaces", ns)) {
            return Err(invalid("文档指向未注册的namespace"));
        }
    }
    Ok(())
}
#[cfg(feature = "service")]
pub fn initialize(store: &mut Store, principals: &[Principal], managed: bool) -> Result<()> {
    let initialized = store
        .state
        .documents
        .contains_key(&path("namespaces", "shared"));
    if !initialized && !managed && principals.iter().all(|p| p.agent_id.is_none()) {
        return Ok(());
    }
    let mut updates = Documents::new();
    if !initialized {
        let mut spaces = BTreeSet::from(["shared".to_owned()]);
        for p in principals {
            spaces.extend(p.read_namespaces.iter().chain(&p.write_namespaces).cloned());
        }
        for p in store.state.documents.keys() {
            let (ns, _) = namespace::split(p)?;
            if !p.starts_with("control/") {
                spaces.insert(ns.into());
            }
        }
        for id in spaces {
            updates.insert(
                path("namespaces", &id),
                markdown(&Space {
                    schema_version: 1,
                    name: if id == "shared" {
                        "公共空间".into()
                    } else {
                        id.clone()
                    },
                    id,
                    description: "Existing server configuration".into(),
                    status: "ACTIVE".into(),
                })?,
            );
        }
    }
    for p in principals {
        if let Some(id) = &p.agent_id
            && !store.state.documents.contains_key(&path("agents", id))
        {
            let reads = if initialized {
                vec![]
            } else {
                p.read_namespaces.clone()
            };
            let writes = if initialized {
                vec![]
            } else {
                p.write_namespaces.clone()
            };
            let default_write = writes
                .iter()
                .find(|s| *s == id)
                .or_else(|| writes.first())
                .cloned();
            let defaults = reads
                .iter()
                .filter(|s| *s != "shared")
                .chain(reads.iter().filter(|s| *s == "shared"))
                .take(8)
                .cloned()
                .collect();
            updates.insert(
                path("agents", id),
                markdown(&Agent {
                    schema_version: 1,
                    id: id.clone(),
                    name: id.clone(),
                    read_namespaces: reads,
                    write_namespaces: writes,
                    default_read_namespaces: defaults,
                    default_write_namespace: default_write,
                })?,
            );
        }
    }
    if !updates.is_empty() {
        let request = json!({"operation":"registry-bootstrap","records":updates});
        store.transact(
            &format!("registry-{}", sha(&request.to_string())),
            &request,
            |state| {
                state.documents.extend(updates);
                Ok(json!({"registry_initialized":true}))
            },
        )?;
    }
    Ok(())
}
pub fn space(docs: &Documents, id: &str) -> Result<Space> {
    slug(id)?;
    if id == "shared" && !docs.contains_key(&path("namespaces", id)) {
        return Ok(Space {
            schema_version: 1,
            id: "shared".into(),
            name: "公共空间".into(),
            description: String::new(),
            status: "ACTIVE".into(),
        });
    }
    decode(
        docs.get(&path("namespaces", id))
            .ok_or_else(|| Error::NotFound("namespace不存在；请在服务端创建".into()))?,
    )
}
#[cfg(feature = "service")]
pub fn agent(docs: &Documents, p: &Principal) -> Result<Agent> {
    if let Some(id) = &p.agent_id {
        decode(
            docs.get(&path("agents", id))
                .ok_or_else(|| Error::Forbidden("Agent尚未接入管理目录".into()))?,
        )
    } else {
        Ok(Agent {
            schema_version: 1,
            id: "legacy".into(),
            name: "Legacy client".into(),
            read_namespaces: vec!["shared".into()],
            write_namespaces: vec!["shared".into()],
            default_read_namespaces: vec!["shared".into()],
            default_write_namespace: Some("shared".into()),
        })
    }
}
#[cfg(feature = "service")]
pub fn ensure(docs: &Documents, access: &Access, write: bool) -> Result<()> {
    if let Some(id) = &access.agent_id {
        let a: Agent = decode(
            docs.get(&path("agents", id))
                .ok_or_else(|| Error::Forbidden("Agent未注册".into()))?,
        )?;
        if !a.read_namespaces.contains(&access.namespace)
            || (write && !a.write_namespaces.contains(&access.namespace))
            || access
                .read_namespaces
                .iter()
                .any(|ns| !a.read_namespaces.contains(ns))
            || (access.inherit_shared && !a.read_namespaces.iter().any(|s| s == "shared"))
        {
            return Err(Error::Forbidden("namespace权限已变更".into()));
        }
    }
    if access.agent_id.is_none()
        && (access.namespace != "shared" || access.read_namespaces.iter().any(|ns| ns != "shared"))
    {
        return Err(Error::Forbidden("旧凭据仅授权shared".into()));
    }
    let s = space(docs, &access.namespace)?;
    if write && s.status == "ARCHIVED" {
        return Err(Error::Forbidden("namespace已归档，禁止写入".into()));
    }
    for ns in &access.read_namespaces {
        space(docs, ns)?;
    }
    Ok(())
}
#[cfg(feature = "service")]
pub fn discovery(docs: &Documents, p: &Principal) -> Result<Value> {
    let a = agent(docs, p)?;
    let mut spaces = vec![];
    for ns in &a.read_namespaces {
        let s = space(docs, ns)?;
        spaces.push(json!({"id":s.id,"name":s.name,"description":s.description,"status":s.status,"read":true,"write":a.write_namespaces.contains(ns)&&s.status=="ACTIVE"}));
    }
    Ok(
        json!({"agent_id":p.agent_id,"namespaces":spaces,"default_read_namespaces":a.default_read_namespaces,"default_write_namespace":a.default_write_namespace,"registry_version":1}),
    )
}
fn current_hash(docs: &Documents, p: &str, expected: &str) -> Result<()> {
    let c = docs
        .get(p)
        .ok_or_else(|| Error::NotFound("管理记录不存在".into()))?;
    if expected.is_empty() || sha(c) != expected {
        return Err(conflict("管理记录版本已变化；重新读取后合并"));
    }
    Ok(())
}
fn audit(docs: &mut Documents, action: &str, target: &str) {
    let now = chrono::Utc::now();
    let p = path("audit", &now.format("%Y-%m").to_string());
    let event =
        json!({"time":now.to_rfc3339(),"action":action,"actor":"administrator","target":target});
    docs.entry(p)
        .or_insert_with(|| "# Management audit\n".into())
        .push_str(&format!("{event}\n"));
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    name: String,
    #[serde(default)]
    description: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateSpace {
    name: String,
    description: String,
    status: String,
    expected_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateAgent {
    name: String,
    read_namespaces: Vec<String>,
    write_namespaces: Vec<String>,
    default_read_namespaces: Vec<String>,
    default_write_namespace: Option<String>,
    expected_sha256: String,
}
pub fn mutate(docs: &mut Documents, operation: &str, id: &str, v: &Value) -> Result<Value> {
    crate::document::reject_secrets(&v.to_string())?;
    match operation {
        "namespace-create" => {
            let c: Create = serde_json::from_value(v.clone())
                .map_err(|_| invalid("创建空间只接受name/description"))?;
            label(&c.name, 80, true)?;
            label(&c.description, 400, false)?;
            let s = Space {
                schema_version: 1,
                id: format!("ns-{}", uuid::Uuid::new_v4()),
                name: c.name.trim().into(),
                description: c.description,
                status: "ACTIVE".into(),
            };
            let text = markdown(&s)?;
            let hash = sha(&text);
            docs.insert(path("namespaces", &s.id), text);
            audit(docs, operation, &s.id);
            Ok(json!({"namespace":s,"sha256":hash}))
        }
        "namespace-update" => {
            let c: UpdateSpace =
                serde_json::from_value(v.clone()).map_err(|_| invalid("namespace更新字段无效"))?;
            let p = path("namespaces", id);
            current_hash(docs, &p, &c.expected_sha256)?;
            let s = Space {
                schema_version: 1,
                id: id.into(),
                name: c.name.trim().into(),
                description: c.description,
                status: c.status,
            };
            let text = markdown(&s)?;
            validate_document(&p, &text)?;
            let hash = sha(&text);
            docs.insert(p, text);
            audit(docs, operation, id);
            Ok(json!({"namespace":s,"sha256":hash}))
        }
        "agent-update" => {
            let c: UpdateAgent =
                serde_json::from_value(v.clone()).map_err(|_| invalid("Agent权限字段无效"))?;
            let p = path("agents", id);
            current_hash(docs, &p, &c.expected_sha256)?;
            for ns in &c.read_namespaces {
                space(docs, ns)?;
            }
            let a = Agent {
                schema_version: 1,
                id: id.into(),
                name: c.name.trim().into(),
                read_namespaces: c.read_namespaces,
                write_namespaces: c.write_namespaces,
                default_read_namespaces: c.default_read_namespaces,
                default_write_namespace: c.default_write_namespace,
            };
            let text = markdown(&a)?;
            validate_document(&p, &text)?;
            let hash = sha(&text);
            docs.insert(p, text);
            audit(docs, operation, id);
            Ok(json!({"agent":a,"sha256":hash}))
        }
        _ => Err(invalid("未知管理操作")),
    }
}
#[cfg(feature = "service")]
pub fn overview(docs: &Documents, principals: &[Principal]) -> Result<Value> {
    let mut agents = vec![];
    for (p, c) in docs
        .iter()
        .filter(|(p, _)| p.starts_with("control/agents/"))
    {
        let mut a =
            serde_json::to_value(decode::<Agent>(c)?).map_err(|_| invalid("Agent记录无效"))?;
        a["sha256"] = json!(sha(c));
        a["credential_configured"] = json!(
            principals
                .iter()
                .any(|p| p.agent_id.as_deref() == a["id"].as_str())
        );
        agents.push(a);
        let _ = p;
    }
    let mut spaces = vec![];
    for (p, c) in docs
        .iter()
        .filter(|(p, _)| p.starts_with("control/namespaces/"))
    {
        let s: Space = decode(c)?;
        let mut counts = BTreeMap::<String, u64>::new();
        for path in namespace::view(docs, &s.id, false).keys() {
            *counts
                .entry(crate::document::path_kind(path)?.into())
                .or_default() += 1;
        }
        let members:Vec<_>=agents.iter().filter(|a|a["read_namespaces"].as_array().is_some_and(|v|v.contains(&json!(s.id)))).map(|a|json!({"id":a["id"],"name":a["name"],"read":true,"write":a["write_namespaces"].as_array().is_some_and(|v|v.contains(&json!(s.id)))})).collect();
        let mut v = serde_json::to_value(s).map_err(|_| invalid("空间记录无效"))?;
        v["sha256"] = json!(sha(c));
        v["counts"] = json!(counts);
        v["members"] = json!(members);
        spaces.push(v);
        let _ = p;
    }
    let mut events = vec![];
    for (_, c) in docs
        .iter()
        .rev()
        .filter(|(p, _)| p.starts_with("control/audit/"))
    {
        for line in c
            .lines()
            .rev()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            if events.len() == 50 {
                break;
            }
            events.push(serde_json::from_str::<Value>(line).map_err(|_| invalid("审计记录损坏"))?);
        }
        if events.len() == 50 {
            break;
        }
    }
    Ok(
        json!({"namespaces":spaces,"agents":agents,"audit":events,"limits":{"documents":4096,"bytes":33554432,"route_namespaces":8,"audit_visible":50},"credentials":"Agent凭据由部署配置绑定；管理页面不签发或读取token"}),
    )
}
