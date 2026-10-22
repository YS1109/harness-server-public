use std::{
    env, fs,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn git(args: &[&str]) -> Option<String> {
    if !Path::new(".git").exists() {
        return None;
    }
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn main() {
    for key in [
        "HARNESS_BUILD_COMMIT",
        "HARNESS_BUILD_DIRTY",
        "SOURCE_DATE_EPOCH",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    for path in [
        "build.rs",
        "Cargo.toml",
        "Cargo.lock",
        "src",
        "docs",
        "tools",
        ".github",
        "README.md",
        "AGENTS.md",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    // Also support linked worktrees. Archive/Docker builds can supply explicit
    // provenance; missing Git metadata must never imply a clean checkout.
    for name in ["HEAD", "index"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            println!("cargo:rerun-if-changed={path}");
            if name == "HEAD" {
                if let Ok(head) = fs::read_to_string(&path) {
                    if let Some(reference) = head.trim().strip_prefix("ref: ") {
                        if let Some(path) = git(&["rev-parse", "--git-path", reference]) {
                            println!("cargo:rerun-if-changed={path}");
                        }
                    }
                }
            }
        }
    }
    let commit = env::var("HARNESS_BUILD_COMMIT")
        .ok()
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .filter(|s| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".into());
    let dirty = match env::var("HARNESS_BUILD_DIRTY").ok().as_deref() {
        Some("true") => "true",
        Some("false") => "false",
        Some(_) => "unknown",
        None => match git(&["status", "--porcelain", "--untracked-files=normal"]) {
            Some(s) if s.is_empty() => "false",
            Some(_) => "true",
            None => "unknown",
        },
    };
    let epoch = match env::var("SOURCE_DATE_EPOCH") {
        Ok(s) => s
            .parse::<u64>()
            .expect("SOURCE_DATE_EPOCH must be epoch seconds"),
        Err(_) => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };
    for (key, value) in [
        ("HARNESS_BUILD_COMMIT", commit),
        ("HARNESS_BUILD_DIRTY", dirty.to_owned()),
        ("HARNESS_BUILD_EPOCH", epoch.to_string()),
        ("HARNESS_BUILD_TARGET", env::var("TARGET").unwrap()),
    ] {
        println!("cargo:rustc-env={key}={value}");
    }
}
