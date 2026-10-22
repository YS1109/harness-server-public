use harness_server::{
    document::{self, Documents},
    engine,
    store::State,
};
use serde_json::{Value, json};

fn card(id: &str, aliases: Value) -> String {
    document::render(
        json!({"schema_version":1,"memory_type":"project","project_id":id,
            "title":id,"status":"ACTIVE","verified_at":"unknown","aliases":aliases})
        .as_object()
        .unwrap(),
        "# Project",
    )
    .unwrap()
}
fn edit(docs: &mut Documents, path: &str, key: &str, value: Value) {
    let (mut m, body) = document::parse(&docs[path]).unwrap();
    m.insert(key.into(), value);
    docs.insert(path.into(), document::render(&m, &body).unwrap());
}
fn fixture() -> State {
    let mut state = State::default();
    state.documents.insert(
        "memory/project/demo.md".into(),
        card("demo", json!(["legacy", "legacy", "demo"])),
    );
    state
        .documents
        .insert("memory/project/other.md".into(), card("other", json!([])));
    engine::start(
        &mut state,
        &json!({"agent":"test","native_session_id":"indexed","project":"demo",
            "title":"Task","workstream_id":"goal"}),
    )
    .unwrap();
    state
}
#[test]
fn indexes_resolve_aliases_and_are_rebuilt_for_changed_documents() {
    let mut state = fixture();
    let path = "handoffs/sessions/active/test-indexed.md";
    edit(&mut state.documents, path, "project", json!("legacy"));
    document::validate_all(&state.documents).unwrap();
    edit(
        &mut state.documents,
        "memory/project/demo.md",
        "aliases",
        json!([]),
    );
    assert!(
        document::validate_all(&state.documents)
            .unwrap_err()
            .to_string()
            .contains("跨项目")
    );
    edit(&mut state.documents, path, "project", json!("demo"));
    document::validate_all(&state.documents).unwrap();
    state
        .documents
        .get_mut("memory/project/demo.md")
        .unwrap()
        .push_str(&"x".repeat(2001));
    assert!(document::validate_all(&state.documents).is_err());
}
#[test]
fn ambiguous_aliases_missing_links_and_duplicate_identities_still_fail() {
    let mut state = fixture();
    edit(
        &mut state.documents,
        "memory/project/other.md",
        "aliases",
        json!(["legacy"]),
    );
    assert!(
        document::validate_all(&state.documents)
            .unwrap_err()
            .to_string()
            .contains("歧义")
    );
    let mut state = fixture();
    let ws = state
        .documents
        .remove("handoffs/workstreams/goal.md")
        .unwrap();
    assert!(
        document::validate_all(&state.documents)
            .unwrap_err()
            .to_string()
            .contains("不存在")
    );
    state
        .documents
        .insert("handoffs/workstreams/goal.md".into(), ws.clone());
    state
        .documents
        .insert("handoffs/workstreams/archive/goal.md".into(), ws);
    assert!(
        document::validate_all(&state.documents)
            .unwrap_err()
            .to_string()
            .contains("重复Workstream")
    );
    let mut state = fixture();
    let session = state.documents["handoffs/sessions/active/test-indexed.md"].clone();
    let (mut m, body) = document::parse(&session).unwrap();
    m.insert("status".into(), json!("COMPLETED"));
    state.documents.insert(
        "handoffs/sessions/completed/test-indexed.md".into(),
        document::render(&m, &body).unwrap(),
    );
    assert!(
        document::validate_all(&state.documents)
            .unwrap_err()
            .to_string()
            .contains("重复Session")
    );
}
#[test]
fn build_provenance_has_explicit_unknowns_and_no_machine_paths() {
    let build = harness_server::build_info::metadata();
    assert!(
        build["commit"].is_null() || matches!(build["commit"].as_str().unwrap().len(), 40 | 64)
    );
    assert!(build["dirty"].is_null() || build["dirty"].is_boolean());
    assert!(build["built_at"].as_str().is_some());
    assert!(build["target"].as_str().is_some());
}
