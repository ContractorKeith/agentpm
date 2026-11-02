use agentpm::{
    contracts::{Command, PROTOCOL_VERSION, Request, RequestId},
    harness,
};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "agentpm M0 contract demonstration")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Validate a deterministic script through an external fake-agent process.
    Fake { script: String },
    /// Create, retrieve or explicitly update immutable context snapshots.
    Context {
        #[arg(long)]
        data_dir: std::path::PathBuf,
        #[command(subcommand)]
        command: ContextCommand,
    },
}
#[derive(Subcommand)]
enum ContextCommand {
    Create {
        manifest: std::path::PathBuf,
    },
    Show {
        revision: String,
    },
    Update {
        revision: String,
        #[arg(long)]
        acknowledge: String,
        manifest: std::path::PathBuf,
    },
}
fn run() -> Result<bool, String> {
    let script = match Cli::parse().command {
        Commands::Fake { script } => script,
        Commands::Context { data_dir, command } => {
            use agentpm::context::{ContextStore, read_manifest};
            let store = ContextStore::new(data_dir.join("context"));
            let value = match command {
                ContextCommand::Create { manifest } => {
                    serde_json::json!({"version": 1, "revision": store.create(read_manifest(&manifest)?)?})
                }
                ContextCommand::Show { revision } => {
                    serde_json::json!({"version": 1, "revision": revision, "package": store.show(&revision)?})
                }
                ContextCommand::Update {
                    revision,
                    acknowledge,
                    manifest,
                } => {
                    serde_json::json!({"version": 1, "revision": store.update(&revision, &acknowledge, read_manifest(&manifest)?)?})
                }
            };
            println!("{value}");
            return Ok(false);
        }
    };
    let executable = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("fake-agent");
    let events = harness::execute(
        &Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("fake-demo")?,
            command: Command::ExerciseFake { script },
        },
        &executable,
    )?;
    let failed = events
        .iter()
        .any(|e| matches!(e.event, agentpm::contracts::Event::Failed { .. }));
    for event in events {
        println!(
            "{}",
            serde_json::to_string(&event).map_err(|e| e.to_string())?
        );
    }
    Ok(failed)
}
fn main() {
    match run() {
        Ok(false) => {}
        Ok(true) => std::process::exit(1),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}
