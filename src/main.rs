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
}
fn run() -> Result<bool, String> {
    let Commands::Fake { script } = Cli::parse().command;
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
