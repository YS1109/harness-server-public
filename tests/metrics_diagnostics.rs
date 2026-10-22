#![cfg(feature = "service")]

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::IntoResponse,
};
use chrono::Utc;
use harness_server::{
    api,
    document::{self, Documents},
    error::{Error, ValidationReason},
    metrics::{Event, Ledger, RouteResult},
    store::Store,
};
use serde_json::{Value, json};
use std::{fs, path::Path};
use tower::ServiceExt;

const TOKEN: &str = "metrics-test-token-00000000000000000000";
const PRIVATE: &str = "private-content-marker";

fn project() -> String {
    document::render(
        json!({
            "schema_version": 1, "memory_type": "project", "project_id": "demo",
            "title": "Demo", "status": "ACTIVE", "verified_at": "unknown"
        })
        .as_object()
        .unwrap(),
        &format!("# \u{4e2d}\u{6587} \u{1f600} e\u{301}\n{PRIVATE}"),
    )
    .unwrap()
}

fn app(root: &Path) -> Router {
    let mut docs = Documents::new();
    docs.insert("memory/project/demo.md".into(), project());
    docs.insert(
        "memory/project/demo/topic.md".into(),
        document::render(
            json!({
                "schema_version": 1, "memory_type": "project_topic",
                "project_id": "demo", "topic_id": "topic", "title": PRIVATE,
                "summary": PRIVATE, "triggers": ["exact-topic"],
                "status": "ACTIVE", "verified_at": "unknown"
            })
            .as_object()
            .unwrap(),
            PRIVATE,
        )
        .unwrap(),
    );
    // An empty log is valid and distinguishes measured zero from an unknown count.
    docs.insert("memory/logs/2026-01.md".into(), String::new());
    let mut store = Store::open(root).unwrap();
    store.import(docs).unwrap();
    api::router(store, vec![TOKEN.into()]).unwrap()
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("Content-Type", "application/json");
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn events(root: &Path) -> Vec<Value> {
    let mut result = Vec::new();
    for entry in fs::read_dir(root.join(".runtime/metrics")).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        result.extend(
            text.lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap()),
        );
    }
    result
}

#[test]
fn validation_classification_uses_variants_and_preserves_http_codes() {
    let cases = [
        (
            Error::Field {
                field: PRIVATE.into(),
                expected: PRIVATE.into(),
                actual: json!({"sensitive": PRIVATE}),
            },
            Some(ValidationReason::FieldFormat),
            "validation",
            StatusCode::BAD_REQUEST,
        ),
        (
            Error::Budget {
                kind: PRIVATE.into(),
                path: format!("memory/project/{PRIVATE}.md"),
                actual: 2001,
                limit: 2000,
            },
            Some(ValidationReason::BodyBudget),
            "validation",
            StatusCode::BAD_REQUEST,
        ),
        (
            Error::Invalid(format!("{PRIVATE}: field budget identity conflict")),
            Some(ValidationReason::OtherValidation),
            "validation",
            StatusCode::BAD_REQUEST,
        ),
        (
            Error::Conflict(format!("{PRIVATE}: field budget identity conflict")),
            None,
            "conflict",
            StatusCode::CONFLICT,
        ),
        (
            Error::Forbidden(PRIVATE.into()),
            None,
            "forbidden",
            StatusCode::FORBIDDEN,
        ),
        (
            Error::NotFound(PRIVATE.into()),
            None,
            "not_found",
            StatusCode::NOT_FOUND,
        ),
        (
            Error::Storage(std::io::Error::other(PRIVATE)),
            None,
            "storage",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    ];
    for (error, reason, code, status) in cases {
        assert_eq!(error.validation_reason(), reason);
        assert_eq!(error.code(), code);
        let diagnostics = json!({
            "error_code": error.code(),
            "validation_reason": error.validation_reason()
        });
        assert!(!diagnostics.to_string().contains(PRIVATE));
        assert_eq!(error.into_response().status(), status);
    }
}

#[test]
fn mixed_legacy_and_new_events_keep_unknowns_and_fixed_distributions() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = Ledger::new(temp.path());
    let now = Utc::now();
    // This is the original event shape, without either new diagnostic field.
    let legacy = json!({
        "timestamp": now, "namespace": "shared", "command": "route", "project": "demo",
        "handler_ms": 1.0, "success": true, "retry": false, "error_code": null,
        "returned_characters": null, "handoff_characters": null
    });
    let mut rows = vec![legacy.clone()];
    let mut failure = legacy.clone();
    failure["success"] = json!(false);
    failure["error_code"] = json!("validation");
    rows.push(failure.clone());
    for reason in ["field_format", "body_budget", "other_validation"] {
        let mut row = failure.clone();
        row["validation_reason"] = json!(reason);
        // Even an inconsistent event cannot turn a failed route into a known outcome.
        row["route_result"] = json!("none");
        rows.push(row);
    }
    // Future enum labels cannot turn old readers into arbitrary-text collectors.
    failure["validation_reason"] = json!("future-reason");
    failure["route_result"] = json!("future-result");
    rows.push(failure);
    for outcome in ["auto", "candidates", "none", "future-result"] {
        let mut row = legacy.clone();
        row["route_result"] = json!(outcome);
        rows.push(row);
    }
    let directory = temp.path().join(".runtime/metrics");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join(format!("{}.jsonl", now.format("%Y-%m-%d"))),
        rows.iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let report = ledger.report(7, None, None, now);
    assert_eq!(report["coverage"]["malformed_events"], 0);
    assert_eq!(report["metrics"]["observed_calls"], 10);
    assert_eq!(report["metrics"]["failures"], 5);
    assert_eq!(report["metrics"]["error_codes"], json!({"validation": 5}));
    assert_eq!(
        report["metrics"]["validation_reasons"],
        json!({"field_format": 1, "body_budget": 1, "other_validation": 1, "unclassified": 2})
    );
    assert_eq!(
        report["metrics"]["route_results"],
        json!({"auto": 1, "candidates": 1, "none": 1, "unknown": 7})
    );
    assert!(report["metrics"]["returned_characters"].is_null());
    assert_eq!(report["metrics"]["returned_character_observed_calls"], 0);
    assert_eq!(
        report["command_metrics"]["route"]["route_results"],
        report["metrics"]["route_results"]
    );
    assert_eq!(
        report["namespaces"][0]["metrics"]["validation_reasons"],
        report["metrics"]["validation_reasons"]
    );
    let parsed: Event = serde_json::from_value(rows[5].clone()).unwrap();
    let serialized = serde_json::to_value(parsed).unwrap();
    assert_eq!(serialized["validation_reason"], "unclassified");
    assert_eq!(serialized["route_result"], "unknown");
}

