use clap::{Parser, Subcommand};
use harness_server::{
    api,
    store::{Store, import_directory},
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
    },
    Import {
        #[arg(long)]
        source: PathBuf,
    },
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let mut store = Store::open(args.data_dir)?;
    match args.command {
        Command::Import { source } => {
            let docs = import_directory(&source)?;
            let count = docs.len();
            store.import(docs)?;
            println!(
                "{}",
                serde_json::json!({"imported_documents":count,"revision":store.revision})
            );
        }
        Command::Serve { bind, auth_config } => {
            let principals = if let Some(path) = auth_config {
                harness_server::auth::load(&path)?
            } else {
                harness_server::auth::legacy(api::tokens_from_environment()?)
            };
            let app = api::router_with_admin(
                store,
                principals,
                std::env::var("HARNESS_ADMIN_TOKEN").ok(),
            )?;
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
