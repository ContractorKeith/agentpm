use agentpm::{
    contracts::{Command, PROTOCOL_VERSION, Request, RequestId},
    harness,
};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "agentpm local supervisor and fake execution")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    #[arg(long, global = true)]
    data_dir: Option<std::path::PathBuf>,
}
#[derive(Subcommand)]
enum Commands {
    /// Validate a deterministic script through an external fake-agent process.
    Fake { script: String },
    /// Create, retrieve or explicitly update immutable context snapshots.
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    /// Manage repository-local owned milestone worktrees.
    Worktree {
        #[arg(long)]
        repo: std::path::PathBuf,
        #[command(subcommand)]
        action: WorktreeCommand,
    },
    Supervisor {
        #[command(subcommand)]
        command: SupervisorCommand,
    },
    Run {
        #[command(subcommand)]
        command: RunCommand,
    },
    Events {
        #[arg(long, default_value_t = 0)]
        cursor: u64,
    },
}
#[derive(Subcommand)]
enum SupervisorCommand {
    Start,
    Status,
    #[command(hide = true)]
    Serve,
}
#[derive(Subcommand)]
enum RunCommand {
    Start {
        script: String,
        #[arg(long)]
        command_id: String,
    },
    Show {
        run: String,
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
    use agentpm::supervisor::{self, Command as ServiceCommand, Request as ServiceRequest};
    let cli = Cli::parse();
    let dir = match cli.data_dir {
        Some(dir) => dir,
        None => std::path::PathBuf::from(std::env::var("HOME").map_err(|e| e.to_string())?)
            .join(".agentpm"),
    };
    let script = match cli.command {
        Commands::Fake { script } => script,
        Commands::Context { command } => {
            use agentpm::context::{ContextStore, read_manifest};
            let store = ContextStore::new(dir.join("context"));
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
        command => {
            let (id, command) = match command {
                Commands::Supervisor {
                    command: SupervisorCommand::Serve,
                } => {
                    supervisor::serve(&dir)?;
                    return Ok(false);
                }
                Commands::Supervisor {
                    command: SupervisorCommand::Start,
                } => {
                    use std::os::unix::{fs::DirBuilderExt, process::CommandExt};
                    let mut builder = std::fs::DirBuilder::new();
                    builder.recursive(true).mode(0o700);
                    builder.create(&dir).map_err(|e| e.to_string())?;
                    use std::os::unix::fs::PermissionsExt;
                    let meta = std::fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
                    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
                        return Err("data directory must be private (chmod 700)".into());
                    }
                    let log = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(dir.join("supervisor.log"))
                        .map_err(|e| e.to_string())?;
                    let mut child = std::process::Command::new(
                        std::env::current_exe().map_err(|e| e.to_string())?,
                    )
                    .arg("--data-dir")
                    .arg(&dir)
                    .args(["supervisor", "serve"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(log)
                    .process_group(0)
                    .spawn()
                    .map_err(|e| e.to_string())?;
                    for _ in 0..100 {
                        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                            return Err(format!(
                                "supervisor start failed; inspect {}",
                                dir.join("supervisor.log").display()
                            ));
                        }
                        if let Ok(value) = supervisor::client(
                            &dir,
                            ServiceRequest {
                                version: 1,
                                id: "status".into(),
                                command: ServiceCommand::Status {},
                            },
                        ) {
                            println!("{value}");
                            return Ok(false);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    return Err("supervisor startup timed out; inspect supervisor.log".into());
                }
                Commands::Supervisor {
                    command: SupervisorCommand::Status,
                } => ("status".into(), ServiceCommand::Status {}),
                Commands::Run {
                    command: RunCommand::Start { script, command_id },
                } => (command_id, ServiceCommand::Start { script }),
                Commands::Run {
                    command: RunCommand::Show { run },
                } => ("show".into(), ServiceCommand::Show { run }),
                Commands::Events { cursor } => ("events".into(), ServiceCommand::Events { cursor }),
                Commands::Fake { .. } | Commands::Context { .. } | Commands::Worktree { .. } => {
                    unreachable!()
                }
            };
            println!(
                "{}",
                supervisor::client(
                    &dir,
                    ServiceRequest {
                        version: 1,
                        id,
                        command
                    }
                )?
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
