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
        .stdin(if script == "-" {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("launch {}: {e}", executable.display()))
}
pub fn launch_snapshot(executable: &Path, bytes: &[u8], session: &str) -> Result<Child, String> {
    use std::io::Write;
    let mut child = launch_session(executable, "-", session)?;
    let result = child.stdin.take().ok_or("missing stdin")?.write_all(bytes);
    if let Err(e) = result {
        let _ = child.kill();
        let _ = finish(child);
        return Err(format!("deliver snapshot: {e}"));
    }
    Ok(child)
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
// WNOWAIT reserves the leader PID/PGID until all group signals are complete.
fn exited_unreaped(child: &Child) -> Result<bool, String> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(unsafe { info.si_pid() } != 0)
}
pub fn finish(child: Child) -> Result<String, String> {
    finish_with_timeout(child, Duration::from_secs(30))
}
pub fn finish_with_timeout(mut child: Child, timeout: Duration) -> Result<String, String> {
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    let stderr = child.stderr.take().ok_or("missing stderr")?;
    let out = thread::spawn(move || drain(stdout));
    let err = thread::spawn(move || drain(stderr));
    let start = Instant::now();
    let exited = loop {
        let exited = exited_unreaped(&child)?;
        if exited || start.elapsed() >= timeout {
            break exited;
        }
        thread::sleep(Duration::from_millis(10));
    };
    // The unreaped leader reserves the numeric group identity, even after exit.
    // Never signal this group after wait() releases that identity.
    let signal = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    let signal_error = if signal != 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    let status = child.wait().map_err(|e| e.to_string())?;
    if let Some(e) = signal_error {
        // Darwin can return EPERM for a group containing only the zombie leader.
        let zombie_only =
            cfg!(target_os = "macos") && exited && e.raw_os_error() == Some(libc::EPERM);
        if e.raw_os_error() != Some(libc::ESRCH) && !zombie_only {
            return Err(e.to_string());
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exit_is_observed_without_releasing_identity_until_group_cleanup() {
        use std::os::unix::process::CommandExt;
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        // This shell has no command and exits; repeated WNOWAIT must still observe it.
        let start = Instant::now();
        while !exited_unreaped(&child).unwrap() {
            assert!(start.elapsed() < Duration::from_secs(3));
            thread::sleep(Duration::from_millis(10));
        }
        assert!(exited_unreaped(&child).unwrap());
        // finish owns the only reap, even though protocol validation fails.
        let pid = child.id();
        let result = finish_with_timeout(child, Duration::from_secs(1));
        assert!(result.is_err());
        let rc = unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) };
        assert_eq!(rc, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
    #[test]
    fn normal_exit_and_timeout_clean_up_descendant_pipe_holders() {
        use std::os::unix::process::CommandExt;
        for timeout in [false, true] {
            let command = if timeout {
                "sleep 10 & wait"
            } else {
                "sleep 10 & echo '{\"protocol_version\":1,\"action\":{\"kind\":\"success\"}}'"
            };
            let child = Command::new("/bin/sh")
                .args(["-c", command])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap();
            let start = Instant::now();
            let result = finish_with_timeout(child, Duration::from_millis(100));
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "descendant pipes were not closed"
            );
            assert_eq!(result.is_err(), timeout);
        }
    }
}