#[tokio::test]
async fn get_counts_unicode_content_preserves_empty_zero_and_leaves_failures_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let service = app(temp.path());
    let (status, _) = call(
        &service,
        "GET",
        "/v1/documents?path=memory/project/missing-private-path.md",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, failed) = call(&service, "GET", "/v1/stats", Value::Null, None).await;
    assert!(failed["command_metrics"]["get"]["returned_characters"].is_null());
    assert_eq!(
        failed["command_metrics"]["get"]["returned_character_observed_calls"],
        0
    );
    let (status, doc) = call(
        &service,
        "GET",
        "/v1/documents?path=memory/project/demo.md",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let content = doc["content"].as_str().unwrap();
    assert_eq!(content, project());
    assert_ne!(content.len(), content.chars().count());
    // The HTTP get response remains unchanged; only its telemetry adds the count.
    assert!(doc.get("returned_characters").is_none());
    let (status, _) = call(
        &service,
        "GET",
        "/v1/documents?path=memory/logs/2026-01.md",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, report) = call(&service, "GET", "/v1/stats", Value::Null, None).await;
    let get = &report["command_metrics"]["get"];
    assert_eq!(get["observed_calls"], 3);
    assert_eq!(get["returned_characters"], content.chars().count() as u64);
    assert_eq!(get["returned_character_observed_calls"], 2);
    assert_eq!(get["route_results"], json!({}));
    assert_eq!(get["validation_reasons"], json!({}));
    let events = events(temp.path());
    assert!(events[0]["returned_characters"].is_null());
    assert_eq!(events[2]["returned_characters"], 0);
    assert!(!json!(events).to_string().contains("missing-private-path"));
}

