use flate2::read::GzDecoder;
use fs2::FileExt;
use harness_server::{
    backup::{self, Config, OssConfig},
    document::{Documents, render},
    engine, execution,
    store::Store,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

fn project(body: &str) -> String {
    render(json!({"schema_version":1,"memory_type":"project","project_id":"demo","title":"Demo","status":"ACTIVE","verified_at":"unknown"}).as_object().unwrap(), body).unwrap()
}

fn setup() -> (TempDir, Config, Store) {
    let temp = TempDir::new().unwrap();
    let config = Config {
        schema_version: 1,
        data_dir: temp.path().join("data"),
        output_dir: temp.path().join("backups"),
        oss: None,
    };
    let mut store = Store::open(&config.data_dir).unwrap();
    let mut docs = Documents::new();
    docs.insert("memory/project/demo.md".into(), project("# Demo\n"));
    docs.insert("memory/process/personal-coding-preferences.md".into(), render(json!({"schema_version":1,"memory_type":"process","process_id":"personal-coding-preferences","title":"Preferences","status":"ACTIVE","verified_at":"unknown","scope":"personal"}).as_object().unwrap(), "# Preferences\n\n- Only run relevant tests.\n").unwrap());
    store.import(docs).unwrap();
    let request =
        json!({"project":"demo","agent":"pi","native_session_id":"backup-task","title":"Task"});
    store
        .transact("saved-start", &request, |s| engine::start(s, &request))
        .unwrap();
    (temp, config, store)
}

fn manifest(result: &Value) -> PathBuf {
    PathBuf::from(result["manifest"].as_str().unwrap())
}

fn restore(config: &Config, result: &Value, directory: &Path) -> Store {
    fs::create_dir(directory).unwrap();
    let archive = config
        .output_dir
        .join(result["backup"]["archive"].as_str().unwrap());
    tar::Archive::new(GzDecoder::new(fs::File::open(archive).unwrap()))
        .unpack(directory)
        .unwrap();
    Store::open(directory).unwrap()
}

#[test]
fn online_backup_restores_documents_history_and_idempotency_while_writer_stays_open() {
    let (temp, config, mut store) = setup();
    let head = fs::read(config.data_dir.join("HEAD")).unwrap();
    fs::write(
        config.data_dir.join(".env"),
        "DO_NOT_INCLUDE_CREDENTIAL_FILE",
    )
    .unwrap();
    let result = backup::create(&config, true).unwrap();
    assert_eq!(
        result["backup"]["revision"],
        store.revision.as_deref().unwrap()
    );
    assert_eq!(result["backup"]["documents"], store.state.documents.len());
    assert_eq!(result["backup"]["receipts"], 1);
    assert_eq!(result["backup"]["revisions"], 2);
    assert_eq!(fs::read(config.data_dir.join("HEAD")).unwrap(), head);
    assert!(backup::verify(&config, &manifest(&result)).is_ok());
    let restored = restore(&config, &result, &temp.path().join("restored"));
    assert_eq!(restored.state.documents, store.state.documents);
    assert!(!temp.path().join("restored/.env").exists());
    drop(restored);
    let mut restored = Store::open(temp.path().join("restored")).unwrap();
    let before = restored.revision.clone();
    let request =
        json!({"project":"demo","agent":"pi","native_session_id":"backup-task","title":"Task"});
    let replay = restored
        .transact("saved-start", &request, |_| panic!("receipt was lost"))
        .unwrap();
    assert_eq!(replay["idempotent"], true);
    assert_eq!(restored.revision, before);
    store
        .transact("after-backup", &json!({"new":true}), |s| {
            s.documents
                .insert("memory/project/demo.md".into(), project("# New version\n"));
            Ok(json!({"written":true}))
        })
        .unwrap();
    assert_ne!(store.revision, restored.revision);
    assert_eq!(
        restored.state.documents["memory/project/demo.md"],
        project("# Demo\n")
    );
}

#[test]
fn incomplete_revisions_are_reported_and_current_corruption_is_not_skipped() {
    let (_temp, config, _store) = setup();
    let id = uuid::Uuid::new_v4().to_string();
    fs::create_dir(config.data_dir.join("revisions").join(&id)).unwrap();
    let result = backup::create(&config, true).unwrap();
    assert_eq!(
        result["backup"]["excluded_incomplete_revisions"],
        json!([id])
    );
    let head = fs::read_to_string(config.data_dir.join("HEAD")).unwrap();
    fs::write(
        config
            .data_dir
            .join("revisions")
            .join(head.trim())
            .join(".meta.json"),
        b"partial-json",
    )
    .unwrap();
    assert!(backup::create(&config, true).is_err());
    assert_eq!(
        fs::read_dir(&config.output_dir)
            .unwrap()
            .filter_map(|p| p.ok())
            .filter(|p| p.file_name().to_string_lossy().ends_with(".tar.gz"))
            .count(),
        1
    );
}

#[test]
fn nested_output_and_overlapping_backup_jobs_are_rejected() {
    let (_temp, config, _store) = setup();
    let mut nested = config.clone();
    nested.output_dir = config.data_dir.join("backups");
    assert!(backup::create(&nested, true).is_err());
    assert!(!nested.output_dir.exists());
    fs::create_dir(&config.output_dir).unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(config.output_dir.join(".backup.lock"))
        .unwrap();
    lock.try_lock_exclusive().unwrap();
    assert!(backup::create(&config, true).is_err());
}

#[test]
fn checksum_verify_and_upload_do_not_require_the_original_store() {
    let (temp, config, store) = setup();
    let result = backup::create(&config, true).unwrap();
    drop(store);
    fs::rename(&config.data_dir, temp.path().join("offline-data")).unwrap();
    assert!(backup::verify(&config, &manifest(&result)).is_ok());
    assert!(
        backup::upload(&config, &manifest(&result))
            .unwrap_err()
            .to_string()
            .contains("未启用")
    );
}

#[test]
fn tampering_fails_verification_and_config_cannot_embed_credentials() {
    let (temp, config, _store) = setup();
    let result = backup::create(&config, true).unwrap();
    fs::write(
        config
            .output_dir
            .join(result["backup"]["archive"].as_str().unwrap()),
        b"corrupt",
    )
    .unwrap();
    assert!(backup::verify(&config, &manifest(&result)).is_err());
    let path = temp.path().join("config.json");
    fs::write(&path, json!({"schema_version":1,"data_dir":config.data_dir,"output_dir":config.output_dir,"access_key":"not-a-real-credential"}).to_string()).unwrap();
    let error = Config::load(&path).err().unwrap().to_string();
    assert!(!error.contains("not-a-real-credential"));
    assert!(error.contains("不支持"));
}

#[test]
fn cli_creates_and_verifies_backup_without_acquiring_the_live_writer_lock() {
    let (temp, config, _store) = setup();
    let path = temp.path().join("backup-config.json");
    fs::write(&path, json!({"schema_version":1,"data_dir":config.data_dir,"output_dir":config.output_dir,"oss":null}).to_string()).unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_harness-backup"))
        .arg("--config")
        .arg(&path)
        .arg("create")
        .arg("--local-only")
        .output()
        .unwrap();
    assert!(result.status.success());
    let result: Value = serde_json::from_slice(&result.stdout).unwrap();
    let verify = std::process::Command::new(env!("CARGO_BIN_EXE_harness-backup"))
        .arg("--config")
        .arg(&path)
        .arg("verify")
        .arg("--manifest")
        .arg(manifest(&result))
        .output()
        .unwrap();
    assert!(verify.status.success());
}

#[test]
fn backup_restores_execution_tasks_attempts_and_dispatch_context() {
    let (temp, config, mut store) = setup();
    let request = json!({"task_id":"qq-task","conversation_id":"qq-chat","project":"demo","source_key":"message-one","title":"Remote task","goal":"Persist remote progress"});
    let task = store
        .transact("task-create", &request, |s| execution::create(s, &request))
        .unwrap();
    let request =
        json!({"agent":"pi","native_session_id":"remote-agent","expected_sha256":task["sha256"]});
    store
        .transact("task-begin", &request, |s| {
            execution::begin_attempt(s, "qq-task", &request)
        })
        .unwrap();
    let request =
        json!({"title":"QQ dispatcher","goal":"Resume orchestration","task_ids":["qq-task"]});
    store
        .transact("context", &request, |s| {
            execution::put_context(s, "qq-chat", &request)
        })
        .unwrap();
    let result = backup::create(&config, true).unwrap();
    let restored = restore(&config, &result, &temp.path().join("restored"));
    assert_eq!(restored.state.documents, store.state.documents);
    assert_eq!(
        execution::get(&restored.state.documents, "qq-task").unwrap()["task"]["status"],
        "DISPATCHING"
    );
    assert!(
        restored
            .state
            .documents
            .keys()
            .any(|p| p.starts_with("executions/attempts/"))
    );
    assert_eq!(
        execution::get_context(&restored.state.documents, "qq-chat").unwrap()["context"]["task_ids"],
        json!(["qq-task"])
    );
}

#[cfg(unix)]
fn oss_fixture(config: &mut Config, behavior: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let binary = config.output_dir.parent().unwrap().join("mock-ossutil");
    fs::write(
        &binary,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$0.calls\"\n{behavior}\n"),
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    config.oss = Some(OssConfig {
        enabled: true,
        bucket: "private-test-bucket".into(),
        endpoint: "https://oss-cn-hangzhou.aliyuncs.com".into(),
        prefix: "harness/backups".into(),
        ossutil: binary.clone(),
        credentials_file: None,
    });
    binary
}

#[cfg(unix)]
#[test]
fn oss_uploads_archive_and_manifest_and_redacts_errors_then_allows_retry() {
    let (_temp, mut config, _store) = setup();
    let binary = oss_fixture(
        &mut config,
        "for arg in \"$@\"; do case \"$arg\" in *.json) printf 'sensitive-tool-output' >&2; exit 9;; esac; done\nexit 0",
    );
    let error = backup::create(&config, false).unwrap_err().to_string();
    assert!(error.contains("本地备份已完成"));
    assert!(!error.contains("sensitive-tool-output"));
    let path = fs::read_dir(&config.output_dir)
        .unwrap()
        .filter_map(|p| p.ok())
        .map(|p| p.path())
        .find(|p| p.extension().is_some_and(|s| s == "json"))
        .unwrap();
    assert!(!path.with_extension("uploaded.json").exists());
    let calls = fs::read_to_string(PathBuf::from(format!("{}.calls", binary.display()))).unwrap();
    assert!(calls.contains("--endpoint\nhttps://oss-cn-hangzhou.aliyuncs.com\ncp\n"));
    assert_eq!(calls.matches("--force").count(), 2);
    oss_fixture(&mut config, "exit 0");
    let result = backup::upload(&config, &path).unwrap();
    assert_eq!(result["objects"].as_array().unwrap().len(), 2);
    assert!(path.with_extension("uploaded.json").exists());
    assert!(
        result["verification"]
            .as_str()
            .unwrap()
            .contains("尚未验证")
    );
}

#[cfg(unix)]
#[test]
fn symlinks_are_rejected_and_local_only_does_not_need_oss_credentials() {
    use std::os::unix::{fs::PermissionsExt, fs::symlink};
    let (_temp, mut config, _store) = setup();
    oss_fixture(&mut config, "exit 0");
    config.oss.as_mut().unwrap().credentials_file = Some(
        config
            .output_dir
            .parent()
            .unwrap()
            .join("missing.credentials"),
    );
    let result = backup::create(&config, true).unwrap();
    assert!(backup::upload(&config, &manifest(&result)).is_err());
    assert_eq!(
        fs::metadata(manifest(&result))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let head = fs::read_to_string(config.data_dir.join("HEAD")).unwrap();
    let path = config
        .data_dir
        .join("revisions")
        .join(head.trim())
        .join("memory/project/demo.md");
    fs::remove_file(&path).unwrap();
    symlink("/etc/passwd", &path).unwrap();
    assert!(backup::create(&config, true).is_err());
}
