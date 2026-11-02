//! Subordinate process owner: service death never removes the execution deadline.
use std::{
    io::Read,
    os::fd::AsRawFd,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
fn run() -> Result<String, String> {
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let args: Vec<_> = std::env::args().collect();
    let executable = args.get(1).ok_or("missing executable")?;
    let timeout: u64 = args
        .get(2)
        .ok_or("missing deadline")?
        .parse()
        .map_err(|_| "invalid deadline")?;
    let started = Instant::now();
    let mut input = std::io::stdin();
    let fd = input.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err("nonblocking stdin unavailable".into());
        }
    }
    let mut bytes = Vec::new();
    loop {
        if STOP.load(Ordering::Relaxed)
            || started.elapsed() >= Duration::from_secs(2).min(Duration::from_millis(timeout))
        {
            return Err("snapshot delivery deadline".into());
        }
        let mut buf = [0; 4096];
        match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                bytes.extend_from_slice(&buf[..n]);
                if bytes.len() > 65536 {
                    return Err("snapshot exceeds limit".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    use sha2::{Digest, Sha256};
    if args.get(3).ok_or("missing snapshot hash")? != &format!("{:x}", Sha256::digest(&bytes)) {
        return Err("incomplete or changed snapshot".into());
    }
    if STOP.load(Ordering::Relaxed) || started.elapsed() >= Duration::from_millis(timeout) {
        return Err("execution deadline before launch".into());
    }
    let child = agentpm::process::launch_session(
        Path::new(executable),
        "-",
        &std::env::var("AGENTPM_EXECUTION_ID").map_err(|e| e.to_string())?,
    )?;
    agentpm::process::finish_with_grace(
        child,
        &bytes,
        Duration::from_millis(timeout).saturating_sub(started.elapsed()),
        Duration::from_millis(50),
        || STOP.load(Ordering::Relaxed),
    )
}
fn main() {
    match run() {
        Ok(result) => {
            let (kind, code) = if result.starts_with("succeeded") {
                ("success", 0)
            } else if result.starts_with("blocked") {
                ("block", 0)
            } else {
                ("failure", 1)
            };
            eprintln!("{result}");
            let action = match kind {
                "block" => serde_json::json!({"kind":"block","reason":"fake worker blocked"}),
                "failure" => serde_json::json!({"kind":"failure","reason":"fake worker failed"}),
                _ => serde_json::json!({"kind":"success"}),
            };
            println!(
                "{}",
                serde_json::json!({"protocol_version":1,"action":action})
            );
            std::process::exit(code);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
}
