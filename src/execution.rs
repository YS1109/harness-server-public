//! Low-frequency durable orchestration snapshots, separate from knowledge routing.
use crate::{
    document::{Documents, parse, path_kind, render, sha, slug},
    error::{Error, Result, conflict, invalid},
    store::State,
};
use chrono::Utc;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn decode<T: DeserializeOwned>(v: &Value) -> Result<T> {
    serde_json::from_value(v.clone()).map_err(|_| invalid("执行记录字段缺失、类型错误或含未知字段"))
}
fn text(s: &str, max: usize, required: bool) -> Result<()> {
    if s.chars().count() > max || (required && s.trim().is_empty()) {
        return Err(invalid(format!("执行记录文本为空或超过{max}字")));
    }
    Ok(())
}
fn now() -> String {
    Utc::now().to_rfc3339()
}
fn path(kind: &str, id: &str) -> Result<String> {
    slug(id)?;
    Ok(format!("executions/{kind}/{id}.md"))
}
fn read<T: DeserializeOwned>(docs: &Documents, p: &str) -> Result<T> {
    let c = docs
        .get(p)
        .ok_or_else(|| Error::NotFound("执行记录不存在".into()))?;
    let (m, _) = parse(c)?;
    decode(&json!(m))
}
fn save<T: Serialize>(docs: &mut Documents, p: String, record: &T, body: &str) -> Result<()> {
    let value = serde_json::to_value(record).map_err(|_| invalid("执行记录无法序列化"))?;
    let content = render(
        value
            .as_object()
            .ok_or_else(|| invalid("执行记录必须为对象"))?,
        body,
    )?;
    crate::document::validate(&p, &content)?;
    docs.insert(p, content);
    Ok(())
}
fn cas(docs: &Documents, p: &str, expected: &str) -> Result<()> {
    match docs.get(p) {
        Some(c) if sha(c) == expected => Ok(()),
        None if expected.is_empty() => Ok(()),
        _ => Err(conflict(
            "执行记录SHA-256冲突，请核验当前任务与执行尝试后合并",
        )),
    }
}
fn bump(version: &mut u64) -> Result<()> {
    *version = version
        .checked_add(1)
        .ok_or_else(|| invalid("执行记录版本溢出"))?;
    Ok(())
}
fn is_terminal(status: &str) -> bool {
    matches!(status, "SUCCEEDED" | "FAILED" | "CANCELLED")
}
fn status_valid(status: &str) -> bool {
    matches!(
        status,
        "QUEUED"
            | "DISPATCHING"
            | "RUNNING"
            | "WAITING_USER"
            | "RECONCILING"
            | "SUCCEEDED"
            | "FAILED"
            | "CANCELLED"
    )
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Task {
    schema_version: u32,
    task_id: String,
    conversation_id: String,
    project: String,
    parent_task_id: Option<String>,
    source_key: String,
    source_fingerprint: String,
    title: String,
    goal: String,
    acceptance: String,
    status: String,
    version: u64,
    current_attempt_id: Option<String>,
    attempt_count: u64,
    current: String,
    cancel_requested: bool,
    delivery_status: String,
    delivery_key: String,
    delivery_summary: String,
    created_at: String,
    updated_at: String,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Checkpoint {
    current: String,
    completed_steps: Vec<String>,
    next_steps: Vec<String>,
    blockers: Vec<String>,
    verification: Vec<String>,
    artifacts: Vec<String>,
}
impl Checkpoint {
    fn validate(&self) -> Result<()> {
        text(&self.current, 400, true)?;
        let groups = [
            (&self.completed_steps, 8, 120),
            (&self.next_steps, 5, 120),
            (&self.blockers, 5, 120),
            (&self.verification, 5, 120),
            (&self.artifacts, 8, 200),
        ];
        let mut count = self.current.chars().count();
        for (items, max_items, max_text) in groups {
            if items.len() > max_items {
                return Err(invalid("检查点条目数量超限"));
            }
            for s in items {
                text(s, max_text, true)?;
                count += s.chars().count();
            }
        }
        if count > 1200 {
            return Err(invalid("检查点总文本超过1200字，不自动截断"));
        }
        Ok(())
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    schema_version: u32,
    attempt_id: String,
    number: u64,
    task_id: String,
    title: String,
    agent: String,
    native_session_id: Option<String>,
    status: String,
    version: u64,
    checkpoint: Option<Checkpoint>,
    created_at: String,
    updated_at: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Context {
    schema_version: u32,
    conversation_id: String,
    title: String,
    goal: String,
    constraints: Vec<String>,
    open_questions: Vec<String>,
    task_ids: Vec<String>,
    version: u64,
    created_at: String,
    updated_at: String,
}
fn save_task(docs: &mut Documents, t: &Task) -> Result<()> {
    save(
        docs,
        path("tasks", &t.task_id)?,
        t,
        &format!(
            "# {}\n\n目标：{}\n\n验收：{}\n",
            t.title, t.goal, t.acceptance
        ),
    )
}
fn save_attempt(docs: &mut Documents, a: &Attempt) -> Result<()> {
    // The structured checkpoint is in front matter; avoid duplicating it in the body.
    save(
        docs,
        path("attempts", &a.attempt_id)?,
        a,
        &format!("# {}\n", a.title),
    )
}
fn task_response(docs: &Documents, t: &Task) -> Result<Value> {
    let p = path("tasks", &t.task_id)?;
    let latest = t
        .current_attempt_id
        .as_ref()
        .map(|id| read::<Attempt>(docs, &path("attempts", id)?))
        .transpose()?;
    Ok(json!({"task":t,"sha256":sha(&docs[&p]),"latest_attempt":latest}))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Create {
    task_id: Option<String>,
    conversation_id: String,
    project: String,
    parent_task_id: Option<String>,
    source_key: String,
    title: String,
    goal: String,
    #[serde(default)]
    acceptance: String,
}
pub fn create(state: &mut State, v: &Value) -> Result<Value> {
    let mut c: Create = decode(v)?;
    c.project = crate::document::canonical(&state.documents, &c.project)?;
    if c.project == "daily" {
        return Ok(json!({"mode":"SKIP"}));
    }
    slug(&c.conversation_id)?;
    slug(&c.source_key)?;
    if let Some(id) = &c.task_id {
        slug(id)?;
    }
    text(&c.title, 100, true)?;
    text(&c.goal, 600, true)?;
    text(&c.acceptance, 600, false)?;
    let fingerprint = sha(&serde_json::to_string(&c).map_err(|_| invalid("任务无法序列化"))?);
    for (p, _) in state
        .documents
        .iter()
        .filter(|(p, _)| p.starts_with("executions/tasks/"))
    {
        let t: Task = read(&state.documents, p)?;
        if t.source_key == c.source_key {
            if t.source_fingerprint != fingerprint {
                return Err(conflict("来源去重键已绑定不同任务需求"));
            }
            let mut response = task_response(&state.documents, &t)?;
            response["created"] = json!(false);
            response["deduplicated"] = json!(true);
            return Ok(response);
        }
    }
    if let Some(parent) = &c.parent_task_id {
        let t: Task = read(&state.documents, &path("tasks", parent)?)?;
        if t.conversation_id != c.conversation_id {
            return Err(conflict("父任务属于其他调度会话"));
        }
    }
    let id = c
        .task_id
        .unwrap_or_else(|| format!("task-{}", uuid::Uuid::new_v4()));
    let p = path("tasks", &id)?;
    if state.documents.contains_key(&p) {
        return Err(conflict("task_id已经存在"));
    }
    let timestamp = now();
    let t = Task {
        schema_version: 1,
        task_id: id.clone(),
        conversation_id: c.conversation_id,
        project: c.project,
        parent_task_id: c.parent_task_id,
        source_key: c.source_key,
        source_fingerprint: fingerprint,
        title: c.title,
        goal: c.goal,
        acceptance: c.acceptance,
        status: "QUEUED".into(),
        version: 1,
        current_attempt_id: None,
        attempt_count: 0,
        current: String::new(),
        cancel_requested: false,
        delivery_status: "NONE".into(),
        delivery_key: format!("result-{id}-0"),
        delivery_summary: String::new(),
        created_at: timestamp.clone(),
        updated_at: timestamp,
    };
    save_task(&mut state.documents, &t)?;
    let mut response = task_response(&state.documents, &t)?;
    response["created"] = json!(true);
    response["deduplicated"] = json!(false);
    Ok(response)
}
pub fn get(docs: &Documents, id: &str) -> Result<Value> {
    task_response(docs, &read(docs, &path("tasks", id)?)?)
}
pub fn get_attempt(docs: &Documents, task_id: &str, attempt_id: &str) -> Result<Value> {
    let p = path("attempts", attempt_id)?;
    let a: Attempt = read(docs, &p)?;
    if a.task_id != task_id {
        return Err(conflict("执行尝试不属于该任务"));
    }
    Ok(json!({"attempt":a,"sha256":sha(&docs[&p])}))
}
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct List {
    conversation_id: Option<String>,
    project: Option<String>,
    status: Option<String>,
    active_only: bool,
    limit: usize,
    offset: usize,
}
impl Default for List {
    fn default() -> Self {
        Self {
            conversation_id: None,
            project: None,
            status: None,
            active_only: false,
            limit: 20,
            offset: 0,
        }
    }
}
pub fn list(docs: &Documents, v: &Value) -> Result<Value> {
    let mut q: List = decode(v)?;
    if let Some(id) = &q.conversation_id {
        slug(id)?;
    }
    if let Some(project) = &q.project {
        q.project = Some(crate::document::canonical(docs, project)?);
        if q.project.as_deref() == Some("daily") {
            return Ok(json!({"mode":"SKIP"}));
        }
    }
    if !(1..=50).contains(&q.limit) || q.status.as_ref().is_some_and(|s| !status_valid(s)) {
        return Err(invalid("任务列表limit或status无效"));
    }
    let mut tasks = Vec::new();
    for (p, _) in docs
        .iter()
        .filter(|(p, _)| p.starts_with("executions/tasks/"))
    {
        let t: Task = read(docs, p)?;
        if q.conversation_id
            .as_ref()
            .is_some_and(|id| *id != t.conversation_id)
            || q.project.as_ref().is_some_and(|id| *id != t.project)
            || q.status.as_ref().is_some_and(|s| *s != t.status)
            || (q.active_only && is_terminal(&t.status))
        {
            continue;
        }
        tasks.push(json!({"task_id":t.task_id,"conversation_id":t.conversation_id,"project":t.project,"parent_task_id":t.parent_task_id,"title":t.title,"status":t.status,"current":t.current,"current_attempt_id":t.current_attempt_id,"cancel_requested":t.cancel_requested,"delivery_status":t.delivery_status,"updated_at":t.updated_at,"version":t.version,"sha256":sha(&docs[p])}));
    }
    let total = tasks.len();
    let items: Vec<_> = tasks.into_iter().skip(q.offset).take(q.limit).collect();
    Ok(
        json!({"tasks":items,"total":total,"offset":q.offset,"remaining":total.saturating_sub(q.offset.saturating_add(items.len())),"order":"task_id ascending"}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Begin {
    expected_sha256: String,
    agent: String,
    native_session_id: Option<String>,
}
pub fn begin_attempt(state: &mut State, task_id: &str, v: &Value) -> Result<Value> {
    let c: Begin = decode(v)?;
    slug(&c.agent)?;
    if let Some(id) = &c.native_session_id {
        slug(id)?;
    }
    let p = path("tasks", task_id)?;
    let mut t: Task = read(&state.documents, &p)?;
    cas(&state.documents, &p, &c.expected_sha256)?;
    if !matches!(t.status.as_str(), "QUEUED" | "FAILED") || t.cancel_requested {
        return Err(conflict(
            "只有待执行或明确失败且未取消的任务可创建新尝试；未知结果先核验",
        ));
    }
    let timestamp = now();
    let a = Attempt {
        schema_version: 1,
        attempt_id: format!("attempt-{}", uuid::Uuid::new_v4()),
        number: t
            .attempt_count
            .checked_add(1)
            .ok_or_else(|| invalid("尝试数量溢出"))?,
        task_id: task_id.into(),
        title: t.title.clone(),
        agent: c.agent,
        native_session_id: c.native_session_id,
        status: "DISPATCHING".into(),
        version: 1,
        checkpoint: None,
        created_at: timestamp.clone(),
        updated_at: timestamp.clone(),
    };
    t.status = "DISPATCHING".into();
    t.current_attempt_id = Some(a.attempt_id.clone());
    t.attempt_count = t
        .attempt_count
        .checked_add(1)
        .ok_or_else(|| invalid("尝试数量溢出"))?;
    t.delivery_status = "NONE".into();
    t.delivery_key = format!("result-{}-{}", t.task_id, t.attempt_count);
    t.delivery_summary.clear();
    t.current.clear();
    t.updated_at = timestamp;
    bump(&mut t.version)?;
    save_attempt(&mut state.documents, &a)?;
    save_task(&mut state.documents, &t)?;
    task_response(&state.documents, &t)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    expected_sha256: String,
    attempt_id: String,
    status: String,
    native_session_id: Option<String>,
    checkpoint: Checkpoint,
}
fn transition(from: &str, to: &str) -> bool {
    match from {
        "DISPATCHING" => matches!(
            to,
            "DISPATCHING" | "RUNNING" | "RECONCILING" | "FAILED" | "CANCELLED"
        ),
        "RUNNING" => matches!(
            to,
            "RUNNING" | "WAITING_USER" | "RECONCILING" | "SUCCEEDED" | "FAILED" | "CANCELLED"
        ),
        "WAITING_USER" => matches!(
            to,
            "WAITING_USER" | "RUNNING" | "RECONCILING" | "FAILED" | "CANCELLED"
        ),
        "RECONCILING" => matches!(
            to,
            "RECONCILING" | "RUNNING" | "SUCCEEDED" | "FAILED" | "CANCELLED"
        ),
        _ => false,
    }
}
pub fn checkpoint(state: &mut State, task_id: &str, v: &Value) -> Result<Value> {
    let c: Update = decode(v)?;
    c.checkpoint.validate()?;
    let p = path("tasks", task_id)?;
    let mut t: Task = read(&state.documents, &p)?;
    cas(&state.documents, &p, &c.expected_sha256)?;
    if t.current_attempt_id.as_deref() != Some(&c.attempt_id) {
        return Err(conflict("旧执行尝试不能更新当前任务"));
    }
    if !transition(&t.status, &c.status) {
        return Err(conflict("不允许的执行状态迁移"));
    }
    let mut a: Attempt = read(&state.documents, &path("attempts", &c.attempt_id)?)?;
    if let Some(id) = c.native_session_id {
        slug(&id)?;
        if a.native_session_id.as_ref().is_some_and(|old| *old != id) {
            return Err(conflict("原生执行会话已绑定，不能替换"));
        }
        a.native_session_id = Some(id);
    }
    t.status = c.status.clone();
    t.current = c.checkpoint.current.clone();
    t.updated_at = now();
    bump(&mut t.version)?;
    if is_terminal(&t.status) {
        t.delivery_status = "PENDING".into();
    }
    a.status = c.status;
    a.checkpoint = Some(c.checkpoint);
    a.updated_at = t.updated_at.clone();
    bump(&mut a.version)?;
    save_attempt(&mut state.documents, &a)?;
    save_task(&mut state.documents, &t)?;
    task_response(&state.documents, &t)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cancel {
    expected_sha256: String,
}
pub fn cancel(state: &mut State, task_id: &str, v: &Value) -> Result<Value> {
    let c: Cancel = decode(v)?;
    let p = path("tasks", task_id)?;
    let mut t: Task = read(&state.documents, &p)?;
    cas(&state.documents, &p, &c.expected_sha256)?;
    if is_terminal(&t.status) {
        return Err(conflict("已结束任务不能请求取消"));
    }
    t.cancel_requested = true;
    if t.status == "QUEUED" {
        t.status = "CANCELLED".into();
        t.delivery_status = "PENDING".into();
    }
    t.updated_at = now();
    bump(&mut t.version)?;
    save_task(&mut state.documents, &t)?;
    task_response(&state.documents, &t)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delivery {
    expected_sha256: String,
    delivery_key: String,
    status: String,
    #[serde(default)]
    summary: String,
}
pub fn delivery(state: &mut State, task_id: &str, v: &Value) -> Result<Value> {
    let c: Delivery = decode(v)?;
    text(&c.summary, 240, false)?;
    let p = path("tasks", task_id)?;
    let mut t: Task = read(&state.documents, &p)?;
    cas(&state.documents, &p, &c.expected_sha256)?;
    if c.delivery_key != t.delivery_key {
        return Err(conflict("旧尝试的回复不能标记当前结果已投递"));
    }
    let allowed = match t.delivery_status.as_str() {
        "PENDING" => c.status == "SENDING",
        "SENDING" => matches!(c.status.as_str(), "SENT" | "UNKNOWN"),
        "UNKNOWN" => matches!(c.status.as_str(), "SENT" | "PENDING"),
        _ => false,
    };
    if !is_terminal(&t.status) || !allowed {
        return Err(conflict("回复投递状态迁移无效；未知结果先核验"));
    }
    if t.delivery_status == "UNKNOWN" && c.summary.trim().is_empty() {
        return Err(invalid("解除未知投递状态需要核验说明"));
    }
    t.delivery_status = c.status;
    t.delivery_summary = c.summary;
    t.updated_at = now();
    bump(&mut t.version)?;
    save_task(&mut state.documents, &t)?;
    task_response(&state.documents, &t)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextPut {
    #[serde(default)]
    expected_sha256: String,
    title: String,
    goal: String,
    #[serde(default)]
    constraints: Vec<String>,
    #[serde(default)]
    open_questions: Vec<String>,
    #[serde(default)]
    task_ids: Vec<String>,
}
fn validate_context(c: &Context) -> Result<()> {
    slug(&c.conversation_id)?;
    text(&c.title, 100, true)?;
    text(&c.goal, 400, true)?;
    if c.constraints.len() > 8 || c.open_questions.len() > 8 || c.task_ids.len() > 20 {
        return Err(invalid("调度上下文条目数量超限"));
    }
    let mut chars = c.title.chars().count() + c.goal.chars().count();
    for s in c.constraints.iter().chain(&c.open_questions) {
        text(s, 200, true)?;
        chars += s.chars().count();
    }
    let mut ids = BTreeSet::new();
    for id in &c.task_ids {
        slug(id)?;
        if !ids.insert(id) {
            return Err(invalid("重复task_id"));
        }
    }
    if chars > 1200 {
        return Err(invalid("调度上下文摘要超过1200字"));
    }
    Ok(())
}
pub fn put_context(state: &mut State, id: &str, v: &Value) -> Result<Value> {
    let c: ContextPut = decode(v)?;
    let p = path("contexts", id)?;
    cas(&state.documents, &p, &c.expected_sha256)?;
    let old = state
        .documents
        .contains_key(&p)
        .then(|| read::<Context>(&state.documents, &p))
        .transpose()?;
    let mut record = Context {
        schema_version: 1,
        conversation_id: id.into(),
        title: c.title,
        goal: c.goal,
        constraints: c.constraints,
        open_questions: c.open_questions,
        task_ids: c.task_ids,
        version: old.as_ref().map_or(0, |c| c.version),
        created_at: old.map_or_else(now, |c| c.created_at),
        updated_at: now(),
    };
    bump(&mut record.version)?;
    validate_context(&record)?;
    save(
        &mut state.documents,
        p,
        &record,
        &format!("# {}\n", record.title),
    )?;
    get_context(&state.documents, id)
}
pub fn get_context(docs: &Documents, id: &str) -> Result<Value> {
    let p = path("contexts", id)?;
    let c: Context = read(docs, &p)?;
    Ok(json!({"context":c,"sha256":sha(&docs[&p])}))
}

fn base_valid(schema: u32, version: u64, created: &str, updated: &str) -> Result<()> {
    let created =
        chrono::DateTime::parse_from_rfc3339(created).map_err(|_| invalid("执行记录时间无效"))?;
    let updated =
        chrono::DateTime::parse_from_rfc3339(updated).map_err(|_| invalid("执行记录时间无效"))?;
    if schema != 1 || version == 0 || updated < created {
        return Err(invalid("执行记录schema、版本或时间无效"));
    }
    Ok(())
}
pub fn validate_document(p: &str, content: &str) -> Result<()> {
    let (m, body) = parse(content)?;
    if body.chars().count() > 1600 {
        return Err(invalid("执行记录正文超过1600字"));
    }
    match path_kind(p)? {
        "execution_task" => {
            let t: Task = decode(&json!(m))?;
            base_valid(t.schema_version, t.version, &t.created_at, &t.updated_at)?;
            if path("tasks", &t.task_id)? != p || t.project == "daily" || !status_valid(&t.status) {
                return Err(invalid("任务路径、项目或状态无效"));
            }
            slug(&t.project)?;
            slug(&t.conversation_id)?;
            slug(&t.source_key)?;
            text(&t.title, 100, true)?;
            text(&t.goal, 600, true)?;
            text(&t.acceptance, 600, false)?;
            text(&t.current, 400, false)?;
            text(&t.delivery_summary, 240, false)?;
            if t.source_fingerprint.len() != 64
                || !t.source_fingerprint.bytes().all(|c| c.is_ascii_hexdigit())
                || t.delivery_key != format!("result-{}-{}", t.task_id, t.attempt_count)
                || !matches!(
                    t.delivery_status.as_str(),
                    "NONE" | "PENDING" | "SENDING" | "SENT" | "UNKNOWN"
                )
            {
                return Err(invalid("任务来源或投递记录无效"));
            }
        }
        "execution_attempt" => {
            let a: Attempt = decode(&json!(m))?;
            base_valid(a.schema_version, a.version, &a.created_at, &a.updated_at)?;
            if a.number == 0
                || path("attempts", &a.attempt_id)? != p
                || !status_valid(&a.status)
                || a.status == "QUEUED"
            {
                return Err(invalid("尝试路径或状态无效"));
            }
            slug(&a.task_id)?;
            slug(&a.agent)?;
            text(&a.title, 100, true)?;
            if let Some(id) = a.native_session_id {
                slug(&id)?;
            }
            if let Some(c) = a.checkpoint {
                c.validate()?;
            }
        }
        "execution_context" => {
            let c: Context = decode(&json!(m))?;
            base_valid(c.schema_version, c.version, &c.created_at, &c.updated_at)?;
            if path("contexts", &c.conversation_id)? != p {
                return Err(invalid("调度上下文身份无效"));
            }
            validate_context(&c)?;
        }
        _ => return Err(invalid("不是执行记录")),
    }
    Ok(())
}
pub fn validate_links(docs: &Documents) -> Result<()> {
    let mut tasks = BTreeMap::<String, Task>::new();
    let mut attempts = BTreeMap::<String, Attempt>::new();
    let mut contexts = Vec::<Context>::new();
    for (p, _) in docs.iter().filter(|(p, _)| p.starts_with("executions/")) {
        match path_kind(p)? {
            "execution_task" => {
                let t: Task = read(docs, p)?;
                tasks.insert(t.task_id.clone(), t);
            }
            "execution_attempt" => {
                let a: Attempt = read(docs, p)?;
                attempts.insert(a.attempt_id.clone(), a);
            }
            "execution_context" => contexts.push(read(docs, p)?),
            _ => {}
        }
    }
    let mut source_keys = BTreeSet::new();
    let mut numbers = BTreeMap::<String, BTreeSet<u64>>::new();
    for a in attempts.values() {
        let t = tasks
            .get(&a.task_id)
            .ok_or_else(|| invalid("尝试关联任务不存在"))?;
        if a.number > t.attempt_count
            || !numbers
                .entry(a.task_id.clone())
                .or_default()
                .insert(a.number)
        {
            return Err(invalid("尝试序号重复或越界"));
        }
        if t.current_attempt_id.as_deref() != Some(&a.attempt_id) && !is_terminal(&a.status) {
            return Err(invalid("历史尝试仍在执行"));
        }
    }
    for t in tasks.values() {
        if !source_keys.insert(&t.source_key) {
            return Err(invalid("重复任务来源去重键"));
        }
        let mut chain = BTreeSet::new();
        chain.insert(&t.task_id);
        let mut parent_id = t.parent_task_id.as_ref();
        while let Some(id) = parent_id {
            let parent = tasks.get(id).ok_or_else(|| invalid("父任务不存在"))?;
            if parent.conversation_id != t.conversation_id || !chain.insert(&parent.task_id) {
                return Err(invalid("父任务跨会话或存在循环"));
            }
            parent_id = parent.parent_task_id.as_ref();
        }
        if numbers.get(&t.task_id).map_or(0, |s| s.len()) as u64 != t.attempt_count {
            return Err(invalid("执行尝试数量不一致"));
        }
        if let Some(id) = &t.current_attempt_id {
            let a = attempts.get(id).ok_or_else(|| invalid("当前尝试不存在"))?;
            if a.task_id != t.task_id
                || a.number != t.attempt_count
                || a.status != t.status
                || a.checkpoint.as_ref().map_or("", |c| c.current.as_str()) != t.current
            {
                return Err(invalid("任务与当前尝试不一致"));
            }
        } else if t.attempt_count != 0 || !matches!(t.status.as_str(), "QUEUED" | "CANCELLED") {
            return Err(invalid("任务缺少执行尝试"));
        }
        if is_terminal(&t.status) == (t.delivery_status == "NONE") {
            return Err(invalid("任务执行和结果投递状态不一致"));
        }
    }
    for c in contexts {
        for id in c.task_ids {
            let t = tasks
                .get(&id)
                .ok_or_else(|| invalid("调度上下文关联任务不存在"))?;
            if t.conversation_id != c.conversation_id {
                return Err(invalid("调度上下文关联其他会话任务"));
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Page {
    limit: usize,
    offset: usize,
}
impl Default for Page {
    fn default() -> Self {
        Self {
            limit: 20,
            offset: 0,
        }
    }
}
pub fn list_attempts(docs: &Documents, task_id: &str, v: &Value) -> Result<Value> {
    let _: Task = read(docs, &path("tasks", task_id)?)?;
    let q: Page = decode(v)?;
    if !(1..=50).contains(&q.limit) {
        return Err(invalid("limit必须为1–50"));
    }
    let mut rows = Vec::new();
    for (p, _) in docs
        .iter()
        .filter(|(p, _)| p.starts_with("executions/attempts/"))
    {
        let a: Attempt = read(docs, p)?;
        if a.task_id == task_id {
            rows.push(a);
        }
    }
    rows.sort_by_key(|a| std::cmp::Reverse(a.number));
    let total = rows.len();
    let rows:Vec<_>=rows.into_iter().skip(q.offset).take(q.limit).map(|a|json!({"attempt_id":a.attempt_id,"number":a.number,"agent":a.agent,"native_session_id":a.native_session_id,"status":a.status,"updated_at":a.updated_at})).collect();
    Ok(
        json!({"attempts":rows,"total":total,"offset":q.offset,"remaining":total.saturating_sub(q.offset.saturating_add(rows.len())),"order":"attempt number descending"}),
    )
}
