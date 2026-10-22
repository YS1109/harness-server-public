//! Explicit, read-only routing evaluation. Reports exclude raw queries and memory bodies.
use crate::{
    document::{self, Documents},
    engine,
    error::{Result, invalid},
    namespace,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, io::Write, path::Path};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub version: String,
    pub cases: Vec<Case>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub project: String,
    pub query: String,
    // Required even for negatives: null explicitly means no automatic selection.
    pub expected_topic: Value,
    #[serde(default)]
    pub expected_namespace: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub required_topics: Vec<String>,
}
pub fn parse(value: &Value) -> Result<Suite> {
    if value["cases"]
        .as_array()
        .is_some_and(|cases| cases.iter().any(|c| c.get("expected_topic").is_none()))
    {
        return Err(invalid("每个用例必须显式提供expected_topic，负例使用null"));
    }
    let suite: Suite = serde_json::from_value(value.clone()).map_err(|_| {
        invalid("suite必须含version及cases，每例需id/project/query/expected_topic；不接受未知字段")
    })?;
    validate(&suite)?;
    Ok(suite)
}
pub fn validate(suite: &Suite) -> Result<()> {
    if suite.version.is_empty()
        || suite.version.chars().count() > 80
        || suite.cases.is_empty()
        || suite.cases.len() > 200
    {
        return Err(invalid("suite version必须1–80字，cases必须1–200条"));
    }
    let mut ids = BTreeSet::new();
    for c in &suite.cases {
        document::slug(&c.id)?;
        document::slug(&c.project)?;
        if c.project == "daily" {
            return Err(invalid("daily不参加评测"));
        }
        if !ids.insert(&c.id) || c.query.chars().count() > 2000 {
            return Err(invalid("用例ID重复或query超过2000字"));
        }
        if !c.expected_topic.is_null() {
            document::slug(
                c.expected_topic
                    .as_str()
                    .ok_or_else(|| invalid("expected_topic必须是专题ID或null"))?,
            )?;
        }
        if let Some(ns) = &c.expected_namespace {
            document::slug(ns)?;
        }
        if let Some(id) = &c.topic {
            document::slug(id)?;
        }
        if c.required_topics.len() > 16 {
            return Err(invalid("required_topics最多16个"));
        }
        for id in &c.required_topics {
            document::slug(id)?;
        }
        document::reject_secrets(&c.query)?;
    }
    document::reject_secrets(&suite.version)
}
pub fn evaluate(
    docs: &Documents,
    scopes: &[String],
    namespace: &str,
    value: &Value,
) -> Result<Value> {
    let suite = parse(value)?;
    let mut related = Documents::new();
    for ns in scopes {
        for (p, c) in namespace::view(docs, ns, false) {
            if namespace::shared_memory(&p) {
                related.insert(namespace::physical(ns, &p), c);
            }
        }
    }
    let mut positive = 0;
    let mut correct = 0;
    let mut missed = 0;
    let mut negatives = 0;
    let mut false_selections = 0;
    let mut unavailable = 0;
    let mut rows = Vec::new();
    for c in &suite.cases {
        let expected = c.expected_topic.as_str();
        let available_topic = |id: &str| {
            scopes
                .iter()
                .filter(|ns| c.expected_namespace.as_ref().is_none_or(|e| e == *ns))
                .any(|ns| {
                    let view = namespace::view(docs, ns, false);
                    document::canonical(&view, &c.project)
                        .ok()
                        .is_some_and(|project| {
                            view.get(&format!("memory/project/{project}/{id}.md"))
                                .and_then(|text| document::parse(text).ok())
                                .is_some_and(|(m, _)| document::string(&m, "status") == "ACTIVE")
                        })
                })
        };
        let missing = expected
            .into_iter()
            .chain(c.required_topics.iter().map(String::as_str))
            .any(|id| !available_topic(id));
        let request = json!({"project":c.project,"scenario":c.query,"topic":c.topic.as_deref().unwrap_or(""),"topic_namespace":c.expected_namespace.as_deref().unwrap_or("")});
        let routed = if missing {
            None
        } else {
            engine::route_scoped(docs, scopes, &request).ok()
        };
        let available = routed
            .as_ref()
            .is_some_and(|r| !r["project_memory"].is_null());
        if !available {
            unavailable += 1;
            rows.push(json!({"id":c.id,"project":c.project,"status":"unavailable","reason":if missing{"missing_topic_dependency"}else{"missing_project_or_route_error"}}));
            continue;
        }
        let r = routed.unwrap();
        let selected = r["memory_sources"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| {
                s["path"]
                    .as_str()
                    .is_some_and(|p| document::path_kind(p).ok() == Some("project_topic"))
            });
        let actual = selected
            .and_then(|s| s["path"].as_str())
            .and_then(|p| p.rsplit('/').next())
            .and_then(|p| p.strip_suffix(".md"));
        let actual_namespace = selected.and_then(|s| s["namespace"].as_str());
        let ok = if let Some(expected) = expected {
            positive += 1;
            if actual.is_none() {
                missed += 1;
            }
            let ok = actual == Some(expected)
                && c.expected_namespace
                    .as_deref()
                    .is_none_or(|ns| actual_namespace == Some(ns));
            correct += usize::from(ok);
            ok
        } else {
            negatives += 1;
            false_selections += usize::from(actual.is_some());
            actual.is_none()
        };
        rows.push(json!({"id":c.id,"project":c.project,"status":if ok{"correct"}else{"incorrect"},"expected_topic":c.expected_topic,"expected_namespace":c.expected_namespace,"actual_topic":actual,"actual_namespace":actual_namespace}));
    }
    Ok(
        json!({"evaluated_at":chrono::Utc::now(),"engine_version":env!("CARGO_PKG_VERSION"),"suite_version":suite.version,"suite_fingerprint":document::sha(&value.to_string()),"snapshot_fingerprint":document::sha(&serde_json::to_string(&related).expect("serializable memory snapshot")),"namespace":namespace,"read_namespaces":scopes,"summary":{"total":suite.cases.len(),"unavailable":unavailable,"positive_evaluated":positive,"positive_correct":correct,"positive_accuracy":if positive==0{None}else{Some(correct as f64/positive as f64)},"missed":missed,"negative_evaluated":negatives,"false_selections":false_selections},"cases":rows,"scope":"仅评估此用例集在所记录记忆快照上的检索正确性；不是任务净收益；最近评测不保证代表当前记忆。"}),
    )
}
fn write_atomic(path: &Path, value: &Value) -> std::io::Result<()> {
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".eval-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
pub fn save(root: &Path, namespace: &str, report: &mut Value) {
    let dir = root.join(".runtime/evaluations");
    report["report_saved"] = json!(true);
    if write_atomic(&dir.join(format!("latest-{namespace}.json")), report)
        .and_then(|_| write_atomic(&dir.join("latest.json"), report))
        .is_err()
    {
        report["report_saved"] = json!(false);
    }
}
pub fn latest(root: &Path, ns: Option<&str>) -> Value {
    if ns.is_some_and(|s| document::slug(s).is_err()) {
        return Value::Null;
    }
    let path = root
        .join(".runtime/evaluations")
        .join(ns.map_or_else(|| "latest.json".into(), |s| format!("latest-{s}.json")));
    match fs::File::open(path) {
        Ok(file) => {
            use std::io::Read;
            let mut bytes = Vec::new();
            if file.take(1024 * 1024 + 1).read_to_end(&mut bytes).is_err()
                || bytes.len() > 1024 * 1024
            {
                return json!({"status":"unavailable","reason":"report_unreadable"});
            }
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!({"status":"unavailable","reason":"report_corrupt"}))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(_) => json!({"status":"unavailable","reason":"report_unreadable"}),
    }
}
