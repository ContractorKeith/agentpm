use agentpm::supervisor::{Command, Request, client};
use serde_json::Value;
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command as Process},
    time::{Duration, Instant},
};
struct Service {
    child: Child,
    dir: tempfile::TempDir,
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Service {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let child = Process::new(env!("CARGO_BIN_EXE_agentpm"))
            .arg("--data-dir")
            .arg(dir.path())
            .args(["supervisor", "serve"])
            .spawn()
            .unwrap();
        let service = Self { child, dir };
        wait(|| service.call("status", Command::Status {}).ok());
        service
    }
    fn call(&self, id: &str, command: Command) -> Result<Value, String> {
        client(
            self.dir.path(),
            Request {
                version: 1,
                id: id.into(),
                command,
            },
        )
    }
    fn script(&self, name: &str, text: &str) -> String {
        let path = self.dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        path.to_str().unwrap().into()
    }
}
fn wait<T>(mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < Duration::from_secs(8), "timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}
#[test]
fn durable_detached_idempotent_replay_and_restart() {
    let mut s = Service::new();
    let script = s.script(
        "slow.json",
        r#"[{"kind":"delay","milliseconds":300},{"kind":"success"}]"#,
    );
    let first = s
        .call(
            "run-one",
            Command::Start {
                script: script.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        first,
        s.call(
            "run-one",
            Command::Start {
                script: script.clone()
            }
        )
        .unwrap()
    );
    assert!(
        s.call(
            "run-one",
            Command::Start {
                script: "/different".into()
            }
        )
        .unwrap_err()
        .contains("different payload")
    );
    // Each request opens and closes a fresh socket. The child belongs to the service.
    let done = wait(|| {
        let v = s
            .call(
                "show",
                Command::Show {
                    run: "run-one".into(),
                },
            )
            .unwrap();
        (v["state"] == "finished").then_some(v)
    });
    assert!(done["result"].as_str().unwrap().contains("succeeded"));
    assert!(done["pid"].as_u64().unwrap() > 0);
    let mut cursor = 0;
    let mut kinds = Vec::new();
    loop {
        let v = s.call("events", Command::Events { cursor }).unwrap();
        let events = v["events"].as_array().unwrap();
        if events.is_empty() {
            break;
        }
        let e = &events[0];
        assert!(e["cursor"].as_u64().unwrap() > cursor);
        cursor = e["cursor"].as_u64().unwrap();
        kinds.push(e["event"]["kind"].as_str().unwrap().to_string());
    }
    assert_eq!(kinds, vec!["launch_intent", "launched", "finished"]);
    s.child.kill().unwrap();
    s.child.wait().unwrap();
    s.child = Process::new(env!("CARGO_BIN_EXE_agentpm"))
        .arg("--data-dir")
        .arg(s.dir.path())
        .args(["supervisor", "serve"])
        .spawn()
        .unwrap();
    wait(|| s.call("status", Command::Status {}).ok());
    assert_eq!(
        done,
        s.call(
            "show",
            Command::Show {
                run: "run-one".into()
            }
        )
        .unwrap()
    );
    assert_eq!(first, s.call("run-one", Command::Start { script }).unwrap());
    assert!(
        s.call("events", Command::Events { cursor }).unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn mismatch_lock_and_diagnostics() {
    let s = Service::new();
    assert!(
        client(
            s.dir.path(),
            Request {
                version: 99,
                id: "wrong".into(),
                command: Command::Start {
                    script: "/missing".into()
                }
            }
        )
        .unwrap_err()
        .contains("protocol mismatch")
    );
    assert!(
        s.call(
            "show",
            Command::Show {
                run: "wrong".into()
            }
        )
        .is_err()
    );
    let duplicate = Process::new(env!("CARGO_BIN_EXE_agentpm"))
        .arg("--data-dir")
        .arg(s.dir.path())
        .args(["supervisor", "serve"])
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("already running"));
    for (id, script) in [
        (
            "missing",
            s.dir.path().join("absent").to_str().unwrap().to_string(),
        ),
        ("invalid", s.script("invalid.json", "not json")),
    ] {
        s.call(id, Command::Start { script }).unwrap();
        let done = wait(|| {
            let v = s.call("show", Command::Show { run: id.into() }).unwrap();
            (v["state"] == "failed").then_some(v)
        });
        let result = done["result"].as_str().unwrap();
        assert!(result.contains("stderr:"));
        assert!(result.contains(if id == "missing" {
            "No such file"
        } else {
            "expected"
        }));
    }
}
fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
#[test]
fn excessive_stderr_and_stdout_are_drained_and_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("child");
    executable(
        &exe,
        "#!/bin/sh\ni=0; while [ $i -lt 30000 ]; do echo diagnostic-line >&2; i=$((i+1)); done\necho '{\"protocol_version\":1,\"action\":{\"kind\":\"success\"}}'\n",
    );
    let result =
        agentpm::process::finish(agentpm::process::launch(&exe, "unused").unwrap()).unwrap();
    assert!(result.contains("succeeded"));
    assert!(result.contains("truncated"));
    assert!(result.len() < 70000);
    executable(
        &exe,
        "#!/bin/sh\ni=0; while [ $i -lt 30000 ]; do echo excessive-output; i=$((i+1)); done\n",
    );
    assert!(
        agentpm::process::finish(agentpm::process::launch(&exe, "unused").unwrap())
            .unwrap_err()
            .contains("exceeds")
    );
}
#[test]
fn cli_starts_background_service_and_returns() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = Process::new(env!("CARGO_BIN_EXE_agentpm"))
        .arg("--data-dir")
        .arg(dir.path())
        .args(["supervisor", "start"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    // Exercise the detached service through fresh client connections.
    let script = dir.path().join("script.json");
    std::fs::write(&script, r#"[{"kind":"success"}]"#).unwrap();
    let v = client(
        dir.path(),
        Request {
            version: 1,
            id: "cli-run".into(),
            command: Command::Start {
                script: script.to_str().unwrap().into(),
            },
        },
    )
    .unwrap();
    assert_eq!(v["run"], "cli-run");
    // Service status includes the supervisor identity for local operational tooling.
    let status = client(
        dir.path(),
        Request {
            version: 1,
            id: "status".into(),
            command: Command::Status {},
        },
    )
    .unwrap();
    wait(|| {
        let v = client(
            dir.path(),
            Request {
                version: 1,
                id: "show".into(),
                command: Command::Show {
                    run: "cli-run".into(),
                },
            },
        )
        .unwrap();
        (v["state"] == "finished").then_some(v)
    });
    unsafe {
        libc::kill(status["pid"].as_i64().unwrap() as i32, libc::SIGTERM);
    }
}
