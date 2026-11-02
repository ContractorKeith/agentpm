//! Owned, bounded fake process execution. Never replay an uncertain launch.
use crate::harness::{Action, FakeMessage, MAX_BYTES};
use std::{
    io::Read,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub fn launch(executable: &Path, script: &str) -> Result<Child, String> {
    launch_session(executable, script, "external-test")
}
pub fn launch_session(executable: &Path, script: &str, session: &str) -> Result<Child, String> {
    use std::os::unix::process::CommandExt;
    Command::new(executable)
        .env("AGENTPM_EXECUTION_ID", session)
        .arg(script)
        .arg("--realtime")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("launch {}: {e}", executable.display()))
}
fn drain(mut stream: impl Read) -> (Vec<u8>, bool) {
    let mut saved = Vec::new();
    let mut buf = [0; 4096];
    let mut overflow = false;
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let keep = n.min((MAX_BYTES as usize).saturating_sub(saved.len()));
                saved.extend_from_slice(&buf[..keep]);
                overflow |= keep < n;
            }
            Err(_) => {
                overflow = true;
                break;
            }
        }
    }
    (saved, overflow)
}
pub fn finish(mut child: Child) -> Result<String, String> {
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    let stderr = child.stderr.take().ok_or("missing stderr")?;
    let out = thread::spawn(move || drain(stdout));
    let err = thread::spawn(move || drain(stderr));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if start.elapsed() > Duration::from_secs(30) {
            // Only this newly spawned process group is owned; never use stored PIDs.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            break child
                .try_wait()
                .map_err(|e| e.to_string())?
                .ok_or("timeout reaping child")?;
        }
        thread::sleep(Duration::from_millis(10));
    };
    // Reap descendants that might otherwise hold the pipes open indefinitely.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let (bytes, overflow) = out.join().map_err(|_| "stdout reader panicked")?;
    let (diagnostic, truncated) = err.join().map_err(|_| "stderr reader panicked")?;
    let diagnostics = format!(
        "stderr{}: {}",
        if truncated { " (truncated)" } else { "" },
        String::from_utf8_lossy(&diagnostic)
    );
    let validate = || -> Result<String, String> {
        if overflow {
            return Err("stdout exceeds 64 KiB or read failed".into());
        }
        let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
        let mut terminal = None;
        for line in text.lines() {
            if terminal.is_some() {
                return Err("output after terminal".into());
            }
            let message: FakeMessage =
                serde_json::from_str(line).map_err(|e| format!("invalid fake protocol: {e}"))?;
            if message.protocol_version != 1 {
                return Err("unsupported fake protocol".into());
            }
            terminal = match message.action {
                Action::Success {} => Some(("succeeded", 0)),
                Action::Failure { .. } => Some(("failed", 1)),
                Action::Block { .. } => Some(("blocked", 0)),
                Action::Delay { .. } => None,
                Action::Malformed {} => return Err("malformed action".into()),
            };
        }
        let (outcome, code) = terminal.ok_or("missing terminal report")?;
        if status.code() != Some(code) {
            return Err(format!("inconsistent child exit {status}"));
        }
        Ok(outcome.into())
    };
    validate()
        .map(|outcome| {
            format!(
                "{outcome}; stdout: {}; {diagnostics}",
                String::from_utf8_lossy(&bytes)
            )
        })
        .map_err(|e| format!("{e}; {diagnostics}"))
}
