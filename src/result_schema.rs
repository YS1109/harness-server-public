//! Shared stateless result validation. Authorization, CAS and accumulated budgets stay server-side.
use crate::{
    document::{reject_secrets, slug},
    error::{Error, Result},
};
use serde_json::Value;
use std::collections::BTreeSet;
fn fail(field: &str, expected: &str, actual: Value) -> Error {
    Error::Field {
        field: field.into(),
        expected: expected.into(),
        actual,
    }
}
fn object<'a>(
    v: &'a Value,
    field: &str,
    allowed: &[&str],
) -> Result<&'a serde_json::Map<String, Value>> {
    let o = v
        .as_object()
        .ok_or_else(|| fail(field, "object", Value::String(type_name(v).into())))?;
    for k in o.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(fail(
                &format!("{field}.{k}"),
                "supported field",
                Value::String("unknown field".into()),
            ));
        }
    }
    Ok(o)
}
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
fn text<'a>(v: &'a Value, field: &str, max: usize, required: bool) -> Result<&'a str> {
    let s = v
        .as_str()
        .ok_or_else(|| fail(field, "string", Value::String(type_name(v).into())))?;
    let n = s.chars().count();
    if (required && s.is_empty()) || n > max {
        return Err(fail(
            field,
            &format!("{}..={max} characters", usize::from(required)),
            Value::from(n),
        ));
    }
    Ok(s)
}
fn array<'a>(v: &'a Value, field: &str) -> Result<&'a Vec<Value>> {
    v.as_array()
        .ok_or_else(|| fail(field, "array", Value::String(type_name(v).into())))
}
fn slug_field(v: &Value, field: &str) -> Result<()> {
    let s = text(v, field, 128, true)?;
    slug(s).map_err(|_| {
        fail(
            field,
            "lowercase slug",
            Value::String("invalid slug".into()),
        )
    })
}
/// No network, state changes, body truncation, SHA freshness assumptions or extra CLI step.
pub fn validate(v: &Value) -> Result<()> {
    object(
        v,
        "result",
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
    // The rendered handoff budget depends on the existing title and goal.
    text(&v["current"], "result.current", usize::MAX, true)?;
    if let Some(status) = v.get("status") {
        let s = text(status, "result.status", 32, false)?;
        if !matches!(s, "" | "IDLE" | "COMPLETED" | "WAITING_USER") {
            return Err(fail(
                "result.status",
                "IDLE | COMPLETED | WAITING_USER",
                Value::String("unsupported status".into()),
            ));
        }
    }
    for key in ["decisions", "blockers", "verification", "next_steps"] {
        if let Some(value) = v.get(key) {
            let field = format!("result.{key}");
            let rows = array(value, &field)?;
            if rows.len() > 5 {
                return Err(fail(&field, "at most 5 items", Value::from(rows.len())));
            }
            for (i, item) in rows.iter().enumerate() {
                let f = format!("{field}[{i}]");
                let s = text(item, &f, 120, false)?;
                if s.contains('\n') {
                    return Err(fail(&f, "single line", Value::String("multiline".into())));
                }
            }
        }
    }
    if let Some(topics) = v.get("project_topics") {
        let mut seen = BTreeSet::new();
        for (i, t) in array(topics, "result.project_topics")?.iter().enumerate() {
            let p = format!("result.project_topics[{i}]");
            object(
                t,
                &p,
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
            slug_field(&t["topic_id"], &format!("{p}.topic_id"))?;
            if !seen.insert(t["topic_id"].as_str().unwrap()) {
                return Err(fail(
                    &format!("{p}.topic_id"),
                    "unique topic_id",
                    Value::String("duplicate".into()),
                ));
            }
            text(&t["title"], &format!("{p}.title"), usize::MAX, true)?;
            text(&t["summary"], &format!("{p}.summary"), 160, true)?;
            let body = text(&t["body"], &format!("{p}.body"), usize::MAX, true)?;
            let n = body.trim_end().trim_start_matches('\n').chars().count();
            if n > 6000 {
                return Err(fail(
                    &format!("{p}.body"),
                    "at most 6000 characters",
                    Value::from(n),
                ));
            }
            let f = format!("{p}.triggers");
            let triggers = array(&t["triggers"], &f)?;
            if !(1..=8).contains(&triggers.len()) {
                return Err(fail(&f, "1..=8 strings", Value::from(triggers.len())));
            }
            for (n, item) in triggers.iter().enumerate() {
                text(item, &format!("{f}[{n}]"), usize::MAX, false)?;
            }
            if let Some(a) = t.get("aliases") {
                for (n, item) in array(a, &format!("{p}.aliases"))?.iter().enumerate() {
                    text(item, &format!("{p}.aliases[{n}]"), usize::MAX, false)?;
                }
            }
            if let Some(h) = t.get("expected_sha256") {
                let s = text(h, &format!("{p}.expected_sha256"), 64, false)?;
                if !s.is_empty() && (s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit())) {
                    return Err(fail(
                        &format!("{p}.expected_sha256"),
                        "SHA-256 or empty for new topic",
                        Value::String("invalid hash".into()),
                    ));
                }
            }
            if let Some(date) = t.get("verified_at") {
                let s = text(date, &format!("{p}.verified_at"), 10, true)?;
                if s != "unknown"
                    && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                        .ok()
                        .is_none_or(|d| d > chrono::Utc::now().date_naive())
                {
                    return Err(fail(
                        &format!("{p}.verified_at"),
                        "YYYY-MM-DD not in future or unknown",
                        Value::String("invalid date".into()),
                    ));
                }
            }
            if let Some(e) = t.get("verification_evidence") {
                text(e, &format!("{p}.verification_evidence"), 240, true)?;
                if t.get("verified_at").is_none() {
                    return Err(fail(
                        &format!("{p}.verified_at"),
                        "required with verification_evidence",
                        Value::Null,
                    ));
                }
            }
        }
    }
    if let Some(id) = v.get("process_id") {
        slug_field(id, "result.process_id")?;
    }
    if let Some(title) = v.get("process_title") {
        text(title, "result.process_title", usize::MAX, false)?;
    }
    if let Some(rows) = v.get("process_learnings") {
        let rows = array(rows, "result.process_learnings")?;
        if !rows.is_empty() {
            slug_field(&v["process_id"], "result.process_id")?;
        }
        for (i, item) in rows.iter().enumerate() {
            let field = format!("result.process_learnings[{i}]");
            let s = text(item, &field, 240, true)?;
            if s.contains('\n') {
                return Err(fail(
                    &field,
                    "single line",
                    Value::String("multiline".into()),
                ));
            }
        }
    }
    reject_secrets(&v.to_string())
}
