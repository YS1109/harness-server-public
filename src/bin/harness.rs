use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

#[derive(Parser, Clone)]
#[command(
    version,
    about = "Remote Harness client; stdout is JSON, errors never imply a successful save"
)]
struct Args {
    #[arg(long, env = "HARNESS_URL", global = true)]
    url: Option<String>,
    #[arg(long, env = "HARNESS_CLIENT_CONFIG", global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    idempotency_key: Option<String>,
    #[arg(long, env = "HARNESS_AGENT_ID", global = true)]
    agent_id: Option<String>,
    #[arg(long, env = "HARNESS_NAMESPACE", global = true)]
    namespace: Option<String>,
    #[arg(long, env = "HARNESS_READ_NAMESPACES", global = true)]
    read_namespaces: Option<String>,
    /// Use the one-request scoped endpoint only after connect/setup confirms support.
    #[arg(long, global=true, value_parser=["v1","scoped-v2"])]
    write_protocol: Option<String>,
    /// Select shared knowledge or the already connected device space.
    #[arg(long,global=true,value_parser=["shared","device"])]
    scope: Option<String>,
    /// Print CLI wall-clock milliseconds and actual HTTP request count to stderr.
    #[arg(long, global = true)]
    timings: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand, Clone)]
enum Command {
    /// Read the embedded Agent rules locally; no credential or HTTP call is needed.
    #[command(alias = "rules")]
    Instructions {
        /// Print the small entry suitable for an Agent's global prompt.
        #[arg(long, conflicts_with = "json")]
        bootstrap: bool,
        /// Return the rules and local scope as structured JSON.
        #[arg(long)]
        json: bool,
    },
    /// Persist a stable local identity; no service registration or credential is created.
    Init,
    /// Show the local identity without reading the credential or contacting the service.
    Identity,
    /// Discover existing server-created spaces and current permissions.
    Namespaces,
    /// Validate and save selections from the server catalog. Does not create a space.
    Connect,
    /// Verify the server, merge configuration and optional project binding atomically.
    Setup {
        #[arg(long)]
        token_env: Option<String>,
        #[arg(long, requires = "project_root")]
        project: Option<String>,
        #[arg(long, requires = "project")]
        project_root: Option<PathBuf>,
    },
    /// Read-only effective configuration and server authorization diagnosis.
    Doctor,
    /// Merge one local project mapping without editing JSON.
    Bind {
        project: String,
        root: PathBuf,
    },
    /// Explicit offline-suite evaluation against a read-only server memory snapshot.
    EvalRouting {
        #[arg(long)]
        suite: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Route {
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = "")]
        scenario: String,
        #[arg(long)]
        topic: Option<String>,
        #[arg(long)]
        topic_namespace: Option<String>,
        #[arg(long)]
        content: bool,
        #[arg(long)]
        candidates_only: bool,
        #[arg(long, default_value_t = 0)]
        candidate_offset: usize,
    },
    Start {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        native_session_id: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        workstream_id: Option<String>,
        #[arg(long, default_value = "")]
        scenario: String,
        #[arg(long)]
        content: bool,
    },
    Finish {
        #[arg(long, default_value = "tracked")]
        mode: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        session_id: Option<String>,
        #[arg(long)]
        result_file: PathBuf,
        #[arg(long)]
        expected_workstream_sha256: Option<String>,
        #[arg(long)]
        execution_group_id: Option<String>,
    },
    Get {
        #[arg(long)]
        path: String,
    },
    List {
        #[arg(long)]
        q: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long,value_parser=["path","updated_desc"],default_value="path")]
        sort: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    Put {
        #[arg(long)]
        path: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        expected_sha256: Option<String>,
    },
    Abort {
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        expected_sha256: String,
    },
    Health,
    Stats {
        #[arg(long,default_value_t=30,value_parser=clap::value_parser!(u32).range(7..=90))]
        days: u32,
        #[arg(long)]
        project: Option<String>,
    },
}
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    url: Option<String>,
    agent_id: Option<String>,
    namespace: Option<String>,
    read_namespaces: Option<Vec<String>>,
    token_env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    write_protocol: Option<String>,
    #[serde(default)]
    projects: BTreeMap<String, PathBuf>,
}
fn default_config() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home).join(".config"));
    Some(base.join("harness-server/client.json"))
}
fn project(explicit: Option<String>, cfg: &Config) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(s) = explicit {
        harness_server::document::slug(&s)?;
        return Ok(s);
    }
    let cwd = std::env::current_dir()?.canonicalize()?;
    let mut candidates = Vec::new();
    for (id, root) in &cfg.projects {
        if !root.is_absolute() {
            return Err("project binding must use an absolute local path".into());
        }
        let root = match root.canonicalize() {
            Ok(p) => p,
            Err(_) if !cwd.starts_with(root) => continue,
            Err(_) => {
                return Err(
                    "current project binding is unavailable; run harness doctor or bind".into(),
                );
            }
        };
        if cwd.starts_with(&root) {
            candidates.push((root.components().count(), id.clone()));
        }
    }
    candidates.sort_by_key(|b| std::cmp::Reverse(b.0));
    if candidates.is_empty() {
        return Err("provide --project or configure a local project binding".into());
    }
    if candidates.len() > 1 && candidates[0].0 == candidates[1].0 {
        return Err("ambiguous local project bindings".into());
    }
    Ok(candidates[0].1.clone())
}
fn init(
    path: &std::path::Path,
    args: &Args,
    verified: Option<&Value>,
) -> Result<Value, Box<dyn std::error::Error>> {
    use fs2::FileExt;
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    if !parent.exists() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let lockpath = parent.join(format!(
        ".{}.lock",
        path.file_name()
            .ok_or("config path needs a filename")?
            .to_string_lossy()
    ));
    for p in [path, lockpath.as_path()] {
        if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("config and lock must not be symlinks".into());
        }
    }
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lockpath)?;
    lock.try_lock_exclusive()
        .map_err(|_| "another client initialization is in progress")?;
    let mut cfg: Config = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(path)?)?
    } else {
        Config::default()
    };
    if let Some(protocol) = &args.write_protocol {
        cfg.write_protocol = Some(protocol.clone());
    }
    if let Command::Setup {
        token_env,
        project,
        project_root,
    } = &args.command
    {
        if let Some(name) = token_env {
            validate_token_env(name)?;
            cfg.token_env = Some(name.clone());
        }
        if let (Some(id), Some(root)) = (project, project_root) {
            harness_server::document::slug(id)?;
            let root = root.canonicalize()?;
            if !root.is_dir() {
                return Err("project root must be a directory".into());
            }
            cfg.projects.insert(id.clone(), root);
        }
    }
    if let Command::Bind { project, root } = &args.command {
        harness_server::document::slug(project)?;
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err("project root must be a directory".into());
        }
        cfg.projects.insert(project.clone(), root);
    }
    if let Some(info) = verified {
        cfg.write_protocol = Some(
            if info["write_protocols"]
                .as_array()
                .is_some_and(|a| a.iter().any(|p| p == "scoped-v2"))
            {
                "scoped-v2"
            } else {
                "v1"
            }
            .into(),
        );
    }
    let binding = matches!(args.command, Command::Bind { .. });
    let previous_id = cfg.agent_id.clone();
    let id = args
        .agent_id
        .clone()
        .or(cfg.agent_id.clone())
        .or_else(|| {
            verified
                .and_then(|v| v["agent_id"].as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            if binding {
                String::new()
            } else {
                format!("agent-{}", uuid::Uuid::new_v4())
            }
        });
    let ns = args.namespace.clone().or_else(|| {
        if binding || previous_id.as_deref() == Some(id.as_str()) {
            cfg.namespace.clone()
        } else {
            None
        }
    });
    if !binding {
        harness_server::document::slug(&id)?;
    }
    if let Some(ns) = &ns {
        harness_server::document::slug(ns)?;
    }
    let reads = args
        .read_namespaces
        .as_deref()
        .map(scopes)
        .transpose()?
        .or_else(|| cfg.read_namespaces.clone());
    if let Some(info) = verified {
        if info["agent_id"].as_str() != Some(id.as_str()) {
            return Err(
                "local Agent identity differs from credential-bound server identity".into(),
            );
        }
        let allowed = info["namespaces"]
            .as_array()
            .ok_or("server namespace discovery unsupported")?;
        for ns in ns.iter().chain(reads.iter().flatten()) {
            if !allowed
                .iter()
                .any(|v| v["id"].as_str() == Some(ns.as_str()) && v["read"] == true)
            {
                return Err(
                    "namespace was not created or is not authorized; configure it in /admin".into(),
                );
            }
        }
    }
    if !binding {
        cfg.agent_id = Some(id.clone());
    }
    cfg.namespace = ns.clone();
    cfg.read_namespaces = reads.clone();
    if let Some(url) = &args.url {
        cfg.url = Some(url.clone());
    }
    if path.exists()
        && matches!(
            args.command,
            Command::Setup { .. } | Command::Bind { .. } | Command::Connect
        )
    {
        let backup = parent.join(format!("client-{}.bak", uuid::Uuid::new_v4()));
        std::fs::copy(path, backup)?;
    }
    let tmp = parent.join(format!(".client-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(&cfg)?)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        #[cfg(unix)]
        {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    Ok(
        json!({"agent_id":cfg.agent_id,"namespace":ns,"read_namespaces":reads,"config":path,"write_protocol":cfg.write_protocol,"projects":cfg.projects,"status":if verified.is_some(){"connected"}else{"local_configured"},"access":if verified.is_some(){"verified against existing server catalog; no space created"}else{"unverified identity only; server administrator creates namespaces and binds credentials/grants"}}),
    )
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = Args::parse();
    let timing = Timings {
        started: std::time::Instant::now(),
        requests: std::cell::Cell::new(0),
        enabled: args.timings,
    };
    if matches!(args.command, Command::Setup { .. })
        && args.url.is_none()
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
    {
        use std::io::Write;
        eprint!("Service URL (leave empty to keep existing/default): ");
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !line.trim().is_empty() {
            args.url = Some(line.trim().to_owned());
        }
    }
    let config_path = args.config.clone().or_else(default_config);
    if matches!(args.command, Command::Init | Command::Bind { .. }) {
        let value = init(
            config_path
                .as_deref()
                .ok_or("provide --config when no user config directory is available")?,
            &args,
            None,
        )?;
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    let mut cfg = if let Some(path) = config_path.clone().filter(|p| {
        p.exists() || (args.config.is_some() && !matches!(args.command, Command::Setup { .. }))
    }) {
        serde_json::from_str::<Config>(&std::fs::read_to_string(path)?)?
    } else {
        Config::default()
    };
    if let Command::Setup {
        token_env: Some(name),
        ..
    } = &args.command
    {
        validate_token_env(name)?;
        cfg.token_env = Some(name.clone());
    }
    if let Some(scope) = &args.scope {
        args.namespace = Some(if scope == "shared" {
            "shared".to_owned()
        } else {
            cfg.namespace.clone().filter(|s| s != "shared").ok_or(
                "device scope is not connected; run connect --namespace <authorized ID> first",
            )?
        });
    }
    let agent_id = args.agent_id.clone().or(cfg.agent_id.clone());
    let automatic_handoff = (matches!(&args.command, Command::Abort { .. })
        || matches!(&args.command,Command::Finish {mode,..} if !matches!(mode.as_str(),"log"|"log_only")))
        && args.namespace.is_none();
    let namespace = if automatic_handoff {
        None
    } else {
        args.namespace.clone().or(cfg.namespace.clone())
    };
    let read_namespaces = args
        .read_namespaces
        .as_deref()
        .map(scopes)
        .transpose()?
        .or(cfg.read_namespaces.clone());
    if let Some(id) = &agent_id {
        harness_server::document::slug(id)?;
    }
    if let Some(ns) = &namespace {
        harness_server::document::slug(ns)?;
    }
    if let Command::Instructions { bootstrap, json } = args.command {
        if let Some(reads) = &read_namespaces {
            scopes(&reads.join(","))?;
        }
        let instructions = if bootstrap {
            include_str!("../client-bootstrap.md").to_owned()
        } else {
            let device = match namespace.as_deref() {
                Some("shared") => "未接入独立设备空间（当前选择 shared）",
                Some(ns) => ns,
                None => "未在本机指定；按服务默认值，首次用 namespaces 确认设备空间",
            };
            include_str!("../client-instructions.md")
                .replace(
                    "{{AGENT_ID}}",
                    agent_id.as_deref().unwrap_or("以凭据绑定身份为准"),
                )
                .replace("{{DEVICE_NAMESPACE}}", device)
                .replace(
                    "{{READ_NAMESPACES}}",
                    &read_namespaces
                        .as_ref()
                        .map(|v| v.join(", "))
                        .unwrap_or_else(|| "按服务默认值".into()),
                )
        };
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "schema_version":1,"version":env!("CARGO_PKG_VERSION"),
                    "source":"embedded-client","agent_id":agent_id,"task_namespace":namespace,
                    "read_namespaces":read_namespaces,"instructions":instructions
                }))?
            );
        } else {
            print!("{instructions}");
        }
        return Ok(());
    }
    if matches!(args.command, Command::Identity) {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"agent_id":agent_id,"namespace":namespace,"read_namespaces":read_namespaces,"access":"local configuration; not service authorization"})
            )?
        );
        return Ok(());
    }
    let url = args
        .url
        .clone()
        .or(cfg.url.clone())
        .unwrap_or_else(|| "http://localhost:8080".to_owned());
    let base = reqwest::Url::parse(&url)?;
    if !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || base.path() != "/"
    {
        return Err("service URL must be an origin without credentials, query or path".into());
    }
    let loopback = base.host_str().is_some_and(|h| {
        h == "localhost"
            || h == "[::1]"
            || h == "::1"
            || h.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if base.scheme() != "https" && !(base.scheme() == "http" && loopback) {
        return Err("non-loopback service URL requires HTTPS".into());
    }
    if matches!(args.command, Command::Doctor) {
        let token_env = cfg.token_env.as_deref().unwrap_or("HARNESS_TOKEN");
        let bindings:Vec<_>=cfg.projects.iter().map(|(id,path)|json!({"project":id,"root":path,"exists":path.is_dir(),"absolute":path.is_absolute()})).collect();
        let mut result = json!({"version":env!("CARGO_PKG_VERSION"),"config":config_path,"url":url,"url_source":if args.url.is_some(){"argument/environment"}else if cfg.url.is_some(){"config"}else{"default"},"token_env":token_env,"token_present":std::env::var(token_env).is_ok_and(|t|!t.is_empty()),"local_identity":{"agent_id":agent_id,"namespace":namespace,"read_namespaces":read_namespaces},"bindings":bindings,"project_match":project(None,&cfg).ok(),"project_match_error":project(None,&cfg).err().map(|e|e.to_string()),"write_protocol":args.write_protocol.as_deref().or(cfg.write_protocol.as_deref()).unwrap_or("v1"),"read_only":true});
        result["build"] = harness_server::build_info::metadata();
        if let Ok(token) = std::env::var(token_env) {
            let mut builder = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none());
            if loopback {
                builder = builder.no_proxy();
            }
            let mut request = builder
                .build()?
                .get(base.join("v1/namespaces")?)
                .bearer_auth(token);
            if let Some(id) = &agent_id {
                request = request.header("X-Harness-Agent-Id", id);
            }
            timing.request();
            match request.send().await {
                Ok(r) if r.status().is_success() => {
                    let info: Value = r.json().await?;
                    let effective = namespace.as_ref().map(|n| json!(n)).unwrap_or_else(|| {
                        if info["default_write_namespace"].is_null() {
                            info["default_read_namespaces"][0].clone()
                        } else {
                            info["default_write_namespace"].clone()
                        }
                    });
                    let reads = read_namespaces
                        .as_ref()
                        .map(|r| json!(r))
                        .unwrap_or_else(|| {
                            if let Some(ns) = &namespace {
                                let mut reads = vec![ns.clone()];
                                if ns != "shared"
                                    && info["namespaces"].as_array().is_some_and(|a| {
                                        a.iter().any(|s| s["id"] == "shared" && s["read"] == true)
                                    })
                                {
                                    reads.push("shared".into());
                                }
                                json!(reads)
                            } else {
                                info["default_read_namespaces"].clone()
                            }
                        });
                    result["effective"] = json!({"namespace":effective,"read_namespaces":reads,"namespace_source":if namespace.is_some(){"local override"}else{"server default"},"read_source":if read_namespaces.is_some(){"local override"}else{"server default"}});
                    result["server"] = info;
                    result["connection"] = json!("verified");
                }
                Ok(r) => {
                    result["connection"] = json!("failed");
                    result["http_status"] = json!(r.status().as_u16());
                }
                Err(_) => {
                    result["connection"] = json!("unreachable");
                }
            }
        } else {
            result["connection"] = json!("missing_credential");
        }
        result["hint"] = json!(
            "本机显式空间优先于服务默认值；connect重新核验并启用服务支持的快速写入。凭据值不输出。"
        );
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    let token = std::env::var(cfg.token_env.as_deref().unwrap_or("HARNESS_TOKEN")).map_err(
        |_| "set the token environment variable; no credentials are stored in client config",
    )?;
    let connecting = matches!(args.command, Command::Connect | Command::Setup { .. });
    let (method, path, value, query) = match args.command.clone() {
        Command::Init
        | Command::Bind { .. }
        | Command::Doctor
        | Command::Identity
        | Command::Instructions { .. } => unreachable!(),
        Command::Namespaces | Command::Connect | Command::Setup { .. } => {
            ("GET", "namespaces", Value::Null, None)
        }
        Command::EvalRouting { suite, .. } => {
            let value: Value = serde_json::from_str(&read_input(&suite)?)?;
            harness_server::routing_eval::parse(&value)?;
            ("POST", "eval-routing", value, None)
        }
        Command::Route {
            project: p,
            scenario,
            topic,
            topic_namespace,
            content,
            candidates_only,
            candidate_offset,
        } => (
            "POST",
            "route",
            json!({"project":project(p,&cfg)?,"scenario":scenario,"topic":topic.unwrap_or_default(),"topic_namespace":topic_namespace.unwrap_or_default(),"content":content,"candidates_only":candidates_only,"candidate_offset":candidate_offset}),
            None,
        ),
        Command::Start {
            project: p,
            agent,
            native_session_id,
            title,
            workstream_id,
            scenario,
            content,
        } => (
            "POST",
            "start",
            json!({"project":project(p,&cfg)?,"agent":agent,"native_session_id":native_session_id,"title":title,"workstream_id":workstream_id.unwrap_or_default(),"scenario":scenario,"content":content}),
            None,
        ),
        Command::Finish {
            mode,
            project: p,
            session_id,
            result_file,
            expected_workstream_sha256,
            execution_group_id,
        } => {
            let result: Value = serde_json::from_str(&read_input(&result_file)?)?;
            harness_server::result_schema::validate(&result)?;
            let mut payload = json!({"mode":mode,"result":result});
            if matches!(mode.as_str(), "log" | "log_only") {
                payload["project"] = json!(project(p, &cfg)?);
            } else {
                if let Some(p) = p {
                    payload["project"] = json!(p);
                }
                payload["session_id"] =
                    json!(session_id.ok_or("tracked finish requires --session-id")?);
                payload["expected_workstream_sha256"] = json!(
                    expected_workstream_sha256
                        .ok_or("tracked finish requires the SHA from start")?
                );
            }
            if let Some(g) = execution_group_id {
                payload["execution_group_id"] = json!(g);
            }
            ("POST", "finish", payload, None)
        }
        Command::Get { path } => ("GET", "documents", Value::Null, Some(path)),
        Command::List {
            q,
            status,
            sort,
            kind,
            project,
            limit,
            offset,
        } => (
            "GET",
            "documents",
            json!({"kind":kind.unwrap_or_default(),"project":project.unwrap_or_default(),"limit":limit,"offset":offset,"q":q.unwrap_or_default(),"status":status.unwrap_or_default(),"sort":sort}),
            None,
        ),
        Command::Put {
            path,
            file,
            expected_sha256,
        } => {
            let mut v = json!({"path":path,"content":read_input(&file)?});
            if let Some(h) = expected_sha256 {
                v["expected_sha256"] = json!(h);
            }
            ("PUT", "documents", v, None)
        }
        Command::Abort {
            session_id,
            expected_sha256,
        } => (
            "POST",
            "abort",
            json!({"session_id":session_id,"expected_sha256":expected_sha256}),
            None,
        ),
        Command::Health => ("GET", "health", Value::Null, None),
        Command::Stats { days, project } => (
            "GET",
            "stats",
            json!({"days":days,"project":project.unwrap_or_default()}),
            None,
        ),
    };
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none());
    // Local service traffic must not pass through the machine's HTTP/system proxy.
    if loopback {
        builder = builder.no_proxy();
    }
    let client = builder.build()?;
    let scoped = namespace.as_deref().is_some_and(|ns| ns != "shared")
        || agent_id.is_some()
        || read_namespaces.is_some();
    let protocol = args
        .write_protocol
        .as_deref()
        .or(cfg.write_protocol.as_deref())
        .unwrap_or("v1");
    if !matches!(protocol, "v1" | "scoped-v2") {
        return Err("unknown write_protocol; use v1 or scoped-v2".into());
    }
    let fast_write =
        protocol == "scoped-v2" && method != "GET" && !matches!(path, "route" | "eval-routing");
    if scoped && method != "GET" && !matches!(path, "route" | "eval-routing") && !fast_write {
        // An old server ignores headers. Fail before sending a potentially mis-scoped write.
        let mut probe = client.get(base.join("v1/health")?).bearer_auth(&token);
        if let Some(ns) = &namespace {
            probe = probe.header("X-Harness-Namespace", ns);
        }
        if let Some(id) = &agent_id {
            probe = probe.header("X-Harness-Agent-Id", id);
        }
        timing.request();
        let response = probe
            .send()
            .await
            .map_err(|_| "scope verification unavailable; write not sent")?;
        if !response.status().is_success() {
            return Err("scope verification failed; write not sent".into());
        }
        let identity: Value = response.json().await?;
        if namespace
            .as_deref()
            .is_some_and(|ns| identity["identity"]["namespace"].as_str() != Some(ns))
            || identity["identity"]["namespace"].as_str().is_none()
            || agent_id
                .as_deref()
                .is_some_and(|id| identity["identity"]["agent_id"].as_str() != Some(id))
            || (read_namespaces.is_some() && identity["registry_version"] != 1)
        {
            return Err(
                "server does not support requested Agent/namespace scopes; write not sent".into(),
            );
        }
    }
    let target = base.join(&format!(
        "{}/{path}",
        if fast_write { "v2/scoped" } else { "v1" }
    ))?;
    let mut request = client.request(method.parse()?, target).bearer_auth(token);
    // Reject old servers that silently ignore scope headers. Every scoped success must echo identity.
    if let Some(ns) = &namespace {
        request = request.header("X-Harness-Namespace", ns);
    }
    if matches!(path, "route" | "start" | "eval-routing")
        && let Some(scopes) = &read_namespaces
    {
        request = request.header("X-Harness-Read-Namespaces", scopes.join(","));
    }
    if let Some(id) = &agent_id {
        request = request.header("X-Harness-Agent-Id", id);
    }
    if method != "GET" {
        if !matches!(path, "route" | "eval-routing") {
            let key = args
                .idempotency_key
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            eprintln!(
                "Idempotency-Key: {key} (reuse this key and identical payload after an uncertain write)"
            );
            request = request.header("Idempotency-Key", key);
        }
        request = request.json(&value);
    }
    if let Some(path) = query {
        request = request.query(&[("path", path)]);
    } else if method == "GET" && path == "documents" {
        request = request.query(&value);
    }
    if method == "GET" && path == "stats" {
        request = request.query(&value);
    }
    timing.request();
    let response=request.send().await.map_err(|_|"service unreachable or request timed out; write outcome unknown, retry with the same idempotency key")?;
    let status = response.status();
    if fast_write
        && status.is_success()
        && response
            .headers()
            .get("X-Harness-Protocol")
            .and_then(|h| h.to_str().ok())
            != Some("scoped-v2")
    {
        return Err(
            "service did not confirm scoped-v2; inspect write outcome before retrying".into(),
        );
    }
    let body = response.text().await?;
    if !status.is_success() {
        if status.is_server_error() && method != "GET" && !matches!(path, "route" | "eval-routing")
        {
            eprintln!(
                "write outcome unknown; retry the identical payload with the same idempotency key"
            );
        }
        return Err(format!("HTTP {status}: {body}").into());
    }
    let output: Value = serde_json::from_str(&body)?;
    if path == "namespaces" {
        if output["registry_version"] != 1 || output["namespaces"].as_array().is_none() {
            return Err("server does not support namespace discovery".into());
        }
        if agent_id
            .as_deref()
            .is_some_and(|id| output["agent_id"].as_str() != Some(id))
        {
            return Err("server did not confirm Agent identity".into());
        }
        if connecting {
            let mut selections = args.clone();
            selections.agent_id = output["agent_id"].as_str().map(str::to_owned);
            if selections.agent_id.is_none() {
                return Err("connect requires a credential-bound Agent; legacy client can use shared without connect".into());
            }
            let result = init(
                config_path.as_deref().ok_or("missing config path")?,
                &selections,
                Some(&output),
            )?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
    } else if scoped && output["mode"] != "SKIP" {
        if namespace
            .as_deref()
            .is_some_and(|ns| output["identity"]["namespace"].as_str() != Some(ns))
            || output["identity"]["namespace"].as_str().is_none()
            || agent_id
                .as_deref()
                .is_some_and(|id| output["identity"]["agent_id"].as_str() != Some(id))
        {
            return Err("service did not confirm Agent/namespace; incompatible server; inspect write outcome before retrying".into());
        }
        if matches!(path, "route" | "start" | "eval-routing")
            && read_namespaces
                .as_ref()
                .is_some_and(|scopes| output["read_namespaces"] != json!(scopes))
        {
            return Err(
                "service did not confirm read scopes; inspect write outcome before retrying".into(),
            );
        }
    }
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn scopes(s: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let list: Vec<_> = s.split(',').map(str::to_owned).collect();
    let mut seen = std::collections::BTreeSet::new();
    if list.len() > 8 {
        return Err("read scope maximum is 8 namespaces".into());
    }
    for ns in &list {
        harness_server::document::slug(ns)?;
        if !seen.insert(ns) {
            return Err("duplicate read namespace".into());
        }
    }
    Ok(list)
}

fn validate_token_env(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut chars = name.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err("token_env must be an environment variable name".into());
    }
    Ok(())
}
fn read_input(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    if path == std::path::Path::new("-") {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin()
            .take(1024 * 1024 + 1)
            .read_to_string(&mut s)?;
        if s.len() > 1024 * 1024 {
            return Err("stdin exceeds 1MiB".into());
        }
        Ok(s)
    } else {
        Ok(std::fs::read_to_string(path)?)
    }
}
struct Timings {
    started: std::time::Instant,
    requests: std::cell::Cell<usize>,
    enabled: bool,
}
impl Timings {
    fn request(&self) {
        self.requests.set(self.requests.get() + 1);
    }
}
impl Drop for Timings {
    fn drop(&mut self) {
        if self.enabled {
            eprintln!(
                "{}",
                json!({"cli_elapsed_ms":self.started.elapsed().as_secs_f64()*1000.0,"http_requests":self.requests.get(),"scope":"CLI进程内墙钟时间含准备和网络；不含进程启动、参数解析、模型及工具往返"})
            );
        }
    }
}
