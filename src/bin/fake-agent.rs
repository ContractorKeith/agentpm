use agentpm::harness::{Action, FakeMessage, MAX_BYTES};
use std::io::{Read, Write};

fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let path = std::env::args_os().nth(1).ok_or("missing script")?;
    let mut bytes = Vec::new();
    let input: Box<dyn Read> = if path == "-" {
        Box::new(std::io::stdin())
    } else {
        Box::new(std::fs::File::open(path)?)
    };
    input.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("script exceeds limit".into());
    }
    let actions: Vec<Action> = serde_json::from_slice(&bytes)?;
    let realtime = std::env::args_os()
        .nth(2)
        .is_some_and(|a| a == "--realtime");
    let mut out = std::io::stdout().lock();
    for action in actions {
        if matches!(action, Action::Malformed {}) {
            writeln!(out, "{{malformed")?;
            return Ok(false);
        }
        if realtime && let Action::Delay { milliseconds } = &action {
            std::thread::sleep(std::time::Duration::from_millis(*milliseconds));
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
// Adversarial process fixture: fork only after exec has closed spawn's error pipe.
fn escaped_pipe_fixture(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut pipe = [0; 2];
    if unsafe { libc::pipe(pipe.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if pid == 0 {
        unsafe {
            libc::close(pipe[0]);
            if libc::setsid() < 0 {
                libc::_exit(2);
            }
            let ready = 1u8;
            libc::write(pipe[1], (&ready as *const u8).cast(), 1);
            libc::close(pipe[1]);
            libc::sleep(2);
            libc::_exit(0);
        }
    }
    let mut ready = 0u8;
    unsafe {
        libc::close(pipe[1]);
    }
    let read = unsafe { libc::read(pipe[0], (&mut ready as *mut u8).cast(), 1) };
    unsafe {
        libc::close(pipe[0]);
    }
    if read != 1 || ready != 1 {
        return Err("escaped fixture failed to detach".into());
    }
    std::fs::write(path, pid.to_string())?;
    println!(
        "{}",
        serde_json::to_string(&FakeMessage {
            protocol_version: 1,
            action: Action::Success {}
        })?
    );
    Ok(())
}
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--fixture-escaped-pipes") {
        let path = std::env::args_os().nth(2).expect("fixture pid file");
        if let Err(e) = escaped_pipe_fixture(std::path::Path::new(&path)) {
            eprintln!("{e}");
            std::process::exit(2);
        }
        return;
    }
    match run() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
}