#[tokio::test]
async fn route_results_and_start_character_budgets_follow_actual_responses() {
    let temp = tempfile::tempdir().unwrap();
    let service = app(temp.path());
    let cases = [
        (json!({"scenario": "exact-topic", "content": true}), "auto"),
        (json!({"scenario": "unmatched-private-query"}), "candidates"),
        (json!({"scenario": ""}), "none"),
        (json!({"topic": "topic"}), "auto"),
        // An exhausted candidate page actually returns no selection or candidates.
        (json!({"candidate_offset": 10}), "none"),
    ];
    let mut returned = 0;
    for (mut request, expected) in cases {
        request["project"] = json!("demo");
        let (status, response) = call(&service, "POST", "/v1/route", request, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::to_value(RouteResult::from_response(&response)).unwrap(),
            expected
        );
        if let Some(content) = response["content"].as_array() {
            returned = content
                .iter()
                .map(|doc| doc["content"].as_str().unwrap().chars().count() as u64)
                .sum::<u64>();
            assert_eq!(response["returned_characters"], returned);
        }
    }
    let (status, _) = call(
        &service,
        "POST",
        "/v1/route",
        json!({"project": "demo", "candidate_offset": -1, "scenario": PRIVATE}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, started) = call(
        &service,
        "POST",
        "/v1/start",
        json!({
            "project": "demo", "agent": "test", "native_session_id": "characters",
            "workstream_id": "metrics", "title": "\u{4efb}\u{52a1}\u{1f600}",
            "scenario": "exact-topic", "content": true
        }),
        Some("start-characters"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let memory_characters = started["memories"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|doc| doc["content"].as_str().unwrap().chars().count() as u64)
        .sum::<u64>();
    let handoff_characters = ["session", "workstream"]
        .iter()
        .map(|key| {
            started["handoff"][key]["content"]
                .as_str()
                .unwrap()
                .chars()
                .count() as u64
        })
        .sum::<u64>();
    let (_, report) = call(&service, "GET", "/v1/stats", Value::Null, None).await;
    let route = &report["command_metrics"]["route"];
    assert_eq!(
        route["route_results"],
        json!({"auto": 2, "candidates": 1, "none": 2, "unknown": 1})
    );
    assert_eq!(route["returned_characters"], returned);
    assert_eq!(route["returned_character_observed_calls"], 1);
    let start = &report["command_metrics"]["start"];
    assert_eq!(start["returned_characters"], memory_characters);
    assert_eq!(start["handoff_characters"], handoff_characters);
    assert_eq!(start["route_results"], json!({"auto":1}));
    let raw = json!(events(temp.path())).to_string();
    for sensitive in [
        PRIVATE,
        "unmatched-private-query",
        "exact-topic",
        "memory/project/",
    ] {
        assert!(!raw.contains(sensitive), "leaked {sensitive}");
    }
    assert_eq!(RouteResult::from_response(&json!({})), RouteResult::Unknown);
}

#[tokio::test]
async fn validation_events_are_content_free_and_conflicts_are_not_guessed() {
    let temp = tempfile::tempdir().unwrap();
    let service = app(temp.path());
    let (status, field) = call(
        &service,
        "POST",
        "/v1/finish",
        json!({"mode": "log", "project": "demo", "result": {"current": PRIVATE, PRIVATE: PRIVATE}}),
        Some("invalid-field"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(field["error"]["code"], "validation");
    assert!(
        field["error"]["details"]["field"]
            .as_str()
            .unwrap()
            .contains(PRIVATE)
    );
    let mut oversized = project();
    oversized.push_str(&"x".repeat(2100));
    let (status, budget) = call(
        &service,
        "PUT",
        "/v1/documents",
        json!({
            "path": "memory/project/demo.md", "content": oversized,
            "expected_sha256": document::sha(&project())
        }),
        Some("invalid-budget"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(budget["error"]["code"], "validation");
    assert_eq!(budget["error"]["details"]["path"], "memory/project/demo.md");
    assert_eq!(budget["error"]["details"]["limit"], 2000);
    let (status, _) = call(
        &service,
        "POST",
        "/v1/route",
        json!({"project": "demo", PRIVATE: PRIVATE}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, conflict) = call(
        &service,
        "PUT",
        "/v1/documents",
        json!({"path": "memory/project/demo.md", "content": project(), "expected_sha256": PRIVATE}),
        Some("conflict"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["error"]["code"], "conflict");
    let (_, report) = call(&service, "GET", "/v1/stats", Value::Null, None).await;
    assert_eq!(
        report["metrics"]["validation_reasons"],
        json!({"field_format": 1, "body_budget": 1, "other_validation": 1})
    );
    assert_eq!(
        report["metrics"]["error_codes"],
        json!({"validation": 3, "conflict": 1})
    );
    let rows = events(temp.path());
    assert!(rows[3]["validation_reason"].is_null());
    assert!(rows.iter().all(|e| e["returned_characters"].is_null()));
    let raw = json!(rows).to_string();
    for sensitive in [
        PRIVATE,
        TOKEN,
        "memory/project/demo.md",
        field["error"]["message"].as_str().unwrap(),
        budget["error"]["message"].as_str().unwrap(),
    ] {
        assert!(!raw.contains(sensitive), "leaked {sensitive}");
    }
}
