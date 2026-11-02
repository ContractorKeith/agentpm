//! Narrow child-process seam; no scheduling, daemon, adapters, or host effects.
use crate::contracts::*;
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Read},
    path::Path,
    process::{Command as ProcessCommand, Stdio},
};

pub const MAX_BYTES: u64 = 64 * 1024;
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Delay { milliseconds: u64 },
    Success {},
    Malformed {},
    Block { reason: String },
    Failure { reason: String },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FakeMessage {
    pub protocol_version: u32,
    pub action: Action,
}

// Validate the entire response before returning any protocol events to the caller.
pub fn execute(request: &Request, fake_agent: &Path) -> Result<Vec<EventEnvelope>, String> {
    request.validate()?;
    let Command::ExerciseFake { script } = &request.command;
    let mut child = ProcessCommand::new(fake_agent)
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .ok_or("missing child stdout")?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() as u64 > MAX_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return Err("fake output exceeds limit or cannot be read".into());
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let mut events = vec![EventEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: request.request_id.clone(),
        sequence: 1,
        logical_milliseconds: 0,
        event: Event::Started {},
    }];
    let mut elapsed = 0u64;
    let mut terminal = false;
    let mut failed = false;
    for line in BufReader::new(bytes.as_slice()).lines() {
        if terminal {
            return Err("output after terminal event".into());
        }
        let message: FakeMessage = serde_json::from_str(&line.map_err(|e| e.to_string())?)
            .map_err(|e| format!("malformed fake protocol: {e}"))?;
        if message.protocol_version != PROTOCOL_VERSION {
            return Err("unsupported fake protocol version".into());
        }
        let event = match message.action {
            Action::Delay { milliseconds } => {
                elapsed = elapsed
                    .checked_add(milliseconds)
                    .ok_or("logical clock overflow")?;
                Event::Delayed { milliseconds }
            }
            Action::Success {} => {
                terminal = true;
                Event::Succeeded {
                    usage: Usage::Unknown {},
                }
            }
            Action::Block { reason } => {
                terminal = true;
                Event::Blocked { reason }
            }
            Action::Failure { reason } => {
                terminal = true;
                failed = true;
                Event::Failed { reason }
            }
            Action::Malformed {} => return Err("unexpected malformed action".into()),
        };
        events.push(EventEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            sequence: events.len() as u64 + 1,
            logical_milliseconds: elapsed,
            event,
        });
    }
    let expected_exit = if failed { 1 } else { 0 };
    if !terminal || status.code() != Some(expected_exit) {
        return Err("missing terminal event or inconsistent process exit".into());
    }
    Ok(events)
}
