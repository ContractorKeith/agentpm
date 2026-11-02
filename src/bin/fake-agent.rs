use agentpm::harness::{Action, FakeMessage, MAX_BYTES};
use std::io::{Read, Write};

fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let path = std::env::args_os().nth(1).ok_or("missing script")?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("script exceeds limit".into());
    }
    let actions: Vec<Action> = serde_json::from_slice(&bytes)?;
    let mut out = std::io::stdout().lock();
    for action in actions {
        if matches!(action, Action::Malformed) {
            writeln!(out, "{{malformed")?;
            return Ok(false);
        }
        let failed = matches!(action, Action::Failure { .. });
        writeln!(
            out,
            "{}",
            serde_json::to_string(&FakeMessage {
                protocol_version: agentpm::contracts::PROTOCOL_VERSION,
                action
            })?
        )?;
        if failed {
            return Ok(false);
        }
    }
    Ok(true)
}
fn main() {
    match run() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
}
