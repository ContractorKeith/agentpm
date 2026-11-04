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
    Ok(unsafe { info.si_pid() } != 0
        && matches!(
            info.si_code,
            libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED
        ))
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
        exited_unreaped,
    )
    .map_err(|e| e.message)
}
pub fn finish_with_grace(
    child: Child,
    input: &[u8],
    timeout: Duration,
    grace: Duration,
    cancel: impl FnMut() -> bool,
) -> Result<String, String> {
    finish_owner(child, input, timeout, grace, true, cancel, exited_unreaped).map_err(|e| e.message)
}
#[derive(Debug)]
struct ProcessError {
    message: String,
    uncertain: bool,
}
impl From<String> for ProcessError {
    fn from(message: String) -> Self {
        Self {
            message,
            uncertain: false,
        }
    }
}
impl From<&str> for ProcessError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
impl ProcessError {
    fn uncertain(message: String) -> Self {
        Self {
            message,
            uncertain: true,
        }
    }
}
pub struct OwnedResult {
    pub result: Result<String, String>,
    pub complete: bool,
}
/// Ownership certainty is typed, independent of untrusted protocol diagnostics.
pub fn finish_with_grace_evidence(
    child: Child,
    input: &[u8],
    timeout: Duration,
    grace: Duration,
    cancel: impl FnMut() -> bool,
) -> OwnedResult {
    match finish_owner(child, input, timeout, grace, true, cancel, exited_unreaped) {
        Ok(v) => OwnedResult {
            result: Ok(v),
            complete: true,
        },
        Err(e) => OwnedResult {
            complete: !e.uncertain,
            result: Err(e.message),
        },
    }
}
// Error paths retain the Child until cleanup/reap succeeds. A failed ownership
// probe never authorizes a signal to a potentially reused numeric identity.
fn cleanup_error(mut child: Child, reason: String, kill_worker: bool) -> ProcessError {
    if !kill_worker {
        thread::spawn(move || {
            let _ = child.wait();
        });
        return ProcessError::uncertain(format!("{reason}; execution identity uncertain"));
    }
    if exited_unreaped(&child).is_err() {
        thread::spawn(move || {
            let _ = child.wait();
        });
        return ProcessError::uncertain(format!("{reason}; execution identity uncertain"));
    }
    let pgid = unsafe { libc::getpgid(child.id() as i32) };
    let killed = if pgid == child.id() as i32 {
        (unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) }) == 0
    } else {
        child.kill().is_ok()
    };
    if !killed && !exited_unreaped(&child).unwrap_or(false) {
        thread::spawn(move || {
            let _ = child.wait();
        });
        return ProcessError::uncertain(format!(
            "{reason}; cleanup denied; execution identity uncertain"
        ));
    }
    match child.wait() {
        Ok(_) => reason.into(),
        Err(e) => ProcessError::uncertain(format!(
            "{reason}; reap failed: {e}; execution identity uncertain"
        )),
    }
}
fn finish_owner(
    mut child: Child,
    input: &[u8],
    timeout: Duration,
    grace: Duration,
    kill_on_timeout: bool,
    mut cancel: impl FnMut() -> bool,
    mut probe: impl FnMut(&Child) -> Result<bool, String>,
) -> Result<String, ProcessError> {
    use std::io::Write;
    // Darwin can hide a zombie PGID even while WNOWAIT reserves its identity.
    if unsafe { libc::getpgid(child.id() as i32) } != child.id() as i32
        && !exited_unreaped(&child).unwrap_or(false)
    {
        return Err(cleanup_error(
            child,
            "child is not its owned group leader".into(),
            kill_on_timeout,
        ));
    }
    let mut setup = || -> Result<_, String> {
        let stdout = child.stdout.take().ok_or("missing stdout")?;
        let stderr = child.stderr.take().ok_or("missing stderr")?;
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let stdin = child.stdin.take();
        if let Some(stream) = &stdin {
            nonblocking(stream)?;
        }
        Ok((stdout, stderr, stdin))
    };
    let (mut stdout, mut stderr, mut stdin) = match setup() {
        Ok(v) => v,
        Err(e) => return Err(cleanup_error(child, e, kill_on_timeout)),
    };
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
        let exited = match probe(&child) {
            Ok(v) => v,
            Err(e) => return Err(cleanup_error(child, e, kill_on_timeout)),
        };
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
        return Err(ProcessError::uncertain(
            "guardian shutdown unconfirmed; execution identity uncertain".into(),
        ));
    }
    let signal = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    let signal_error = if signal != 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    if let Some(e) = &signal_error
        && e.raw_os_error() != Some(libc::ESRCH)
        && !(cfg!(target_os = "macos") && exited && e.raw_os_error() == Some(libc::EPERM))
    {
        return Err(cleanup_error(
            child,
            format!("group termination failed: {e}"),
            kill_on_timeout,
        ));
    }
    let status = match child.wait() {
        Ok(v) => v,
        Err(e) => return Err(cleanup_error(child, e.to_string(), kill_on_timeout)),
    };
    if let Some(e) = signal_error {
        let zombie_only =
            cfg!(target_os = "macos") && exited && e.raw_os_error() == Some(libc::EPERM);
        if e.raw_os_error() != Some(libc::ESRCH) && !zombie_only {
            return Err(e.to_string().into());
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
        .map_err(|e| ProcessError::from(format!("{e}; {diagnostics}")))
}

/// A separate guardian survives service death and retains the worker Child.
pub fn launch_guard(
    executable: &Path,
    session: &str,
    cwd: Option<&Path>,
    allowance_ms: u64,
    input: &[u8],
    receipt: Option<&Path>,
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
    if let Some(receipt) = receipt {
        command.arg(receipt);
    }
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.spawn().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_and_wait_errors_cleanup_real_children() {
        use std::os::unix::process::CommandExt;
        for mode in 0..4 {
            let mut command = Command::new("/bin/sleep");
            command.arg("10");
            if mode != 3 {
                command.process_group(0);
            }
            if mode != 0 {
                command.stdout(Stdio::piped());
            }
            if mode != 1 {
                command.stderr(Stdio::piped());
            }
            let child = command.spawn().unwrap();
            let pid = child.id();
            let result = if mode == 2 {
                finish_owner(
                    child,
                    &[],
                    Duration::from_secs(1),
                    Duration::from_millis(10),
                    true,
                    || false,
                    |_| Err("injected waitid error".into()),
                )
                .map_err(|e| e.message)
            } else {
                finish(child)
            };
            assert!(result.is_err());
            assert_eq!(
                unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }
        let child = Command::new("/bin/sh")
            .args(["-c", "echo 'identity uncertain' >&2; exit 2"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let evidence = finish_with_grace_evidence(
            child,
            &[],
            Duration::from_secs(1),
            Duration::from_millis(10),
            || false,
        );
        assert!(evidence.result.unwrap_err().contains("identity uncertain"));
        assert!(
            evidence.complete,
            "diagnostic text must not change ownership certainty"
        );
    }
    #[test]
    fn guardian_owner_setup_failure_records_uncertain_ownership_and_still_reaps() {
        use std::os::unix::process::CommandExt;
        // Missing stdout on a live child: the guardian owner must not kill a separate
        // worker group, but must report uncertainty and reap the child it holds.
        let child = Command::new("/bin/sleep")
            .arg("1")
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let err = finish_guard(child, &[], Duration::from_secs(1), || false).unwrap_err();
        assert!(err.contains("identity uncertain"), "{err}");
        let start = Instant::now();
        loop {
            let rc = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
            if rc == -1 {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "child never reaped"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
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
