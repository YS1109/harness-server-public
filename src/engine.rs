use crate::{
    document::*,
    error::{Error, Result, conflict, invalid},
    store::State,
};
use chrono::Utc;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
fn required<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    let s = text(v, key);
    if s.is_empty() {
        Err(invalid(format!("缺少{key}")))
    } else {
        Ok(s)
    }
}
fn now() -> String {
    Utc::now().to_rfc3339()
}
fn meta(v: Value) -> Metadata {
    v.as_object().unwrap().clone()
}
fn check_hash(docs: &Documents, path: &str, expected: &str) -> Result<()> {
    match docs.get(path) {
        Some(c) if sha(c) == expected => Ok(()),
        Some(_) => Err(conflict("SHA-256冲突，请读取最新内容后合并")),
        None if expected.is_empty() => Ok(()),
        None => Err(conflict("目标不存在，不能沿用旧哈希")),
    }
}
fn item(docs: &Documents, path: &str) -> Value {
    json!({"path":path,"sha256":sha(&docs[path]),"content":docs[path]})
}
fn skip(project: &str) -> bool {
    project == "daily"
}
fn fields(v: &Value, allowed: &[&str]) -> Result<()> {
    let object = v.as_object().ok_or_else(|| invalid("请求必须是JSON对象"))?;
    if object.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(invalid("请求含不支持的字段；请按v1协议发送"));
    }
    Ok(())
}
fn terms(s: &str) -> BTreeSet<String> {
    let s = s.to_lowercase();
    let mut out = BTreeSet::new();
    for m in Regex::new(r"[a-z0-9]+(?:[_-][a-z0-9]+)*|[\p{Han}]+")
        .unwrap()
        .find_iter(&s)
    {
        let word = m.as_str();
        let chars: Vec<_> = word.chars().collect();
        if chars.first().is_some_and(|c| !c.is_ascii()) {
            for pair in chars.windows(2) {
                out.insert(pair.iter().collect());
            }
        } else {
            out.insert(word.to_owned());
        }
    }
    out
}
fn lexical_match(query: &str, term: &str) -> bool {
    if term.is_empty() {
        return false;
    }
    if term
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
    {
        Regex::new(&format!(
            r"(?:^|[^a-z0-9_./-]){}(?:$|[^a-z0-9_./-])",
            regex::escape(term)
        ))
        .unwrap()
        .is_match(query)
    } else {
        query.contains(term)
    }
}
pub fn route(docs: &Documents, v: &Value) -> Result<Value> {
    route_sources(docs, v, None)
}
/// Rank all authorized origins together; paths remain distinct until response projection.
pub fn route_scoped(docs: &Documents, scopes: &[String], v: &Value) -> Result<Value> {
    let mut sources = Documents::new();
    for ns in scopes {
        for (path, content) in crate::namespace::view(docs, ns, false) {
            if crate::namespace::shared_memory(&path) {
                sources.insert(crate::namespace::physical(ns, &path), content);
            }
        }
    }
    let mut result = route_sources(&sources, v, Some(scopes))?;
    if result["mode"] == "SKIP" {
        return Ok(result);
    }
    let source_refs: Vec<_> = result["project_memory"]
        .as_str()
        .into_iter()
        .chain(
            result["project_topics"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        )
        .chain(
            result["process_memories"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        )
        .map(|p| {
            let (ns, path) = crate::namespace::split(p).expect("validated source");
            json!({"namespace":ns,"path":path})
        })
        .collect();
    result["memory_sources"] = json!(source_refs);
    // Strings in reference fields become qualified references for multi-space consumers.
    for key in ["project_memory", "project_topics", "process_memories"] {
        let qualify = |path: &str| -> Value {
            let (ns, logical) = crate::namespace::split(path).expect("validated source");
            json!({"namespace":ns,"path":logical})
        };
        if scopes.len() > 1 {
            if let Some(path) = result[key].as_str() {
                result[key] = qualify(path);
            } else if let Some(paths) = result[key].as_array() {
                result[key] = json!(
                    paths
                        .iter()
                        .map(|p| qualify(p.as_str().unwrap()))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
    if scopes.len() == 1 {
        for key in ["project_memory", "project_topics", "process_memories"] {
            if let Some(p) = result[key].as_str() {
                result[key] = json!(crate::namespace::split(p)?.1);
            } else if let Some(paths) = result[key].as_array() {
                result[key] = json!(
                    paths
                        .iter()
                        .map(|v| crate::namespace::split(v.as_str().unwrap())
                            .map(|(_, p)| p.to_owned()))
                        .collect::<Result<Vec<_>>>()?
                );
            }
        }
    }
    fn origins(value: &mut Value) {
        match value {
            Value::Array(a) => {
                for v in a {
                    origins(v)
                }
            }
            Value::Object(o) => {
                if let Some(p) = o.get("path").and_then(Value::as_str).map(str::to_owned) {
                    let (ns, path) = crate::namespace::split(&p).expect("validated source");
                    if !o.contains_key("namespace") {
                        o.insert("namespace".into(), json!(ns));
                    }
                    o.insert("path".into(), json!(path));
                }
                for v in o.values_mut() {
                    origins(v);
                }
            }
            _ => {}
        }
    }
    origins(&mut result);
    result["read_namespaces"] = json!(scopes);
    Ok(result)
}
fn route_sources(docs: &Documents, v: &Value, scopes: Option<&[String]>) -> Result<Value> {
    fields(
        v,
        &[
            "project",
            "scenario",
            "topic",
            "topic_namespace",
            "content",
            "candidates_only",
            "candidate_offset",
        ],
    )?;
    let project = required(v, "project")?;
    if skip(project) {
        return Ok(json!({"mode":"SKIP"}));
    }
    let project = if let Some(scopes) = scopes {
        let mut ids = BTreeSet::new();
        for ns in scopes {
            let local = crate::namespace::view(docs, ns, false);
            let id = canonical(&local, project)?;
            if local.contains_key(&format!("memory/project/{id}.md")) {
                ids.insert(id);
            }
        }
        if ids.len() > 1 {
            return Err(invalid("跨空间项目别名存在歧义；使用规范project ID"));
        }
        ids.into_iter().next().unwrap_or_else(|| project.to_owned())
    } else {
        canonical(docs, project)?
    };
    let raw = text(v, "scenario").to_lowercase();
    // Negated clauses cannot promote automatic selections or weak candidates.
    let query = Regex::new(r"(?:不要|不用|别|无需|不需要|不再|不做|不改|禁止)[^，,。；;]*")
        .unwrap()
        .replace_all(&raw, "")
        .to_string();
    let offset = v
        .get("candidate_offset")
        .map(|x| {
            x.as_u64()
                .ok_or_else(|| invalid("candidate_offset必须是非负整数"))
        })
        .transpose()?
        .unwrap_or(0) as usize;
    let only = v
        .get("candidates_only")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || offset > 0;
    let explicit = text(v, "topic");
    if !explicit.is_empty() {
        slug(explicit)?;
        if only {
            return Err(invalid("显式专题与候选分页不可混用"));
        }
    }
    let parents: Vec<String> = if let Some(scopes) = scopes {
        scopes
            .iter()
            .map(|ns| crate::namespace::physical(ns, &format!("memory/project/{project}.md")))
            .collect()
    } else {
        vec![format!("memory/project/{project}.md")]
    };
    let parents: Vec<_> = parents
        .into_iter()
        .filter(|p| {
            docs.get(p)
                .is_some_and(|c| parse(c).is_ok_and(|(m, _)| string(&m, "status") == "ACTIVE"))
        })
        .collect();
    let parent = parents.first().cloned();
    if parent.is_none() && !explicit.is_empty() {
        return Err(invalid("没有ACTIVE项目入口"));
    }
    let mut topics = Vec::new();
    let query_terms = terms(&query);
    let symbol_pattern = Regex::new(r"\b[A-Za-z][A-Za-z0-9]*(?:[_-][A-Za-z0-9]+)+\b").unwrap();
    if parent.is_some() {
        for (p, c) in docs {
            if path_kind(p)? != "project_topic" {
                continue;
            }
            let (m, body) = parse(c)?;
            let (ns, _) = crate::namespace::split(p)?;
            if !parents.contains(&crate::namespace::physical(
                ns,
                &format!("memory/project/{project}.md"),
            )) {
                continue;
            }
            if string(&m, "project_id") != project || string(&m, "status") != "ACTIVE" {
                continue;
            }
            let mut score = 0;
            let id = string(&m, "topic_id").to_owned();
            for (list, weight) in [
                (vec![id.clone()], 8),
                (strings(&m, "aliases"), 6),
                (vec![string(&m, "title").to_owned()], 4),
                (strings(&m, "triggers"), 4),
            ] {
                for term in list {
                    if lexical_match(&query, &term.to_lowercase()) {
                        score += weight;
                    }
                }
            }
            let symbols: BTreeSet<String> = symbol_pattern
                .find_iter(&body)
                .map(|s| s.as_str().to_owned())
                .collect();
            let mut affinity = 0;
            for symbol in symbols {
                if lexical_match(&query, &symbol.to_lowercase()) {
                    score += 5;
                    affinity += symbol
                        .split(['_', '-'])
                        .filter(|part| part.len() >= 3 && id.split('-').any(|p| p == *part))
                        .count();
                }
            }
            let description = format!(
                "{} {} {} {} {}",
                id,
                string(&m, "title"),
                string(&m, "summary"),
                strings(&m, "aliases").join(" "),
                strings(&m, "triggers").join(" ")
            );
            let weak = query_terms.intersection(&terms(&description)).count();
            topics.push((score, affinity, weak, id, p.clone(), m));
        }
    }
    topics.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.3.cmp(&b.3)));
    let ambiguous = topics.len() > 1
        && topics[0].0 > 0
        && topics[0].0 == topics[1].0
        && topics[0].1 == topics[1].1;
    let selected = if !explicit.is_empty() {
        let matches: Vec<_> = topics
            .iter()
            .filter(|t| {
                t.3 == explicit
                    && (text(v, "topic_namespace").is_empty()
                        || crate::namespace::split(&t.4)
                            .is_ok_and(|(ns, _)| ns == text(v, "topic_namespace")))
            })
            .collect();
        if matches.len() > 1 {
            return Err(invalid("专题ID跨空间重复；指定topic_namespace"));
        }
        Some(
            matches
                .first()
                .ok_or_else(|| invalid("专题不存在或不属于项目/读取空间"))?
                .4
                .clone(),
        )
    } else if !only && !ambiguous && topics.first().is_some_and(|t| t.0 > 0) {
        Some(topics[0].4.clone())
    } else {
        None
    };
    let outcome = if parent.is_none() {
        "no-project"
    } else if topics.is_empty() {
        "no-topics"
    } else if !explicit.is_empty() {
        "explicit"
    } else if selected.is_some() {
        "auto"
    } else if only {
        "candidates-only"
    } else if query.trim().is_empty() {
        "no-query"
    } else if ambiguous {
        "ambiguous"
    } else {
        "fallback"
    };
    let mut candidates = Vec::new();
    let mut total = 0;
    if selected.is_none() && (only || !query.trim().is_empty()) {
        topics.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(b.1.cmp(&a.1))
                .then_with(|| {
                    if a.0 == 0 {
                        b.2.cmp(&a.2)
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .then(a.3.cmp(&b.3))
        });
        total = topics.len();
        for t in topics.iter().skip(offset).take(3) {
            candidates.push(json!({"topic_id":t.3,"title":string(&t.5,"title"),"summary":string(&t.5,"summary"),"path":t.4}));
        }
    }
    let mut processes = Vec::new();
    for (p, c) in docs {
        if path_kind(p)? != "process" {
            continue;
        }
        let (m, _) = parse(c)?;
        if string(&m, "status") != "ACTIVE" {
            continue;
        }
        let score = strings(&m, "triggers")
            .iter()
            .filter(|s| !s.is_empty() && query.contains(&s.to_lowercase()))
            .count();
        if score > 0 {
            processes.push((
                score,
                m.get("load_priority")
                    .and_then(Value::as_i64)
                    .unwrap_or(100),
                p.clone(),
            ));
        }
    }
    processes.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let processes: Vec<_> = processes.into_iter().take(2).map(|t| t.2).collect();
    let selected: Vec<_> = selected.into_iter().collect();
    let mut result = json!({"mode":"LOAD","project":project,"project_memory":parent,"project_topics":selected,"process_memories":processes,"topic_candidates":candidates,"route_outcome":outcome,"topic_count":topics.len(),"candidate_total":total,"candidate_offset":offset,"candidate_remaining":total.saturating_sub(offset.saturating_add(candidates.len()))});
    if v.get("content").and_then(Value::as_bool).unwrap_or(false) {
        let paths = parent.into_iter().chain(processes).chain(selected);
        let content: Vec<_> = paths.map(|p| item(docs, &p)).collect();
        let count: usize = content
            .iter()
            .map(|c| text(c, "content").chars().count())
            .sum();
        result["content"] = json!(content);
        result["returned_characters"] = json!(count);
    }
    Ok(result)
}
pub fn get(docs: &Documents, path: &str) -> Result<Value> {
    path_kind(path)?;
    if !docs.contains_key(path) {
        return Err(Error::NotFound("文档不存在".into()));
    }
    Ok(item(docs, path))
}
pub fn list(docs: &Documents, v: &Value) -> Result<Value> {
    let kind = text(v, "kind");
    if !kind.is_empty()
        && !matches!(
            kind,
            "project" | "project_topic" | "process" | "session" | "workstream" | "log"
        )
    {
        return Err(invalid("kind无效"));
    }
    let project = text(v, "project");
    let project = if project.is_empty() {
        None
    } else {
        Some(canonical(docs, project)?)
    };
    if project.as_deref() == Some("daily") {
        return Ok(json!({"mode":"SKIP"}));
    }
    let limit = v["limit"].as_u64().unwrap_or(20) as usize;
    let offset = v["offset"].as_u64().unwrap_or(0) as usize;
    if !(1..=50).contains(&limit) {
        return Err(invalid("limit必须为1–50"));
    }
    let mut rows = Vec::new();
    for (path, content) in docs {
        let actual = path_kind(path)?;
        if actual.starts_with("execution_") {
            continue;
        }
        if !kind.is_empty() && actual != kind {
            continue;
        }
        if actual == "log" {
            if project.is_none() {
                rows.push(json!({"path":path,"kind":actual,"sha256":sha(content)}));
            }
            continue;
        }
        let (m, _) = parse(content)?;
        let owner = if string(&m, "project").is_empty() {
            string(&m, "project_id")
        } else {
            string(&m, "project")
        };
        if let Some(project) = &project
            && (owner.is_empty() || canonical(docs, owner)? != *project)
        {
            continue;
        }
        rows.push(json!({"path":path,"kind":actual,"sha256":sha(content),"title":string(&m,"title"),"project":owner,"status":string(&m,"status"),"updated_at":string(&m,"updated_at"),"session_id":string(&m,"session_id"),"workstream_id":string(&m,"workstream_id")}));
    }
    let total = rows.len();
    let rows: Vec<_> = rows.into_iter().skip(offset).take(limit).collect();
    Ok(
        json!({"documents":rows,"total":total,"offset":offset,"remaining":total.saturating_sub(offset.saturating_add(rows.len()))}),
    )
}
pub fn put(state: &mut State, v: &Value) -> Result<Value> {
    fields(v, &["path", "content", "expected_sha256"])?;
    let path = required(v, "path")?;
    let content = required(v, "content")?;
    if path_kind(path)?.starts_with("execution_") {
        return Err(invalid("执行记录必须通过专用API更新"));
    }
    if path_kind(path)? == "log" {
        return Err(invalid("日志只由finish追加，历史日志通过离线导入"));
    }
    check_hash(&state.documents, path, text(v, "expected_sha256"))?;
    validate(path, content)?;
    if let Some(old) = state.documents.get(path) {
        let (a, _) = parse(old)?;
        let (b, _) = parse(content)?;
        for key in [
            "project_id",
            "process_id",
            "topic_id",
            "session_id",
            "workstream_id",
            "project",
            "agent",
            "native_session_id",
            "parent_session_id",
            "created_at",
            "memory_type",
            "verified_at",
            "verification_evidence",
        ] {
            if a.get(key) != b.get(key) {
                return Err(invalid("受管身份和核验声明不能通过文档PUT更改"));
            }
        }
    }
    state.documents.insert(path.to_owned(), content.to_owned());
    reindex_topics(&mut state.documents)?;
    Ok(json!({"path":path,"sha256":sha(&state.documents[path]),"written":true}))
}
pub fn start(state: &mut State, v: &Value) -> Result<Value> {
    fields(
        v,
        &[
            "project",
            "agent",
            "native_session_id",
            "title",
            "workstream_id",
            "scenario",
            "content",
            "topic",
        ],
    )?;
    let project = required(v, "project")?;
    if skip(project) {
        return Ok(json!({"mode":"SKIP"}));
    }
    let project = canonical(&state.documents, project)?;
    let agent = required(v, "agent")?;
    let native = required(v, "native_session_id")?;
    slug(agent)?;
    slug(native)?;
    if native.starts_with(&format!("{agent}-")) {
        return Err(invalid("native_session_id不能包含Agent前缀"));
    }
    let session_id = format!("{agent}-{native}");
    slug(&session_id)?;
    let title = required(v, "title")?;
    if title.chars().count() > 100 || title.contains('\n') {
        return Err(invalid("title超过100字或含换行"));
    }
    let mut ws = if text(v, "workstream_id").is_empty() {
        format!("{project}-{session_id}")
    } else {
        text(v, "workstream_id").to_owned()
    };
    slug(&ws)?;
    let existing = find(&state.documents, "session_id", &session_id)?;
    if let Some(path) = &existing {
        let (m, _) = parse(&state.documents[path])?;
        if canonical(&state.documents, string(&m, "project"))? != project {
            return Err(conflict("Session项目与当前项目不一致，拒绝续接"));
        }
        let bound = string(&m, "workstream_id");
        if !text(v, "workstream_id").is_empty() && ws != bound {
            return Err(conflict("Session已绑定其他Workstream"));
        }
        ws = bound.to_owned();
    }
    let ws_path = if let Some(path) = find(&state.documents, "workstream_id", &ws)? {
        let (m, _) = parse(&state.documents[&path])?;
        if canonical(&state.documents, string(&m, "project"))? != project {
            return Err(conflict("Workstream属于其他项目"));
        }
        path
    } else {
        if existing.is_some() {
            return Err(invalid("已有关联Workstream丢失，拒绝重建"));
        }
        let path = format!("handoffs/workstreams/{ws}.md");
        let m = meta(
            json!({"schema_version":1,"workstream_id":ws,"title":title,"project":project,"status":"ACTIVE","record_state":"PROVISIONAL","created_at":now(),"updated_at":now()}),
        );
        state.documents.insert(
            path.clone(),
            render(
                &m,
                &format!("# {title}\n\n- 目标：{title}\n- 当前：等待执行。"),
            )?,
        );
        path
    };
    let ws_hash = sha(&state.documents[&ws_path]);
    let (mut sm, body) = if let Some(path) = &existing {
        parse(&state.documents[path])?
    } else {
        (
            meta(
                json!({"schema_version":1,"session_id":session_id,"agent":agent,"native_session_id":native,"project":project,"workstream_id":ws,"title":title,"record_state":"PROVISIONAL","created_at":now()}),
            ),
            format!("# {title}\n\n- 目标：{title}\n- 当前：等待执行。"),
        )
    };
    sm.insert("status".into(), json!("ACTIVE"));
    sm.insert("updated_at".into(), json!(now()));
    sm.insert("workstream_sha256_at_begin".into(), json!(ws_hash));
    if let Some(p) = &existing {
        state.documents.remove(p);
    }
    let sp = format!("handoffs/sessions/active/{session_id}.md");
    state.documents.insert(sp.clone(), render(&sm, &body)?);
    let memories = route(
        &state.documents,
        &json!({"project":project,"scenario":text(v,"scenario"),"topic":text(v,"topic"),"content":v.get("content").and_then(Value::as_bool).unwrap_or(false)}),
    )?;
    Ok(
        json!({"mode":"TRACKED","session_id":session_id,"workstream_id":ws,"workstream_sha256":ws_hash,"session_action":if existing.is_some(){"continued"}else{"created"},"execution_group_id":uuid::Uuid::new_v4().simple().to_string(),"memories":memories,"handoff":{"session":item(&state.documents,&sp),"workstream":item(&state.documents,&ws_path)},"handoff_characters":state.documents[&sp].chars().count()+state.documents[&ws_path].chars().count()}),
    )
}
fn state_body(title: &str, goal: &str, result: &Value) -> Result<String> {
    let current = required(result, "current")?;
    let list = |key: &str| -> Result<String> {
        let Some(v) = result.get(key) else {
            return Ok(if key == "verification" {
                "未执行"
            } else {
                "无"
            }
            .to_owned());
        };
        let array = v
            .as_array()
            .ok_or_else(|| invalid("交接列表必须是字符串数组"))?;
        if array.len() > 5 {
            return Err(invalid("交接列表超过5条"));
        }
        let mut items = Vec::new();
        for item in array {
            let s = item
                .as_str()
                .ok_or_else(|| invalid("交接列表只接受字符串"))?;
            if s.contains('\n') || s.chars().count() > 120 {
                return Err(invalid("交接条目超过120字或含换行"));
            }
            items.push(s);
        }
        Ok(if items.is_empty() {
            "无".to_owned()
        } else {
            items.join("；")
        })
    };
    Ok(format!(
        "# {title}\n\n- 目标：{goal}\n- 当前：{current}\n- 决策：{}。\n- 阻塞：{}。\n- 验证：{}。\n- 下一步：{}。",
        list("decisions")?,
        list("blockers")?,
        list("verification")?,
        list("next_steps")?
    ))
}
fn goal<'a>(body: &'a str, fallback: &'a str) -> &'a str {
    body.lines()
        .find_map(|l| l.strip_prefix("- 目标："))
        .unwrap_or(fallback)
}
fn memory_updates(docs: &mut Documents, project: &str, r: &Value) -> Result<usize> {
    let mut writes = 0;
    let mut seen = BTreeSet::new();
    if let Some(topics) = r.get("project_topics") {
        for t in topics
            .as_array()
            .ok_or_else(|| invalid("project_topics必须是数组"))?
        {
            fields(
                t,
                &[
                    "topic_id",
                    "title",
                    "summary",
                    "triggers",
                    "aliases",
                    "body",
                    "expected_sha256",
                    "verified_at",
                    "verification_evidence",
                ],
            )?;
            let id = required(t, "topic_id")?;
            slug(id)?;
            if !seen.insert(id) {
                return Err(invalid("同一专题重复更新"));
            }
            let parent = format!("memory/project/{project}.md");
            let pc = docs
                .get(&parent)
                .ok_or_else(|| invalid("创建专题前需项目入口"))?;
            if string(&parse(pc)?.0, "status") != "ACTIVE" {
                return Err(invalid("项目入口已停用"));
            }
            let p = format!("memory/project/{project}/{id}.md");
            check_hash(docs, &p, text(t, "expected_sha256"))?;
            let mut m = if let Some(c) = docs.get(&p) {
                let (m, _) = parse(c)?;
                if string(&m, "status") != "ACTIVE" {
                    return Err(invalid("不能隐式重新激活已归档专题"));
                }
                m
            } else {
                meta(
                    json!({"schema_version":1,"memory_type":"project_topic","project_id":project,"topic_id":id,"status":"ACTIVE","verified_at":"unknown"}),
                )
            };
            for key in ["title", "summary", "triggers"] {
                m.insert(
                    key.into(),
                    t.get(key)
                        .cloned()
                        .ok_or_else(|| invalid(format!("专题缺少{key}")))?,
                );
            }
            if let Some(a) = t.get("aliases") {
                m.insert("aliases".into(), a.clone());
            }
            if let Some(date) = t.get("verified_at") {
                m.insert("verified_at".into(), date.clone());
                m.remove("verification_evidence");
                if let Some(e) = t.get("verification_evidence") {
                    if e.as_str()
                        .is_none_or(|s| s.is_empty() || s.chars().count() > 240)
                    {
                        return Err(invalid("核验依据无效"));
                    }
                    m.insert("verification_evidence".into(), e.clone());
                }
            } else if t.get("verification_evidence").is_some() {
                return Err(invalid("核验依据必须与显式核验日期一起提供"));
            }
            m.insert("updated_at".into(), json!(now()));
            docs.insert(p, render(&m, required(t, "body")?)?);
            writes += 1;
        }
    }
    if let Some(learnings) = r.get("process_learnings") {
        let learnings = learnings
            .as_array()
            .ok_or_else(|| invalid("process_learnings必须是数组"))?;
        if !learnings.is_empty() {
            let id = required(r, "process_id")?;
            slug(id)?;
            let p = format!("memory/process/{id}.md");
            let (mut m, mut body) = if let Some(c) = docs.get(&p) {
                let (m, b) = parse(c)?;
                if string(&m, "status") != "ACTIVE" {
                    return Err(invalid("流程已停用"));
                }
                (m, b)
            } else {
                let title = if text(r, "process_title").is_empty() {
                    id
                } else {
                    text(r, "process_title")
                };
                (
                    meta(
                        json!({"schema_version":1,"memory_type":"process","process_id":id,"title":title,"scope":if id=="personal-coding-preferences"{"personal"}else{"global"},"status":"ACTIVE","verified_at":"unknown","updated_at":now(),"triggers":if id=="personal-coding-preferences"{json!(["coding","编码","代码","测试"])}else{json!([id])},"load_priority":if id=="personal-coding-preferences"{5}else{100},"maturity":"L1"}),
                    ),
                    format!("# {title}\n\n## 可复用方法\n"),
                )
            };
            let old = body.clone();
            for learning in learnings {
                let s = learning
                    .as_str()
                    .ok_or_else(|| invalid("流程增量必须是字符串"))?;
                if s.is_empty() || s.chars().count() > 240 || s.contains('\n') {
                    return Err(invalid("流程增量必须是1–240字单行文本"));
                }
                let line = format!("- {s}");
                if !body.lines().any(|l| l.trim() == line) {
                    body.push_str(&format!("\n{line}\n"));
                }
            }
            if body != old {
                m.insert("updated_at".into(), json!(now()));
                docs.insert(p, render(&m, &body)?);
                writes += 1;
            }
        }
    }
    reindex_topics(docs)?;
    Ok(writes)
}
pub fn finish(state: &mut State, v: &Value) -> Result<Value> {
    fields(
        v,
        &[
            "mode",
            "project",
            "session_id",
            "expected_workstream_sha256",
            "execution_group_id",
            "result",
        ],
    )?;
    let r = v.get("result").ok_or_else(|| invalid("缺少result"))?;
    fields(
        r,
        &[
            "status",
            "current",
            "decisions",
            "blockers",
            "verification",
            "next_steps",
            "project_topics",
            "process_id",
            "process_title",
            "process_learnings",
        ],
    )?;
    reject_secrets(&r.to_string())?;
    let log_only = matches!(text(v, "mode"), "log" | "log_only");
    if !text(v, "mode").is_empty() && !matches!(text(v, "mode"), "tracked" | "log" | "log_only") {
        return Err(invalid("mode无效"));
    }
    let mut title = "一次性知识更新".to_owned();
    let mut project = text(v, "project").to_owned();
    let mut session_id = None;
    if log_only {
        required(v, "project")?;
    } else {
        let id = required(v, "session_id")?;
        slug(id)?;
        let sp = find(&state.documents, "session_id", id)?
            .ok_or_else(|| Error::NotFound("Session不存在".into()))?;
        let (mut sm, session_body) = parse(&state.documents[&sp])?;
        project = string(&sm, "project").to_owned();
        if !text(v, "project").is_empty()
            && canonical(&state.documents, text(v, "project"))?
                != canonical(&state.documents, &project)?
        {
            return Err(conflict("finish项目与Session不一致"));
        }
        let ws = find(
            &state.documents,
            "workstream_id",
            string(&sm, "workstream_id"),
        )?
        .ok_or_else(|| invalid("Workstream不存在"))?;
        check_hash(
            &state.documents,
            &ws,
            required(v, "expected_workstream_sha256")?,
        )?;
        title = string(&sm, "title").to_owned();
        let status = if text(r, "status").is_empty() {
            "IDLE"
        } else {
            text(r, "status")
        };
        if !matches!(status, "IDLE" | "COMPLETED" | "WAITING_USER") {
            return Err(invalid("收尾状态无效"));
        }
        let (mut wm, workstream_body) = parse(&state.documents[&ws])?;
        let body = state_body(&title, goal(&session_body, &title), r)?;
        wm.insert("updated_at".into(), json!(now()));
        wm.insert("record_state".into(), json!("CONFIRMED"));
        state.documents.insert(
            ws,
            render(
                &wm,
                &state_body(
                    string(&wm, "title"),
                    goal(&workstream_body, string(&wm, "title")),
                    r,
                )?,
            )?,
        );
        sm.insert("status".into(), json!(status));
        sm.insert("updated_at".into(), json!(now()));
        sm.insert("record_state".into(), json!("CONFIRMED"));
        sm.remove("workstream_sha256_at_begin");
        state.documents.remove(&sp);
        let bucket = if status == "COMPLETED" {
            "completed"
        } else {
            "active"
        };
        state.documents.insert(
            format!("handoffs/sessions/{bucket}/{id}.md"),
            render(&sm, &body)?,
        );
        session_id = Some(id.to_owned());
    }
    if skip(&project) {
        return Ok(json!({"mode":"SKIP"}));
    }
    project = canonical(&state.documents, &project)?;
    let writes = memory_updates(&mut state.documents, &project, r)?;
    let month = Utc::now().format("%Y-%m").to_string();
    let path = format!("memory/logs/{month}.md");
    let summary = state_body(&title, &title, r)?;
    let entry = format!(
        "\n\n## {} · {}\n\n项目：{}；记忆文件更新：{}\n\n{}\n",
        now(),
        title,
        project,
        writes,
        summary
    );
    state
        .documents
        .entry(path)
        .or_insert_with(|| format!("# {month} 执行记录\n"))
        .push_str(&entry);
    Ok(
        json!({"mode":if log_only{"LOG_ONLY"}else{"TRACKED"},"session_id":session_id,"project":project,"memory_updates":writes,"memory_extraction":{"status":if writes==0{"no_change"}else{"updated"}},"execution_group_id":v.get("execution_group_id")}),
    )
}
pub fn abort(state: &mut State, v: &Value) -> Result<Value> {
    fields(v, &["session_id", "expected_sha256"])?;
    let id = required(v, "session_id")?;
    let path = find(&state.documents, "session_id", id)?
        .ok_or_else(|| Error::NotFound("Session不存在".into()))?;
    check_hash(&state.documents, &path, required(v, "expected_sha256")?)?;
    let (m, body) = parse(&state.documents[&path])?;
    if string(&m, "record_state") != "PROVISIONAL" || !body.contains("- 当前：等待执行。") {
        return Err(conflict("仅允许撤销仍为空的临时记录"));
    }
    let ws = string(&m, "workstream_id").to_owned();
    state.documents.remove(&path);
    let used = state
        .documents
        .values()
        .filter_map(|c| parse(c).ok())
        .any(|(m, _)| m.contains_key("session_id") && string(&m, "workstream_id") == ws);
    if !used && let Some(path) = find(&state.documents, "workstream_id", &ws)? {
        let (m, _) = parse(&state.documents[&path])?;
        if string(&m, "record_state") == "PROVISIONAL" {
            state.documents.remove(&path);
        }
    }
    Ok(json!({"aborted":true,"session_id":id}))
}
fn reindex_topics(docs: &mut Documents) -> Result<()> {
    let parents: Vec<_> = docs
        .keys()
        .filter(|p| path_kind(p).ok() == Some("project"))
        .cloned()
        .collect();
    for parent in parents {
        let (m, body) = parse(&docs[&parent])?;
        let project = string(&m, "project_id");
        let mut rows = Vec::new();
        for (p, c) in docs.iter() {
            if path_kind(p)? != "project_topic" {
                continue;
            }
            let (t, _) = parse(c)?;
            if string(&t, "project_id") == project && string(&t, "status") == "ACTIVE" {
                rows.push(format!(
                    "- [{}]({}/{}.md)：{}",
                    string(&t, "title"),
                    project,
                    string(&t, "topic_id"),
                    string(&t, "summary")
                ));
            }
        }
        let start = "<!-- PROJECT-TOPICS-START -->";
        let end = "<!-- PROJECT-TOPICS-END -->";
        let mut body = body;
        if let Some(i) = body.find(start) {
            let j = body[i..]
                .find(end)
                .ok_or_else(|| invalid("专题索引标记未闭合"))?
                + i
                + end.len();
            body.replace_range(i..j, "");
        }
        if !rows.is_empty() {
            body = format!(
                "{}\n\n{start}\n{}\n{end}\n",
                body.trim_end(),
                rows.join("\n")
            );
        }
        docs.insert(parent, render(&m, &body)?);
    }
    Ok(())
}
