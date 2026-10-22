/// Compile-time provenance only; no credentials, machine paths or remote URLs.
pub fn metadata() -> serde_json::Value {
    let commit = env!("HARNESS_BUILD_COMMIT");
    let epoch = env!("HARNESS_BUILD_EPOCH").parse::<i64>().ok();
    serde_json::json!({
        "commit": if commit == "unknown" { None } else { Some(commit) },
        "dirty": match env!("HARNESS_BUILD_DIRTY") {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        "built_at": epoch.and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            .map(|t| t.to_rfc3339()),
        "target": env!("HARNESS_BUILD_TARGET"),
    })
}
