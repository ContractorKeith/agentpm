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
        4
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
    s.call("short", controlled(script.clone(), "a", 120))
        .unwrap();
    assert!(s.call("cap", controlled(script.clone(), "a", 500)).is_err());
    s.call("other", controlled(script.clone(), "b", 10000))
        .unwrap();
    assert!(s.call("global", controlled(script, "c", 500)).is_err());
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
    let child = agentpm::process::launch_session(&exe, "-", "no-input").unwrap();
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
    let mut command = Process::new("/bin/sh");
    command
        .args([
            "-c",
            "echo '{\"protocol_version\":1,\"action\":{\"kind\":\"success\"}}'",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    unsafe {
        command.pre_exec(|| {
            let pid = libc::fork();
            if pid < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if pid == 0 {
                libc::setsid();
                libc::sleep(2);
                libc::_exit(0);
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    let start = Instant::now();
    assert!(
        agentpm::process::finish_with_timeout(child, Duration::from_millis(100))
            .unwrap()
            .starts_with("succeeded")
    );
    assert!(start.elapsed() < Duration::from_secs(1));
}
