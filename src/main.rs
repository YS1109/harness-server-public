use clap::{Parser, Subcommand};
use harness_server::{
    admin_session::LoginConfig,
    api,
    store::{Store, import_directory},
    tenancy,
};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(version)]
struct Args {
    #[arg(long, env = "HARNESS_DATA_DIR", default_value = "data")]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long, env = "HARNESS_BIND", default_value = "127.0.0.1:8080")]
        bind: SocketAddr,
        #[arg(long, env = "HARNESS_AUTH_CONFIG")]
        auth_config: Option<PathBuf>,
        #[arg(long, env = "HARNESS_TENANTS_CONFIG", conflicts_with = "auth_config")]
        tenants_config: Option<PathBuf>,
    },
    /// Generate an administrator password hash offline using hidden terminal input.
    HashPassword,
    /// Offline read-only capacity/retention preview; apply only expired telemetry.
    Maintenance {
        #[arg(long)]
        prune_metrics: bool,
    },
    Import {
        #[arg(long)]
        source: PathBuf,
    },
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    match args.command {
        Command::HashPassword => harness_server::password_cli::run()?,
        Command::Maintenance { prune_metrics } => {
            tenancy::ensure_single_tenant(&args.data_dir)?;
            let store = Store::open(args.data_dir)?;
            let ledger = harness_server::metrics::Ledger::new(store.root());
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"storage":store.usage()?,"metrics":ledger.retention(prune_metrics,chrono::Utc::now())?})
                )?
            );
        }
        Command::Import { source } => {
            tenancy::ensure_single_tenant(&args.data_dir)?;
            let mut store = Store::open(args.data_dir)?;
            let docs = import_directory(&source)?;
            let count = docs.len();
            store.import(docs)?;
            println!(
                "{}",
                serde_json::json!({"imported_documents":count,"revision":store.revision})
            );
        }
        Command::Serve {
            bind,
            auth_config,
            tenants_config,
        } => {
            let app = if let Some(config) = tenants_config {
                tenancy::router(&args.data_dir, &config)?
            } else {
                tenancy::ensure_single_tenant(&args.data_dir)?;
                let principals = if let Some(path) = auth_config {
                    harness_server::auth::load(&path)?
                } else {
                    harness_server::auth::legacy(api::tokens_from_environment()?)
                };
                let login = LoginConfig::from_environment()?;
                let admin_token =
                    optional_admin_env("HARNESS_ADMIN_TOKEN")?.filter(|v| !v.is_empty());
                let store = Store::open(args.data_dir)?;
                api::router_with_admin_login(store, principals, admin_token, login)?
            };
            let listener = tokio::net::TcpListener::bind(bind).await?;
            eprintln!(
                "Harness HTTP service listening on {}",
                listener.local_addr()?
            );
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown())
                .await?;
        }
    }
    Ok(())
}

fn optional_admin_env(name: &str) -> harness_server::error::Result<Option<String>> {
    std::env::var(name).map(Some).or_else(|error| match error {
        std::env::VarError::NotPresent => Ok(None),
        _ => Err(harness_server::error::invalid(
            "Administrator environment must be valid text",
        )),
    })
}

async fn shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
