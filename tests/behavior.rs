use agentpm::{contracts::*, store};
use rusqlite::Connection;
use std::process::Command;

fn revision() -> RevisionIdentity {
    RevisionIdentity {
        head: "head-a".into(),
        base: "base-a".into(),
        prerequisite_heads: vec!["parent-a".into()],
        context: ContextPackageId::new("ctx-a").unwrap(),
        check_configuration: "checks-a".into(),
    }
}
fn evidence() -> Evidence {
    Evidence {
        protocol_version: PROTOCOL_VERSION,
        id: EvidenceId::new("e-1").unwrap(),
        revision: revision(),
        implementer: BackendSessionId::new("impl").unwrap(),
        reviewer: BackendSessionId::new("review").unwrap(),
        checks_passed: true,
        ci_passed: true,
        review_passed: true,
    }
}
#[test]
fn lifecycle_rejects_skips_and_requires_current_independent_evidence() {
    let revision = revision();
    let mut state = MilestoneState::Queued;
    assert!(
        state
            .transition(MilestoneState::Ready, Some(&evidence()), &revision)
            .is_err()
    );
    assert_eq!(state, MilestoneState::Queued);
    for next in [
        MilestoneState::Implementing,
        MilestoneState::Checking,
        MilestoneState::Reviewing,
    ] {
        state.transition(next, None, &revision).unwrap();
    }
    assert!(
        state
            .transition(MilestoneState::Ready, None, &revision)
            .is_err()
    );
    for field in 0..7 {
        let mut stale = evidence();
        match field {
            0 => stale.revision.head = "head-b".into(),
            1 => stale.revision.base = "base-b".into(),
            2 => stale.revision.context = ContextPackageId::new("ctx-b").unwrap(),
            3 => stale.revision.prerequisite_heads.push("parent-b".into()),
            4 => stale.revision.check_configuration = "checks-b".into(),
            5 => stale.reviewer = stale.implementer.clone(),
            _ => stale.ci_passed = false,
        }
        assert!(
            state
                .transition(MilestoneState::Ready, Some(&stale), &revision)
                .is_err()
        );
        assert_eq!(state, MilestoneState::Reviewing);
    }
    state
        .transition(MilestoneState::Ready, Some(&evidence()), &revision)
        .unwrap();
    state
        .transition(MilestoneState::Completed, Some(&evidence()), &revision)
        .unwrap();
    assert!(
        state
            .transition(MilestoneState::Implementing, None, &revision)
            .is_err()
    );
    let mut run = RunState::Queued;
    assert!(run.transition(RunState::Completed, true).is_err());
    assert_eq!(run, RunState::Queued);
    run.transition(RunState::Running, false).unwrap();
    assert!(run.transition(RunState::Completed, false).is_err());
    assert_eq!(run, RunState::Running);
    run.transition(RunState::Completed, true).unwrap();
}
#[test]
fn wire_contract_roundtrips_and_rejects_unknown_input() {
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("req-1").unwrap(),
        command: agentpm::contracts::Command::ExerciseFake {
            script: "script.json".into(),
        },
    };
    let encoded = serde_json::to_string(&request).unwrap();
    assert_eq!(serde_json::from_str::<Request>(&encoded).unwrap(), request);
    let mut unsupported = request.clone();
    unsupported.protocol_version = 2;
    assert!(unsupported.validate().is_err());
    assert!(
        serde_json::from_str::<Request>(&encoded.replace(
            "\"protocol_version\":1",
            "\"extra\":true,\"protocol_version\":1"
        ))
        .is_err()
    );
    assert!(serde_json::from_str::<RunId>("\"\"").is_err());
    let event = EventEnvelope {
        protocol_version: 1,
        request_id: request.request_id,
        sequence: 7,
        logical_milliseconds: 9,
        event: Event::Succeeded {
            usage: Usage::Unknown,
        },
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"status\":\"unknown\""));
    assert_eq!(serde_json::from_str::<EventEnvelope>(&json).unwrap(), event);
    let proof = evidence();
    assert_eq!(
        serde_json::from_str::<Evidence>(&serde_json::to_string(&proof).unwrap()).unwrap(),
        proof
    );
}
#[test]
fn migration_is_repeatable_durable_and_enforces_constraints() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let mut db = Connection::open(&path).unwrap();
    store::migrate(&mut db).unwrap();
    db.execute("INSERT INTO projects VALUES ('p','repo')", [])
        .unwrap();
    db.execute("INSERT INTO runs(id,project_id,state,protocol_version,created_at) VALUES ('r','p','queued',1,'now')", []).unwrap();
    assert!(db.execute("UPDATE runs SET state='invented'", []).is_err());
    assert!(
        db.execute("UPDATE runs SET executing_milliseconds=-1", [])
            .is_err()
    );
    assert!(db.execute("DELETE FROM projects", []).is_err());
    assert!(
        db.execute("INSERT INTO projects VALUES ('p2','repo')", [])
            .is_err()
    );
    assert!(
        db.execute("UPDATE runs SET protocol_version=2", [])
            .is_err()
    );
    db.execute("INSERT INTO events(run_id,request_id,protocol_version,created_at,payload) VALUES ('r','q',1,'now','{}')", []).unwrap();
    db.execute("INSERT INTO events(run_id,request_id,protocol_version,created_at,payload) VALUES ('r','q',1,'now','{}')", []).unwrap();
    let cursor: i64 = db
        .query_row("SELECT max(sequence) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(cursor, 2);
    db.execute("INSERT INTO milestones VALUES ('m','r','reviewing')", [])
        .unwrap();
    assert!(
        db.execute(
            "INSERT INTO evidence VALUES ('e','m','h','b','ctx','[]','checks','same','same','{}')",
            []
        )
        .is_err()
    );
    assert!(
        db.execute(
            "INSERT INTO evidence VALUES ('e','missing','h','b','ctx','[]','checks','i','r','{}')",
            []
        )
        .is_err()
    );
    db.execute(
        "INSERT INTO evidence VALUES ('e','m','h','b','ctx','[]','checks','i','r','{}')",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO requests VALUES ('q','{}',NULL)", [])
        .unwrap();
    assert!(
        db.execute("INSERT INTO requests VALUES ('q','{}',NULL)", [])
            .is_err()
    );
    store::migrate(&mut db).unwrap();
    drop(db);
    let mut db = Connection::open(path).unwrap();
    store::migrate(&mut db).unwrap();
    let usage: String = db
        .query_row("SELECT usage_json FROM runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(usage, "{\"status\":\"unknown\"}");
    db.pragma_update(None, "user_version", 99).unwrap();
    assert!(store::migrate(&mut db).is_err());
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        99
    );
}
fn exercise(script: &str) -> std::process::Output {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.json");
    std::fs::write(&path, script).unwrap();
    // Cargo builds both binaries for process integration tests.
    assert!(std::path::Path::new(env!("CARGO_BIN_EXE_fake-agent")).exists());
    Command::new(env!("CARGO_BIN_EXE_agentpm"))
        .arg("fake")
        .arg(path)
        .output()
        .unwrap()
}
#[test]
fn executable_fake_modes_have_validated_external_behavior() {
    let success = exercise(r#"[{"kind":"delay","milliseconds":25},{"kind":"success"}]"#);
    assert!(success.status.success());
    assert!(success.stderr.is_empty());
    let events: Vec<EventEnvelope> = String::from_utf8(success.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events.len(), 3);
    assert_eq!(events[2].sequence, 3);
    assert_eq!(events[2].logical_milliseconds, 25);
    assert_eq!(
        events[2].event,
        Event::Succeeded {
            usage: Usage::Unknown
        }
    );
    let block = exercise(r#"[{"kind":"block","reason":"decision"}]"#);
    assert!(block.status.success());
    assert!(
        String::from_utf8(block.stdout)
            .unwrap()
            .contains("\"kind\":\"blocked\"")
    );
    let failed = exercise(r#"[{"kind":"failure","reason":"test failure"}]"#);
    assert_eq!(failed.status.code(), Some(1));
    assert!(
        String::from_utf8(failed.stdout)
            .unwrap()
            .contains("\"kind\":\"failed\"")
    );
    for script in [
        r#"[{"kind":"malformed"}]"#,
        "[]",
        r#"[{"kind":"success"},{"kind":"success"}]"#,
        r#"[{"kind":"delay","milliseconds":18446744073709551615},{"kind":"delay","milliseconds":1},{"kind":"success"}]"#,
    ] {
        let invalid = exercise(script);
        assert_eq!(invalid.status.code(), Some(2));
        assert!(invalid.stdout.is_empty());
        assert!(!invalid.stderr.is_empty());
    }
}

#[test]
fn failed_migration_rolls_back_earlier_ddl() {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute("CREATE TABLE runs (collision TEXT)", [])
        .unwrap();
    assert!(store::migrate(&mut db).is_err());
    let projects: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='projects'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(projects, 0);
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
}
