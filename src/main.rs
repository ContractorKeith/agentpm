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
    /// Manage repository-local owned milestone worktrees.
    Worktree {
        #[arg(long)]
        repo: std::path::PathBuf,
        #[command(subcommand)]
        action: WorktreeCommand,
    },
}
#[derive(Subcommand)]
enum WorktreeCommand {
    Allocate {
        run: String,
        milestone: String,
        branch: String,
        #[arg(long, default_value = "HEAD")]
        base: String,
    },
    Inspect {
        run: String,
        milestone: String,
    },
    Cleanup {
        run: String,
        milestone: String,
    },
}
fn run() -> Result<bool, String> {
    let script = match Cli::parse().command {
        Commands::Fake { script } => script,
        Commands::Worktree { repo, action } => {
            let service = agentpm::git::GitService::open(&repo)?;
            let allocation = match action {
                WorktreeCommand::Allocate {
                    run,
                    milestone,
                    branch,
                    base,
                } => service.allocate(&run, &milestone, &branch, &base)?,
                WorktreeCommand::Inspect { run, milestone } => service.inspect(&run, &milestone)?,
                WorktreeCommand::Cleanup { run, milestone } => service.cleanup(&run, &milestone)?,
            };
            println!(
                "{}",
                serde_json::to_string(&allocation).map_err(|e| e.to_string())?
            );
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
