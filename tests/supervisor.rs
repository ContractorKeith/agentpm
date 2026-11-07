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
        Self::in_dir(dir)
    }
    fn in_dir(dir: tempfile::TempDir) -> Self {
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
    std::fs::remove_file(&script).unwrap();
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
        if id == "missing" {
            let error = s.call(id, Command::Start { script }).unwrap_err();
            assert!(error.contains("snapshot") && error.contains("No such file"));
            assert!(s.call("show", Command::Show { run: id.into() }).is_err());
            continue;
        }
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
/// Parallel tests may fork while a script is open for writing (ETXTBSY); retry the launch.
fn launch_retry<T>(mut f: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    for _ in 0..50 {
        match f() {
            Err(e) if e.contains("Text file busy") => std::thread::sleep(Duration::from_millis(20)),
            other => return other,
        }
    }
    f()
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
    let result = agentpm::process::finish(
        launch_retry(|| agentpm::process::launch(&exe, "unused")).unwrap(),
    )
    .unwrap();
    assert!(result.contains("succeeded"));
    assert!(result.contains("truncated"));
    assert!(result.len() < 70000);
    executable(
        &exe,
        "#!/bin/sh\ni=0; while [ $i -lt 30000 ]; do echo excessive-output; i=$((i+1)); done\n",
    );
    assert!(
        agentpm::process::finish(
            launch_retry(|| agentpm::process::launch(&exe, "unused")).unwrap()
        )
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

#[test]
fn mutation_ids_do_not_poison_reads_and_inputs_are_snapshots() {
    let s = Service::new();
    for id in ["status", "show", "events"] {
        let original = r#"[{"kind":"delay","milliseconds":200},{"kind":"success"}]"#;
        let script = s.script(&format!("{id}.json"), original);
        let first = s
            .call(
                id,
                Command::Start {
                    script: script.clone(),
                },
            )
            .unwrap();
        std::fs::write(&script, r#"[{"kind":"failure","reason":"changed"}]"#).unwrap();
        assert_eq!(
            first,
            s.call(
                id,
                Command::Start {
                    script: script.clone()
                }
            )
            .unwrap()
        );
        std::fs::remove_file(&script).unwrap();
        assert_eq!(first, s.call(id, Command::Start { script }).unwrap());
        assert_eq!(
            s.call("status", Command::Status {}).unwrap()["status"],
            "running"
        );
        let done = wait(|| {
            let v = s.call("show", Command::Show { run: id.into() }).unwrap();
            (v["state"] == "finished").then_some(v)
        });
        assert!(done["result"].as_str().unwrap().contains("succeeded"));
        assert!(s.call("events", Command::Events { cursor: 0 }).unwrap()["events"].is_array());
        let db = rusqlite::Connection::open(s.dir.path().join("state.sqlite")).unwrap();
        let saved: Vec<u8> = db
            .query_row(
                "SELECT script_bytes FROM fake_runs WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(saved, original.as_bytes());
    }
    let huge = s.script("huge.json", &"x".repeat(65537));
    assert!(
        s.call("huge", Command::Start { script: huge })
            .unwrap_err()
            .contains("exceeds")
    );
}

#[test]
fn drip_frame_has_absolute_deadline_and_service_recovers() {
    use std::{io::Write, os::unix::net::UnixStream};
    let s = Service::new();
    let mut drip = UnixStream::connect(s.dir.path().join("supervisor.sock")).unwrap();
    drip.write_all(b"{").unwrap();
    let writer = std::thread::spawn(move || {
        for _ in 0..6 {
            std::thread::sleep(Duration::from_millis(700));
            if drip.write_all(b" ").is_err() {
                break;
            }
        }
    });
    let start = Instant::now();
    assert_eq!(
        s.call("status", Command::Status {}).unwrap()["status"],
        "running"
    );
    assert!(start.elapsed() < Duration::from_millis(3800));
    writer.join().unwrap();
}

#[test]
fn fresh_foreground_directory_is_private_under_ordinary_umask() {
    use std::os::unix::process::CommandExt;
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("fresh");
    let mut command = Process::new(env!("CARGO_BIN_EXE_agentpm"));
    command
        .arg("--data-dir")
        .arg(&path)
        .args(["supervisor", "serve"]);
    unsafe {
        command.pre_exec(|| {
            libc::umask(0o022);
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    wait(|| {
        client(
            &path,
            Request {
                version: 1,
                id: "status".into(),
                command: Command::Status {},
            },
        )
        .ok()
    });
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn schema_two_upgrades_without_inventing_old_snapshots() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../src/schema.sql")).unwrap();
    db.execute_batch(include_str!("../src/supervisor-schema.sql"))
        .unwrap();
    db.execute_batch("PRAGMA user_version=2; INSERT INTO fake_runs VALUES('old','running',NULL,'old',123,'old','/','/fake','then');").unwrap();
    agentpm::store::migrate(&mut db).unwrap();
    agentpm::store::migrate(&mut db).unwrap();
    let row: (String, Option<Vec<u8>>) = db
        .query_row(
            "SELECT state,script_bytes FROM fake_runs WHERE id='old'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, ("running".into(), None));
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        agentpm::store::SCHEMA_VERSION
    );
}

fn controlled(script: String, project: &str, allowance_ms: u64) -> Command {
    Command::StartControlled {
        script,
        project: project.into(),
        allowance_ms,
        repository: None,
        milestone: None,
    }
}
fn terminal(s: &Service, run: &str) -> Value {
    wait(|| {
        let v = s.call("show", Command::Show { run: run.into() }).unwrap();
        (!["queued", "running"].contains(&v["state"].as_str().unwrap())).then_some(v)
    })
}
#[test]
fn budgets_caps_cancellation_and_persistent_repair_lineage() {
    let mut s = Service::new();
    s.call(
        "policy",
        Command::Policy {
            slots: 2,
            project_cap: 2,
        },
    )
    .unwrap();
    s.call(
        "project-a-cap",
        Command::ProjectCap {
            project: "a".into(),
            cap: 1,
        },
    )
    .unwrap();
    let script = s.script(
        "long.json",
        r#"[{"kind":"delay","milliseconds":10000},{"kind":"success"}]"#,
    );
    // Long-lived holders make the cap/global rejections independent of timing.
    s.call("hold", controlled(script.clone(), "a", 10000))
        .unwrap();
    assert!(s.call("cap", controlled(script.clone(), "a", 500)).is_err());
    s.call("other", controlled(script.clone(), "b", 10000))
        .unwrap();
    assert!(
        s.call("global", controlled(script.clone(), "c", 500))
            .is_err()
    );
    s.call("cancel-hold", Command::Cancel { run: "hold".into() })
        .unwrap();
    assert_eq!(terminal(&s, "hold")["state"], "failed");
    s.call("short", controlled(script, "a", 120)).unwrap();
    assert_eq!(terminal(&s, "short")["state"], "failed");
    let c = s
        .call(
            "c",
            Command::Controls {
                run: "short".into(),
            },
        )
        .unwrap();
    assert!(c["used_ms"].as_u64().unwrap() >= 120);
    assert!(
        s.call(
            "repair-budget",
            Command::Repair {
                run: "short".into()
            }
        )
        .is_err()
    );
    s.call(
        "cancel",
        Command::Cancel {
            run: "other".into(),
        },
    )
    .unwrap();
    assert_eq!(terminal(&s, "other")["state"], "failed");
    let script = s.script("fail.json", r#"[{"kind":"failure","reason":"repair me"}]"#);
    s.call("lineage", controlled(script, "a", 10000)).unwrap();
    terminal(&s, "lineage");
    for i in 0..3 {
        let id = format!("repair-{i}");
        let response = s
            .call(
                &id,
                Command::Repair {
                    run: "lineage".into(),
                },
            )
            .unwrap();
        assert_eq!(
            response,
            s.call(
                &id,
                Command::Repair {
                    run: "lineage".into()
                }
            )
            .unwrap()
        );
        terminal(&s, "lineage");
    }
    assert!(
        s.call(
            "fourth",
            Command::Repair {
                run: "lineage".into()
            }
        )
        .unwrap_err()
        .contains("limit")
    );
    for i in 0..3 {
        s.call(
            &format!("replan-{i}"),
            Command::Replan {
                run: "lineage".into(),
            },
        )
        .unwrap();
    }
    assert!(
        s.call(
            "replan-four",
            Command::Replan {
                run: "lineage".into()
            }
        )
        .is_err()
    );
    assert_eq!(
        s.call(
            "c",
            Command::Controls {
                run: "lineage".into()
            }
        )
        .unwrap()["repairs"],
        3
    );
    s.call(
        "extend",
        Command::Extend {
            run: "short".into(),
            additional_ms: 1000,
        },
    )
    .unwrap();
    assert!(
        s.call(
            "c",
            Command::Controls {
                run: "short".into()
            }
        )
        .unwrap()["used_ms"]
            .as_u64()
            .unwrap()
            >= 120
    );
    s.child.kill().unwrap();
    s.child.wait().unwrap();
    s.child = Process::new(env!("CARGO_BIN_EXE_agentpm"))
        .arg("--data-dir")
        .arg(s.dir.path())
        .args(["supervisor", "serve"])
        .spawn()
        .unwrap();
    wait(|| s.call("status", Command::Status {}).ok());
    let controls = s
        .call(
            "c",
            Command::Controls {
                run: "lineage".into(),
            },
        )
        .unwrap();
    assert_eq!(controls["repairs"], 3);
    assert_eq!(controls["replans"], 3);
    assert!(
        s.call(
            "fifth",
            Command::Repair {
                run: "lineage".into()
            }
        )
        .is_err()
    );
}
#[test]
fn takeover_stops_worker_and_freezes_cooperative_git_until_inspected_handback() {
    let s = Service::new();
    let repo = tempfile::tempdir().unwrap();
    for args in [
        vec!["init"],
        vec![
            "-c",
            "user.name=Keith",
            "-c",
            "user.email=keith@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    ] {
        assert!(
            Process::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let git = agentpm::git::GitService::open(repo.path()).unwrap();
    let a = git.allocate("owned", "m", "agentpm-test", "HEAD").unwrap();
    let script = s.script(
        "owned.json",
        r#"[{"kind":"delay","milliseconds":10000},{"kind":"success"}]"#,
    );
    s.call(
        "owned",
        Command::StartControlled {
            script,
            project: "p".into(),
            allowance_ms: 10000,
            repository: Some(repo.path().to_str().unwrap().into()),
            milestone: Some("m".into()),
        },
    )
    .unwrap();
    s.call(
        "take",
        Command::Takeover {
            run: "owned".into(),
        },
    )
    .unwrap();
    assert_eq!(terminal(&s, "owned")["state"], "failed");
    assert!(git.cleanup("owned", "m").unwrap_err().contains("takeover"));
    assert!(
        git.allocate("owned", "m", "agentpm-test", "HEAD")
            .unwrap_err()
            .contains("takeover")
    );
    std::fs::write(a.path.join("human.txt"), "keep me").unwrap();
    let report = s
        .call(
            "back",
            Command::Handback {
                run: "owned".into(),
            },
        )
        .unwrap();
    assert_eq!(report["evidence_valid"], false);
    assert!(report["edits"].as_str().unwrap().contains("human.txt"));
    assert!(git.cleanup("owned", "m").is_err());
    assert_eq!(
        std::fs::read_to_string(a.path.join("human.txt")).unwrap(),
        "keep me"
    );
}
#[test]
fn service_crash_retains_slot_exhausts_unknown_allowance_without_duplicate_dispatch() {
    let mut s = Service::new();
    s.call(
        "policy",
        Command::Policy {
            slots: 1,
            project_cap: 1,
        },
    )
    .unwrap();
    let script = s.script(
        "crash.json",
        r#"[{"kind":"delay","milliseconds":10000},{"kind":"success"}]"#,
    );
    let first = s
        .call("crash", controlled(script.clone(), "p", 500))
        .unwrap();
    wait(|| {
        let v = s
            .call(
                "show",
                Command::Show {
                    run: "crash".into(),
                },
            )
            .unwrap();
        (v["state"] == "running").then_some(v)
    });
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
        first,
        s.call("crash", controlled(script.clone(), "p", 500))
            .unwrap()
    );
    let c = s
        .call(
            "controls",
            Command::Controls {
                run: "crash".into(),
            },
        )
        .unwrap();
    assert_eq!(c["identity_unknown"], true);
    assert_eq!(c["used_ms"], 500);
    assert!(s.call("duplicate", controlled(script, "p", 500)).is_err());
    assert!(
        s.call(
            "take",
            Command::Takeover {
                run: "crash".into()
            }
        )
        .is_err()
    );
}

#[test]
fn guardian_retains_deadline_and_reaps_worker_without_service() {
    use std::{io::Write, process::Stdio};
    let dir = tempfile::tempdir().unwrap();
    let worker = dir.path().join("worker");
    let pidfile = dir.path().join("pid");
    executable(
        &worker,
        "#!/bin/sh\necho $$ > \"$PID_FILE\"\ntrap '' TERM\nwhile :; do sleep 10; done\n",
    );
    let mut guard = Process::new(env!("CARGO_BIN_EXE_execution-guard"))
        .arg(worker)
        .arg("1000")
        .arg("4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945")
        .env("PID_FILE", &pidfile)
        .env("AGENTPM_EXECUTION_ID", "guard-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    guard.stdin.take().unwrap().write_all(b"[]").unwrap();
    let pid: i32 = wait(|| {
        std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    });
    let start = Instant::now();
    assert!(!guard.wait().unwrap().success());
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "guardian did not reap worker"
    );
}
#[test]
fn blocked_input_and_escaped_pipe_holders_cannot_extend_deadline() {
    use std::{os::unix::process::CommandExt, process::Stdio};
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("no-input");
    executable(&exe, "#!/bin/sh\ntrap '' TERM\nsleep 10\n");
    let child = launch_retry(|| agentpm::process::launch_session(&exe, "-", "no-input")).unwrap();
    let start = Instant::now();
    assert!(
        agentpm::process::finish_controlled(
            child,
            &vec![b'x'; 65536],
            Duration::from_millis(50),
            || false
        )
        .is_err()
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    let pidfile = dir.path().join("escaped.pid");
    let start = Instant::now();
    let child = Process::new(env!("CARGO_BIN_EXE_fake-agent"))
        .arg("--fixture-escaped-pipes")
        .arg(&pidfile)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    assert!(
        agentpm::process::finish_with_timeout(child, Duration::from_millis(500))
            .unwrap()
            .starts_with("succeeded")
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    let escaped: i32 = std::fs::read_to_string(pidfile).unwrap().parse().unwrap();
    assert_eq!(
        unsafe { libc::kill(escaped, 0) },
        0,
        "fixture must still hold the pipes after owner completion"
    );
}

fn restart(s: &mut Service) {
    s.child.kill().unwrap();
    s.child.wait().unwrap();
    s.child = Process::new(env!("CARGO_BIN_EXE_agentpm"))
        .arg("--data-dir")
        .arg(s.dir.path())
        .args(["supervisor", "serve"])
        .spawn()
        .unwrap();
    wait(|| s.call("status", Command::Status {}).ok());
}
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git_cmd(dir.path(), &["init"]);
    git_cmd(
        dir.path(),
        &[
            "-c",
            "user.name=Keith",
            "-c",
            "user.email=keith@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "base",
        ],
    );
    dir
}
fn git_cmd(repo: &Path, args: &[&str]) {
    let result = Process::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn bound(s: &Service, repo: &Path, run: &str, text: &str, ms: u64) -> Command {
    Command::StartControlled {
        script: s.script(&format!("{run}.json"), text),
        project: "p".into(),
        allowance_ms: ms,
        repository: Some(repo.to_str().unwrap().into()),
        milestone: Some("m".into()),
    }
}
fn cli_cleanup(repo: &Path, run: &str) -> std::process::Output {
    Process::new(env!("CARGO_BIN_EXE_agentpm"))
        .args(["worktree", "--repo"])
        .arg(repo)
        .args(["cleanup", run, "m"])
        .output()
        .unwrap()
}
#[test]
fn unexpected_revisions_block_start_repair_and_resume_without_spending_attempts() {
    let s = Service::new();
    let repo = repo();
    let git = agentpm::git::GitService::open(repo.path()).unwrap();
    let a = git
        .allocate("revisions", "m", "test-revisions", "HEAD")
        .unwrap();
    let command = bound(
        &s,
        repo.path(),
        "revisions",
        r#"[{"kind":"failure","reason":"repair"}]"#,
        10000,
    );
    git_cmd(
        &a.path,
        &[
            "-c",
            "user.name=Keith",
            "-c",
            "user.email=keith@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "unexpected",
        ],
    );
    assert!(
        s.call("revisions", command)
            .unwrap_err()
            .contains("revision reconciliation")
    );
    git_cmd(
        &a.path,
        &["update-ref", "refs/heads/test-revisions", &a.expected_head],
    );
    s.call(
        "revisions",
        bound(
            &s,
            repo.path(),
            "revisions",
            r#"[{"kind":"failure","reason":"repair"}]"#,
            10000,
        ),
    )
    .unwrap();
    terminal(&s, "revisions");
    git_cmd(
        &a.path,
        &[
            "-c",
            "user.name=Keith",
            "-c",
            "user.email=keith@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "human",
        ],
    );
    assert!(
        s.call(
            "repair-rev",
            Command::Repair {
                run: "revisions".into()
            }
        )
        .unwrap_err()
        .contains("revision reconciliation")
    );
    assert!(
        s.call(
            "resume-rev",
            Command::Resume {
                run: "revisions".into()
            }
        )
        .unwrap_err()
        .contains("revision reconciliation")
    );
    assert_eq!(
        s.call(
            "c",
            Command::Controls {
                run: "revisions".into()
            }
        )
        .unwrap()["repairs"],
        0
    );
}
#[test]
fn canonical_reservation_blocks_independent_maintenance_through_uncertain_shutdown() {
    let s = Service::new();
    let repo = repo();
    let git = agentpm::git::GitService::open(repo.path()).unwrap();
    git.allocate("reserved", "m", "test-reserved", "HEAD")
        .unwrap();
    s.call(
        "reserved",
        bound(
            &s,
            repo.path(),
            "reserved",
            r#"[{"kind":"delay","milliseconds":10000},{"kind":"success"}]"#,
            20000,
        ),
    )
    .unwrap();
    assert!(!cli_cleanup(repo.path(), "reserved").status.success());
    assert!(
        git.allocate("reserved", "m", "test-reserved", "HEAD")
            .unwrap_err()
            .contains("reservation")
    );
    let live = wait(|| {
        let v = s
            .call(
                "s",
                Command::Show {
                    run: "reserved".into(),
                },
            )
            .unwrap();
        (v["state"] == "running").then_some(v)
    });
    let pid = live["pid"].as_i64().unwrap() as i32;
    let ready = s
        .dir
        .path()
        .join("receipts")
        .join(format!("{}.ready", live["session"].as_str().unwrap()));
    wait(|| ready.exists().then_some(()));
    // This live guardian's unreaped leader remains held by the running service.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
    s.call(
        "cancel-res",
        Command::Cancel {
            run: "reserved".into(),
        },
    )
    .unwrap();
    wait(|| {
        let c = s
            .call(
                "c",
                Command::Controls {
                    run: "reserved".into(),
                },
            )
            .unwrap();
        (c["identity_unknown"] == true).then_some(c)
    });
    assert!(!cli_cleanup(repo.path(), "reserved").status.success());
    assert_eq!(unsafe { libc::kill(pid, libc::SIGCONT) }, 0);
    terminal(&s, "reserved");
    assert!(cli_cleanup(repo.path(), "reserved").status.success());
}
#[test]
fn handback_abort_preserves_first_inspection_freeze_and_reconciles_both_commit_boundaries() {
    let mut s = Service::new();
    let repo = repo();
    let git = agentpm::git::GitService::open(repo.path()).unwrap();
    let a = git.allocate("saga", "m", "test-saga", "HEAD").unwrap();
    s.call(
        "saga",
        bound(&s, repo.path(), "saga", r#"[{"kind":"success"}]"#, 10000),
    )
    .unwrap();
    terminal(&s, "saga");
    s.call("take-saga", Command::Takeover { run: "saga".into() })
        .unwrap();
    std::fs::write(a.path.join("first"), "one").unwrap();
    let db = rusqlite::Connection::open(s.dir.path().join("state.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER abort_handback BEFORE UPDATE OF human ON execution_controls WHEN NEW.human=0 BEGIN SELECT RAISE(ABORT,'injected human=0 abort'); END;").unwrap();
    assert!(
        s.call("back-saga", Command::Handback { run: "saga".into() })
            .unwrap_err()
            .contains("injected")
    );
    assert!(!cli_cleanup(repo.path(), "saga").status.success());
    assert_eq!(
        s.call("c", Command::Controls { run: "saga".into() })
            .unwrap()["human_owned"],
        true
    );
    std::fs::write(a.path.join("second"), "two").unwrap();
    db.execute_batch("DROP TRIGGER abort_handback").unwrap();
    restart(&mut s);
    let report = s
        .call("back-saga", Command::Handback { run: "saga".into() })
        .unwrap();
    assert!(report["edits"].as_str().unwrap().contains("first"));
    assert!(!report["edits"].as_str().unwrap().contains("second"));
    assert_eq!(
        report,
        s.call("back-saga", Command::Handback { run: "saga".into() })
            .unwrap()
    );
    s.call("take-again", Command::Takeover { run: "saga".into() })
        .unwrap();
    let claims =
        rusqlite::Connection::open(repo.path().join(".git/agentpm-claims.sqlite3")).unwrap();
    claims.execute_batch("CREATE TRIGGER abort_unfreeze BEFORE DELETE ON human_ownership BEGIN SELECT RAISE(ABORT,'injected unfreeze abort'); END").unwrap();
    assert!(
        s.call("back-final", Command::Handback { run: "saga".into() })
            .is_err()
    );
    assert!(
        !db.query_row(
            "SELECT human FROM execution_controls WHERE run='saga'",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
    assert_eq!(
        claims
            .query_row("SELECT count(*) FROM human_ownership", [], |r| r
                .get::<_, u32>(0))
            .unwrap(),
        1
    );
    claims.execute_batch("DROP TRIGGER abort_unfreeze").unwrap();
    restart(&mut s);
    assert_eq!(
        claims
            .query_row("SELECT count(*) FROM human_ownership", [], |r| r
                .get::<_, u32>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        s.call("back-final", Command::Handback { run: "saga".into() })
            .unwrap()["evidence_valid"],
        false
    );
}
#[test]
fn launch_intent_failure_rolls_back_canonical_reservation_and_supervisor_state() {
    let s = Service::new();
    let repo = repo();
    let git = agentpm::git::GitService::open(repo.path()).unwrap();
    git.allocate("rollback", "m", "test-rollback", "HEAD")
        .unwrap();
    let db = rusqlite::Connection::open(s.dir.path().join("state.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER abort_intent BEFORE INSERT ON supervisor_requests WHEN NEW.id='rollback' BEGIN SELECT RAISE(ABORT,'injected intent abort'); END").unwrap();
    assert!(
        s.call(
            "rollback",
            bound(
                &s,
                repo.path(),
                "rollback",
                r#"[{"kind":"success"}]"#,
                10000
            )
        )
        .is_err()
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM fake_runs WHERE id='rollback'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    assert!(cli_cleanup(repo.path(), "rollback").status.success());
}
#[test]
fn guardian_completion_releases_recovered_slots_and_repository_reservation_without_pid_inference() {
    let mut s = Service::new();
    let repo = repo();
    let git = agentpm::git::GitService::open(repo.path()).unwrap();
    git.allocate("recover-owned", "m", "test-recover", "HEAD")
        .unwrap();
    s.call(
        "one-slot",
        Command::Policy {
            slots: 1,
            project_cap: 1,
        },
    )
    .unwrap();
    s.call(
        "recover-owned",
        bound(
            &s,
            repo.path(),
            "recover-owned",
            r#"[{"kind":"delay","milliseconds":10000},{"kind":"success"}]"#,
            1000,
        ),
    )
    .unwrap();
    wait(|| {
        let v = s
            .call(
                "s",
                Command::Show {
                    run: "recover-owned".into(),
                },
            )
            .unwrap();
        (v["state"] == "running").then_some(v)
    });
    restart(&mut s);
    // No timer/PID-based release: only the nonce/hash-bound guardian receipt counts.
    let db = rusqlite::Connection::open(s.dir.path().join("state.sqlite")).unwrap();
    db.execute(
        "UPDATE fake_runs SET pid=?1 WHERE id='recover-owned'",
        [std::process::id()],
    )
    .unwrap();
    terminal(&s, "recover-owned");
    assert_eq!(
        s.call(
            "reconcile",
            Command::Reconcile {
                run: "recover-owned".into()
            }
        )
        .unwrap()["identity_unknown"],
        false
    );
    assert!(cli_cleanup(repo.path(), "recover-owned").status.success());
    s.call(
        "new-after-recovery",
        Command::Start {
            script: s.script("after.json", r#"[{"kind":"success"}]"#),
        },
    )
    .unwrap();
    terminal(&s, "new-after-recovery");
}

#[test]
fn unknown_historical_execution_keeps_capacity_and_canonical_freeze_without_pid_kills() {
    let mut s = Service::new();
    let repo = repo();
    let git = agentpm::git::GitService::open(repo.path()).unwrap();
    git.allocate("historical", "m", "test-historical", "HEAD")
        .unwrap();
    s.call(
        "one-historical",
        Command::Policy {
            slots: 1,
            project_cap: 1,
        },
    )
    .unwrap();
    let db = rusqlite::Connection::open(s.dir.path().join("state.sqlite")).unwrap();
    db.execute("INSERT INTO fake_runs(id,state,operation,pid,session,cwd,executable,created_at) VALUES('historical','running','historical',?1,'legacy-session','/','/missing','unknown')",[std::process::id()]).unwrap();
    db.execute("INSERT INTO execution_controls(run,project,allowance_ms,used_ms,repository,milestone) VALUES('historical','p',500,750,?1,'m')",[repo.path().to_str().unwrap()]).unwrap();
    restart(&mut s);
    for id in ["history-one", "history-two"] {
        let c = s
            .call(
                id,
                Command::Reconcile {
                    run: "historical".into(),
                },
            )
            .unwrap();
        assert_eq!(c["used_ms"], 750);
        assert_eq!(c["identity_unknown"], true);
    }
    assert!(!cli_cleanup(repo.path(), "historical").status.success());
    assert!(
        s.call(
            "blocked-history",
            Command::Start {
                script: s.script("history-next.json", r#"[{"kind":"success"}]"#)
            }
        )
        .is_err()
    );
    // The deliberately unrelated stored PID is this still-running test process.
    assert_eq!(unsafe { libc::kill(std::process::id() as i32, 0) }, 0);
}

/// True once the guardian has spawned its worker, i.e. the snapshot was delivered.
fn worker_started(s: &Service, run: &str) -> bool {
    let v = s.call("pid", Command::Show { run: run.into() }).unwrap();
    v["state"] != "queued"
        && v["pid"].as_u64().is_some_and(|pid| {
            Process::new("pgrep")
                .args(["-P", &pid.to_string()])
                .output()
                .is_ok_and(|o| o.status.success())
        })
        || ["finished", "failed"].contains(&v["state"].as_str().unwrap())
}
fn signal(s: &Service, sig: i32) {
    unsafe {
        libc::kill(s.child.id() as i32, sig);
    }
}
fn controls(s: &Service, id: &str, run: &str) -> Value {
    s.call(id, Command::Controls { run: run.into() }).unwrap()
}
#[test]
fn known_completion_interval_is_not_charged_as_supervisor_idle_time() {
    let s = Service::new();
    s.call(
        "fast",
        controlled(
            s.script(
                "fast.json",
                r#"[{"kind":"delay","milliseconds":200},{"kind":"success"}]"#,
            ),
            "p",
            5000,
        ),
    )
    .unwrap();
    wait(|| worker_started(&s, "fast").then_some(()));
    // Only the supervisor stops; the guardian-owned worker completes meanwhile.
    signal(&s, libc::SIGSTOP);
    std::thread::sleep(Duration::from_millis(1500));
    signal(&s, libc::SIGCONT);
    terminal(&s, "fast");
    let used = controls(&s, "c", "fast")["used_ms"].as_u64().unwrap();
    assert!(
        (150..900).contains(&used),
        "charged {used}ms for a ~200ms worker"
    );
}
fn two_bound_runs(s: &Service, a: &Path, b: &Path) {
    for (run, repo) in [("run-a", a), ("run-b", b)] {
        agentpm::git::GitService::open(repo)
            .unwrap()
            .allocate(run, "m", &format!("test-{run}"), "HEAD")
            .unwrap();
        s.call(
            run,
            bound(
                s,
                repo,
                run,
                r#"[{"kind":"delay","milliseconds":2500},{"kind":"success"}]"#,
                5000,
            ),
        )
        .unwrap();
    }
    for run in ["run-a", "run-b"] {
        wait(|| worker_started(s, run).then_some(()));
    }
}
fn unrelated_work_proceeds(s: &Service) {
    s.call("status-ok", Command::Status {}).unwrap();
    terminal(s, "run-b");
    s.call(
        "unrelated",
        Command::Start {
            script: s.script("unrelated.json", r#"[{"kind":"success"}]"#),
        },
    )
    .unwrap();
    terminal(s, "unrelated");
}
fn returns_and_reconciles(s: &Service, a: &Path, moved: &Path) {
    // Unavailable repository: diagnostics work and the run is not declared unknown.
    assert_eq!(controls(s, "ca", "run-a")["identity_unknown"], false);
    std::fs::rename(moved, a).unwrap();
    let v = terminal(s, "run-a");
    assert_eq!(v["state"], "finished");
    let c = controls(s, "cb", "run-a");
    assert_eq!(c["identity_unknown"], false);
    assert!(c["used_ms"].as_u64().unwrap() < 4500, "{c}");
    assert!(cli_cleanup(a, "run-a").status.success());
}
#[test]
fn unavailable_repository_is_isolated_live_and_reconciles_on_return() {
    let s = Service::new();
    let (a, b) = (repo(), repo());
    let moved = a.path().with_extension("moved");
    two_bound_runs(&s, a.path(), b.path());
    signal(&s, libc::SIGSTOP);
    std::thread::sleep(Duration::from_millis(3200));
    std::fs::rename(a.path(), &moved).unwrap();
    signal(&s, libc::SIGCONT);
    unrelated_work_proceeds(&s);
    // Reservation survives: run-a still holds its slot and has not been failed.
    assert_eq!(
        s.call(
            "show-a",
            Command::Show {
                run: "run-a".into()
            }
        )
        .unwrap()["state"],
        "running"
    );
    assert!(has_attention(&s, "run-a"), "durable attention recorded");
    returns_and_reconciles(&s, a.path(), &moved);
}
fn has_attention(s: &Service, run: &str) -> bool {
    let mut cursor = 0;
    loop {
        let v = s.call("ev", Command::Events { cursor }).unwrap();
        let Some(e) = v["events"].as_array().and_then(|e| e.first().cloned()) else {
            return false;
        };
        cursor = e["cursor"].as_u64().unwrap();
        if e["run"] == run && e["event"]["kind"] == "reconciliation_attention" {
            return true;
        }
    }
}
#[test]
fn unavailable_handback_repository_does_not_fail_other_commands() {
    let s = Service::new();
    {
        let db = rusqlite::Connection::open(s.dir.path().join("state.sqlite")).unwrap();
        db.execute("INSERT INTO supervisor_requests VALUES('hb','{\"kind\":\"handback\",\"run\":\"gone\"}','{}')", []).unwrap();
        db.execute(
            "INSERT INTO handback_transfers VALUES('hb','op','/nonexistent/repo',1)",
            [],
        )
        .unwrap();
    }
    s.call("still-ok", Command::Status {}).unwrap();
    assert!(has_attention(&s, "gone"));
    let db = rusqlite::Connection::open(s.dir.path().join("state.sqlite")).unwrap();
    let pending: i64 = db
        .query_row(
            "SELECT pending FROM handback_transfers WHERE request_id='hb'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 1, "retry state is retained");
}
#[test]
fn unavailable_repository_is_isolated_at_startup_and_reconciles_on_return() {
    let mut s = Service::new();
    let (a, b) = (repo(), repo());
    let moved = a.path().with_extension("moved2");
    two_bound_runs(&s, a.path(), b.path());
    s.child.kill().unwrap();
    s.child.wait().unwrap();
    std::thread::sleep(Duration::from_millis(3200));
    std::fs::rename(a.path(), &moved).unwrap();
    restart(&mut s);
    unrelated_work_proceeds(&s);
    assert_eq!(
        s.call(
            "show-a",
            Command::Show {
                run: "run-a".into()
            }
        )
        .unwrap()["state"],
        "running"
    );
    assert!(has_attention(&s, "run-a"), "durable attention recorded");
    returns_and_reconciles(&s, a.path(), &moved);
}

fn every_control(run: &str) -> Vec<Command> {
    let run = || run.to_string();
    vec![
        Command::Show { run: run() },
        Command::Controls { run: run() },
        Command::Reconcile { run: run() },
        Command::Cancel { run: run() },
        Command::Replan { run: run() },
        Command::Repair { run: run() },
        Command::Resume { run: run() },
        Command::Extend {
            run: run(),
            additional_ms: 1000,
        },
        Command::Takeover { run: run() },
        Command::Handback { run: run() },
    ]
}
#[test]
fn unknown_run_ids_get_a_sentence_naming_the_run() {
    let s = Service::new();
    for (i, command) in every_control("ghost-run").into_iter().enumerate() {
        let e = s.call(&format!("ghost-{i}"), command).unwrap_err();
        assert!(e.contains("unknown run") && e.contains("ghost-run"), "{e}");
        assert!(!e.contains("no rows"), "{e}");
    }
}
#[test]
fn runs_created_by_the_previous_schema_remain_controllable_after_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    {
        // The schema-3 database origin/main writes: no execution_controls rows.
        let db = rusqlite::Connection::open(dir.path().join("state.sqlite")).unwrap();
        db.execute_batch(include_str!("../src/schema.sql")).unwrap();
        db.execute_batch(include_str!("../src/supervisor-schema.sql"))
            .unwrap();
        db.execute_batch("ALTER TABLE fake_runs ADD COLUMN script_bytes BLOB; PRAGMA user_version=3;
          INSERT INTO fake_runs(id,state,result,operation,session,cwd,executable,created_at) VALUES('old-done','finished','ok','old-done','old-done','/','/fake','then');
          INSERT INTO fake_runs(id,state,result,operation,session,cwd,executable,created_at) VALUES('old-failed','failed','bad','old-failed','old-failed','/','/fake','then');").unwrap();
    }
    let s = Service::in_dir(dir);
    for run in ["old-done", "old-failed"] {
        let c = s
            .call(&format!("c-{run}"), Command::Controls { run: run.into() })
            .unwrap();
        // Usage is unknown, never zero: the migrated allowance counts as consumed.
        assert_eq!(c["used_ms"], c["allowance_ms"], "{c}");
        assert_eq!(c["identity_unknown"], false);
        for (i, command) in every_control(run).into_iter().enumerate() {
            if let Err(e) = s.call(&format!("{run}-{i}"), command) {
                assert!(!e.contains("no rows"), "{run} command {i}: {e}");
            }
        }
        // A human extension restores headroom for the migrated run.
        let before = s
            .call(&format!("b-{run}"), Command::Controls { run: run.into() })
            .unwrap()["allowance_ms"]
            .as_u64()
            .unwrap();
        let c = s
            .call(
                &format!("ext-{run}"),
                Command::Extend {
                    run: run.into(),
                    additional_ms: 5000,
                },
            )
            .unwrap();
        assert_eq!(c["allowance_ms"].as_u64().unwrap(), before + 5000);
    }
}

const CONFIRM: &str = "--confirm-ended";
fn confirm(s: &Service, id: &str, run: &str) -> Result<Value, String> {
    s.call(id, Command::ConfirmEnded { run: run.into() })
}
#[test]
fn execution_in_flight_at_upgrade_has_a_human_path_out() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    {
        // origin/main database with an execution still running when it was upgraded.
        let db = rusqlite::Connection::open(dir.path().join("state.sqlite")).unwrap();
        db.execute_batch(include_str!("../src/schema.sql")).unwrap();
        db.execute_batch(include_str!("../src/supervisor-schema.sql"))
            .unwrap();
        db.execute_batch(
            "ALTER TABLE fake_runs ADD COLUMN script_bytes BLOB; PRAGMA user_version=3;",
        )
        .unwrap();
        db.execute(
            "INSERT INTO fake_runs(id,state,operation,pid,session,cwd,executable,created_at,script_bytes) VALUES('oldrun','running','oldrun',999999,'oldrun','/',?1,'then',?2)",
            rusqlite::params![env!("CARGO_BIN_EXE_fake-agent"), br#"[{"kind":"success"}]"#.to_vec()],
        )
        .unwrap();
    }
    let s = Service::in_dir(dir);
    let c = controls(&s, "c0", "oldrun");
    assert_eq!(c["identity_unknown"], true);
    assert_eq!(c["project"], "historical");
    // Every refusal names the supported command.
    for (i, command) in [
        Command::Takeover {
            run: "oldrun".into(),
        },
        Command::Handback {
            run: "oldrun".into(),
        },
        Command::Repair {
            run: "oldrun".into(),
        },
        Command::Resume {
            run: "oldrun".into(),
        },
        Command::Extend {
            run: "oldrun".into(),
            additional_ms: 1000,
        },
    ]
    .into_iter()
    .enumerate()
    {
        let e = s.call(&format!("refuse-{i}"), command).unwrap_err();
        assert!(e.contains(CONFIRM) && e.contains("oldrun"), "{e}");
    }
    let done = confirm(&s, "attest", "oldrun").unwrap();
    assert_eq!(done["identity_unknown"], false);
    // Replay is idempotent; a different payload under the same id is refused.
    assert_eq!(done, confirm(&s, "attest", "oldrun").unwrap());
    assert_eq!(
        s.call(
            "show-old",
            Command::Show {
                run: "oldrun".into()
            }
        )
        .unwrap()["state"],
        "failed"
    );
    assert!(has_event(&s, "oldrun", "human_attested_execution_ended"));
    // The slot is free and the run behaves like a stopped one.
    s.call(
        "ext",
        Command::Extend {
            run: "oldrun".into(),
            additional_ms: 600000,
        },
    )
    .unwrap();
    s.call(
        "res",
        Command::Resume {
            run: "oldrun".into(),
        },
    )
    .unwrap();
    assert_eq!(terminal(&s, "oldrun")["state"], "finished");
    // A known, healthy identity cannot be attested away.
    assert!(confirm(&s, "again", "oldrun").is_err());
}
fn has_event(s: &Service, run: &str, kind: &str) -> bool {
    let mut cursor = 0;
    loop {
        let v = s.call("evk", Command::Events { cursor }).unwrap();
        let Some(e) = v["events"].as_array().and_then(|e| e.first().cloned()) else {
            return false;
        };
        cursor = e["cursor"].as_u64().unwrap();
        if e["run"] == run && e["event"]["kind"] == kind {
            return true;
        }
    }
}
#[test]
fn crashed_guardian_unknown_identity_is_released_by_human_attestation() {
    let s = Service::new();
    let repo = repo();
    agentpm::git::GitService::open(repo.path())
        .unwrap()
        .allocate("crashed", "m", "test-crashed", "HEAD")
        .unwrap();
    s.call(
        "one",
        Command::Policy {
            slots: 1,
            project_cap: 1,
        },
    )
    .unwrap();
    s.call(
        "crashed",
        bound(
            &s,
            repo.path(),
            "crashed",
            r#"[{"kind":"delay","milliseconds":600000},{"kind":"success"}]"#,
            600000,
        ),
    )
    .unwrap();
    wait(|| worker_started(&s, "crashed").then_some(()));
    // The guardian dies without a receipt; its orphaned worker is killed by hand.
    let pid = s
        .call(
            "p",
            Command::Show {
                run: "crashed".into(),
            },
        )
        .unwrap()["pid"]
        .as_i64()
        .unwrap();
    let _ = Process::new("pkill")
        .args(["-KILL", "-P", &pid.to_string()])
        .status();
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    wait(|| (controls(&s, "c", "crashed")["identity_unknown"] == true).then_some(()));
    // Frozen: slot held, cleanup refused, refusals name the command.
    assert!(
        s.call(
            "blocked",
            Command::Start {
                script: s.script("n.json", r#"[{"kind":"success"}]"#)
            }
        )
        .is_err()
    );
    assert!(!cli_cleanup(repo.path(), "crashed").status.success());
    let e = s
        .call(
            "tk",
            Command::Takeover {
                run: "crashed".into(),
            },
        )
        .unwrap_err();
    assert!(e.contains(CONFIRM), "{e}");
    confirm(&s, "attest", "crashed").unwrap();
    assert!(cli_cleanup(repo.path(), "crashed").status.success());
    s.call(
        "next",
        Command::Start {
            script: s.script("n2.json", r#"[{"kind":"success"}]"#),
        },
    )
    .unwrap();
    terminal(&s, "next");
    s.call(
        "ext",
        Command::Extend {
            run: "crashed".into(),
            additional_ms: 1000,
        },
    )
    .unwrap();
}
#[test]
fn exhausted_state_gets_one_consistent_message_naming_the_next_command() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    {
        let db = rusqlite::Connection::open(dir.path().join("state.sqlite")).unwrap();
        db.execute_batch(include_str!("../src/schema.sql")).unwrap();
        db.execute_batch(include_str!("../src/supervisor-schema.sql"))
            .unwrap();
        db.execute_batch("ALTER TABLE fake_runs ADD COLUMN script_bytes BLOB; PRAGMA user_version=3;
          INSERT INTO fake_runs(id,state,result,operation,session,cwd,executable,created_at) VALUES('done','finished','ok','done','done','/','/fake','then');").unwrap();
    }
    let s = Service::in_dir(dir);
    let resume = s
        .call("r", Command::Resume { run: "done".into() })
        .unwrap_err();
    let repair = s
        .call("p", Command::Repair { run: "done".into() })
        .unwrap_err();
    assert_eq!(resume, repair);
    assert!(
        resume.contains("budget exhausted") && resume.contains("run extend done"),
        "{resume}"
    );
}
#[test]
fn attestation_refuses_while_an_orphaned_worker_is_alive() {
    let s = Service::new();
    let repo = repo();
    agentpm::git::GitService::open(repo.path())
        .unwrap()
        .allocate("orphan", "m", "test-orphan", "HEAD")
        .unwrap();
    s.call(
        "orphan",
        bound(
            &s,
            repo.path(),
            "orphan",
            r#"[{"kind":"delay","milliseconds":600000},{"kind":"success"}]"#,
            600000,
        ),
    )
    .unwrap();
    wait(|| worker_started(&s, "orphan").then_some(()));
    let guardian = s
        .call(
            "p",
            Command::Show {
                run: "orphan".into(),
            },
        )
        .unwrap()["pid"]
        .as_i64()
        .unwrap();
    let out = Process::new("pgrep")
        .args(["-P", &guardian.to_string()])
        .output()
        .unwrap();
    let worker: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    // Only the guardian dies; its worker is orphaned and keeps running in the worktree.
    unsafe { libc::kill(guardian as i32, libc::SIGKILL) };
    wait(|| (controls(&s, "c", "orphan")["identity_unknown"] == true).then_some(()));
    assert_eq!(unsafe { libc::kill(worker, 0) }, 0);
    let e = confirm(&s, "attest", "orphan").unwrap_err();
    assert!(e.contains(&worker.to_string()), "{e}");
    // Nothing was released: still unknown, still frozen, no second worker possible.
    assert_eq!(controls(&s, "c2", "orphan")["identity_unknown"], true);
    assert!(!cli_cleanup(repo.path(), "orphan").status.success());
    assert!(
        s.call(
            "res",
            Command::Resume {
                run: "orphan".into()
            }
        )
        .is_err()
    );
    // Once the human actually stops it, the same attestation succeeds.
    unsafe { libc::kill(worker, libc::SIGKILL) };
    wait(|| (unsafe { libc::kill(worker, 0) } != 0).then_some(()));
    let done = confirm(&s, "attest-2", "orphan").unwrap();
    assert_eq!(done["identity_unknown"], false);
    assert!(cli_cleanup(repo.path(), "orphan").status.success());
}
