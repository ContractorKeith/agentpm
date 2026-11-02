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
    launch_in(executable, script, session, None)
}
pub fn launch_in(
    executable: &Path,
    script: &str,
    session: &str,
    cwd: Option<&Path>,
) -> Result<Child, String> {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(executable);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command
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
fn nonblocking(fd: &impl std::os::fd::AsRawFd) -> Result<(), String> {
    let raw = fd.as_raw_fd();
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}
fn collect(stream: &mut impl Read, saved: &mut Vec<u8>, overflow: &mut bool) {
    let mut buf = [0; 4096];
    // Bound work per tick even when a producer never stops writing.
    for _ in 0..16 {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let keep = n.min((MAX_BYTES as usize).saturating_sub(saved.len()));
                saved.extend_from_slice(&buf[..keep]);
                *overflow |= keep < n;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(_) => {
                *overflow = true;
                break;
            }
        }
    }
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
pub fn finish_with_timeout(child: Child, timeout: Duration) -> Result<String, String> {
    finish_controlled(child, &[], timeout, || false)
}
/// Only a live Child whose unreaped leader reserves the group may be signaled.
/// Pipes are nonblocking: escaped pipe holders cannot keep this owner waiting.
pub fn finish_controlled(
    child: Child,
    input: &[u8],
    timeout: Duration,
    cancel: impl FnMut() -> bool,
) -> Result<String, String> {
    finish_with_grace(child, input, timeout, Duration::from_millis(200), cancel)
}
pub fn finish_guard(
    child: Child,
    input: &[u8],
    timeout: Duration,
    cancel: impl FnMut() -> bool,
) -> Result<String, String> {
    finish_owner(
        child,
        input,
        timeout,
        Duration::from_millis(200),
        false,
        cancel,
    )
}
pub fn finish_with_grace(
    child: Child,
    input: &[u8],
    timeout: Duration,
    grace: Duration,
    cancel: impl FnMut() -> bool,
) -> Result<String, String> {
    finish_owner(child, input, timeout, grace, true, cancel)
}
fn finish_owner(
    mut child: Child,
    input: &[u8],
    timeout: Duration,
    grace: Duration,
    kill_on_timeout: bool,
    mut cancel: impl FnMut() -> bool,
) -> Result<String, String> {
    use std::io::Write;
    let mut stdout = child.stdout.take().ok_or("missing stdout")?;
    let mut stderr = child.stderr.take().ok_or("missing stderr")?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut stdin = child.stdin.take();
    if let Some(stream) = &stdin {
        nonblocking(stream)?;
    }
    let mut offset = 0;
    let (mut bytes, mut diagnostic) = (Vec::new(), Vec::new());
    let (mut overflow, mut truncated) = (false, false);
    let start = Instant::now();
    let mut stopping = None;
    let exited = loop {
        collect(&mut stdout, &mut bytes, &mut overflow);
        collect(&mut stderr, &mut diagnostic, &mut truncated);
        if let Some(stream) = &mut stdin {
            match stream.write(&input[offset..]) {
                Ok(n) => offset += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => {
                    stdin = None;
                }
            }
            if offset == input.len() {
                stdin = None;
            }
        }
        let exited = exited_unreaped(&child)?;
        if exited {
            break true;
        }
        if stopping.is_none() && (cancel() || start.elapsed() >= timeout) {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGTERM);
            }
            stopping = Some(Instant::now());
            stdin = None;
        }
        if stopping.is_some_and(|t| t.elapsed() >= grace) {
            break false;
        }
        thread::sleep(Duration::from_millis(10));
    };
    if !exited && !kill_on_timeout {
        // The guardian may be descheduled while holding its worker Child.
        // Killing it here could orphan that separate group. Retain uncertainty.
        thread::spawn(move || {
            let _ = child.wait();
        });
        return Err("guardian shutdown unconfirmed; execution identity uncertain".into());
    }
    let signal = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    let signal_error = if signal != 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    let status = child.wait().map_err(|e| e.to_string())?;
    if let Some(e) = signal_error {
        let zombie_only =
            cfg!(target_os = "macos") && exited && e.raw_os_error() == Some(libc::EPERM);
        if e.raw_os_error() != Some(libc::ESRCH) && !zombie_only {
            return Err(e.to_string());
        }
    }
    collect(&mut stdout, &mut bytes, &mut overflow);
    collect(&mut stderr, &mut diagnostic, &mut truncated);
    if stopping.is_some() {
        return Err("execution canceled or deadline exhausted".into());
    }
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

/// A separate guardian survives service death and retains the worker Child.
pub fn launch_guard(
    executable: &Path,
    session: &str,
    cwd: Option<&Path>,
    allowance_ms: u64,
    input: &[u8],
) -> Result<Child, String> {
    use std::os::unix::process::CommandExt;
    let guard = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("execution-guard");
    let mut command = Command::new(guard);
    command
        .arg(executable)
        .arg(allowance_ms.to_string())
        .arg({
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(input))
        })
        .env("AGENTPM_EXECUTION_ID", session)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.spawn().map_err(|e| e.to_string())
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
