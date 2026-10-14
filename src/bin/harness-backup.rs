use clap::{Parser, Subcommand};
use harness_server::backup::{self, Config};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Online immutable-store backups; OSS credentials stay outside JSON"
)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Create {
        #[arg(long)]
        local_only: bool,
    },
    Upload {
        #[arg(long)]
        manifest: PathBuf,
    },
    Verify {
        #[arg(long)]
        manifest: PathBuf,
    },
}

fn run() -> harness_server::error::Result<serde_json::Value> {
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    match args.command {
        Command::Create { local_only } => backup::create(&config, local_only),
        Command::Upload { manifest } => backup::upload(&config, &manifest),
        Command::Verify { manifest } => backup::verify(&config, &manifest),
    }
}

fn main() {
    match run() {
        Ok(result) => println!("{result}"),
        Err(error) => {
            if let harness_server::error::Error::Storage(io) = error {
                eprintln!(
                    "Backup I/O failed ({:?}); check paths, permissions and disk space; retain completed archives",
                    io.kind()
                );
            } else {
                eprintln!("Backup failed: {error}");
            }
            std::process::exit(1);
        }
    }
}
