use agentpm::{
    execution::{self, Stamp},
    store,
};
use rusqlite::{Connection, params};
fn seed(db: &Connection, id: &str, allowance: i64) {
    db.execute("INSERT INTO fake_runs(id,state,operation,session,cwd,executable,created_at) VALUES(?1,'finished',?1,?1,'/','/fake','test')",[id]).unwrap();
    db.execute(
        "INSERT INTO execution_controls(run,project,allowance_ms) VALUES(?1,'p',?2)",
        params![id, allowance],
    )
    .unwrap();
}
fn stamp(ms: i64) -> Stamp<'static> {
    Stamp { epoch: "first", ms }
}
fn apply(
    db: &mut Connection,
    f: impl FnOnce(&Connection) -> Result<(), String>,
) -> Result<(), String> {
    let tx = db.transaction().unwrap();
    f(&tx)?;
    tx.commit().map_err(|e| e.to_string())
}
fn reserve(db: &mut Connection, token: &str, role: &str, ms: i64) {
    apply(db, |tx| {
        execution::reserve_at(tx, "r", "p", token, role, stamp(ms))
    })
    .unwrap();
}
fn state(db: &mut Connection, token: &str, state: &str, ms: i64) {
    apply(db, |tx| {
        execution::transition_at(tx, token, state, stamp(ms))
    })
    .unwrap();
}
#[test]
fn durable_identity_union_excludes_waits_and_counts_coordinator_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    let mut db = Connection::open(&path).unwrap();
    store::migrate(&mut db).unwrap();
    seed(&db, "r", 1000);
    reserve(&mut db, "w1", "worker", 0);
    state(&mut db, "w1", "active", 0);
    reserve(&mut db, "w2", "worker", 10);
    state(&mut db, "w2", "active", 10);
    reserve(&mut db, "c1", "coordinator", 20);
    state(&mut db, "c1", "active", 20);
    assert!(
        apply(&mut db, |tx| execution::reserve_at(
            tx,
            "r",
            "p",
            "c2",
            "coordinator",
            stamp(20)
        ))
        .is_err()
    );
    // Three workers plus a coordinator fit; the fourth worker does not.
    reserve(&mut db, "w3", "worker", 20);
    assert!(
        apply(&mut db, |tx| execution::reserve_at(
            tx,
            "r",
            "p",
            "w4",
            "worker",
            stamp(20)
        ))
        .is_err()
    );
    state(&mut db, "w3", "finished", 20);
    state(&mut db, "w1", "waiting", 30);
    state(&mut db, "w2", "waiting", 40);
    state(&mut db, "c1", "waiting", 50);
    apply(&mut db, |tx| {
        execution::tick_at(tx, "r", stamp(100)).map(|_| ())
    })
    .unwrap();
    assert_eq!(execution::show(&db, "r").unwrap()["used_ms"], 50);
    state(&mut db, "w1", "active", 110);
    state(&mut db, "w2", "active", 120);
    state(&mut db, "w1", "finished", 130);
    state(&mut db, "w2", "finished", 140);
    state(&mut db, "c1", "finished", 140);
    assert_eq!(execution::show(&db, "r").unwrap()["used_ms"], 80);
    drop(db);
    let mut db = Connection::open(path).unwrap();
    store::migrate(&mut db).unwrap();
    execution::recover(&db, &[]).unwrap();
    execution::recover_clock(
        &db,
        Stamp {
            epoch: "second",
            ms: 0,
        },
    )
    .unwrap();
    assert_eq!(execution::show(&db, "r").unwrap()["used_ms"], 80);
    apply(&mut db, |tx| {
        execution::reserve_at(
            tx,
            "r",
            "p",
            "new",
            "worker",
            Stamp {
                epoch: "second",
                ms: 0,
            },
        )
    })
    .unwrap();
    apply(&mut db, |tx| {
        execution::transition_at(
            tx,
            "new",
            "active",
            Stamp {
                epoch: "second",
                ms: 0,
            },
        )
    })
    .unwrap();
    apply(&mut db, |tx| {
        execution::transition_at(
            tx,
            "new",
            "finished",
            Stamp {
                epoch: "second",
                ms: 25,
            },
        )
    })
    .unwrap();
    assert_eq!(execution::show(&db, "r").unwrap()["used_ms"], 105);
}
#[test]
fn repeated_recovery_never_reduces_recorded_time_or_releases_unknown_identity() {
    let mut db = Connection::open_in_memory().unwrap();
    store::migrate(&mut db).unwrap();
    seed(&db, "r", 500);
    reserve(&mut db, "owned", "worker", 0);
    state(&mut db, "owned", "active", 0);
    db.execute("UPDATE fake_runs SET state='running' WHERE id='r'", [])
        .unwrap();
    db.execute(
        "UPDATE execution_controls SET used_ms=750 WHERE run='r'",
        [],
    )
    .unwrap();
    for _ in 0..2 {
        execution::recover(&db, &[]).unwrap();
        assert_eq!(execution::show(&db, "r").unwrap()["used_ms"], 750);
    }
    assert_eq!(
        db.query_row(
            "SELECT state FROM execution_identities WHERE token='owned'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "uncertain"
    );
    db.execute("UPDATE execution_policy SET slots=1,project_cap=1", [])
        .unwrap();
    assert!(execution::admit(&db, "p").is_err());
}

#[test]
fn waiting_identity_survives_restart_without_charging_human_or_ci_wait() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    let mut db = Connection::open(&path).unwrap();
    store::migrate(&mut db).unwrap();
    seed(&db, "r", 1000);
    reserve(&mut db, "waiting", "worker", 0);
    state(&mut db, "waiting", "active", 0);
    state(&mut db, "waiting", "waiting", 25);
    db.execute("UPDATE fake_runs SET state='running' WHERE id='r'", [])
        .unwrap();
    drop(db);
    let mut db = Connection::open(path).unwrap();
    store::migrate(&mut db).unwrap();
    execution::recover(&db, &[]).unwrap();
    execution::recover_clock(
        &db,
        Stamp {
            epoch: "restarted",
            ms: 0,
        },
    )
    .unwrap();
    apply(&mut db, |tx| {
        execution::tick_at(
            tx,
            "r",
            Stamp {
                epoch: "restarted",
                ms: 1000,
            },
        )
        .map(|_| ())
    })
    .unwrap();
    assert_eq!(execution::show(&db, "r").unwrap()["used_ms"], 25);
    assert_eq!(
        execution::show(&db, "r").unwrap()["identity_unknown"],
        false
    );
    apply(&mut db, |tx| {
        execution::transition_at(
            tx,
            "waiting",
            "active",
            Stamp {
                epoch: "restarted",
                ms: 1000,
            },
        )
    })
    .unwrap();
    apply(&mut db, |tx| {
        execution::transition_at(
            tx,
            "waiting",
            "finished",
            Stamp {
                epoch: "restarted",
                ms: 1020,
            },
        )
    })
    .unwrap();
    assert_eq!(execution::show(&db, "r").unwrap()["used_ms"], 45);
}
#[test]
fn overlapping_identities_exhaust_one_shared_allowance_and_cannot_reset_it() {
    let mut db = Connection::open_in_memory().unwrap();
    store::migrate(&mut db).unwrap();
    seed(&db, "r", 100);
    reserve(&mut db, "a", "worker", 0);
    state(&mut db, "a", "active", 0);
    reserve(&mut db, "b", "worker", 0);
    state(&mut db, "b", "active", 0);
    reserve(&mut db, "c", "coordinator", 0);
    state(&mut db, "c", "active", 0);
    assert!(!execution::tick_at(&db, "r", stamp(50)).unwrap());
    assert_eq!(execution::show(&db, "r").unwrap()["used_ms"], 50);
    assert!(execution::tick_at(&db, "r", stamp(100)).unwrap());
    assert_eq!(execution::show(&db, "r").unwrap()["used_ms"], 100);
    assert!(
        apply(&mut db, |tx| execution::reserve_at(
            tx,
            "r",
            "p",
            "replacement",
            "worker",
            stamp(100)
        ))
        .is_err()
    );
    for token in ["a", "b", "c"] {
        state(&mut db, token, "finished", 100);
    }
    assert!(execution::tick_at(&db, "r", stamp(200)).unwrap());
    assert_eq!(execution::show(&db, "r").unwrap()["used_ms"], 100);
}

#[test]
fn known_completion_intervals_count_overlap_once_and_exclude_idle() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Connection::open(dir.path().join("state.sqlite")).unwrap();
    store::migrate(&mut db).unwrap();
    seed(&db, "r", 5000);
    reserve(&mut db, "w1", "worker", 0);
    state(&mut db, "w1", "active", 0);
    reserve(&mut db, "w2", "worker", 100);
    state(&mut db, "w2", "active", 100);
    // Receipts are consumed long after both ended: [0,200] and [100,400] overlap.
    let late = Stamp {
        epoch: "first",
        ms: 3000,
    };
    for (token, elapsed) in [("w1", 200), ("w2", 300)] {
        apply(&mut db, |tx| {
            execution::finish_known(tx, token, elapsed, late)
        })
        .unwrap();
    }
    apply(&mut db, |tx| execution::tick_at(tx, "r", late).map(|_| ())).unwrap();
    let used: i64 = db
        .query_row(
            "SELECT used_ms FROM execution_controls WHERE run='r'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        used, 400,
        "union of known intervals, no post-completion idle"
    );
    // A later, disjoint interval adds only its own length.
    reserve(&mut db, "w3", "worker", 3000);
    state(&mut db, "w3", "active", 3000);
    apply(&mut db, |tx| {
        execution::finish_known(
            tx,
            "w3",
            150,
            Stamp {
                epoch: "first",
                ms: 9000,
            },
        )
    })
    .unwrap();
    let used: i64 = db
        .query_row(
            "SELECT used_ms FROM execution_controls WHERE run='r'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(used, 550);
}

#[test]
fn known_completion_after_restart_preserves_recorded_maximum() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Connection::open(dir.path().join("state.sqlite")).unwrap();
    store::migrate(&mut db).unwrap();
    seed(&db, "r", 5000);
    reserve(&mut db, "w1", "worker", 0);
    state(&mut db, "w1", "active", 0);
    db.execute(
        "UPDATE execution_controls SET used_ms=750 WHERE run='r'",
        [],
    )
    .unwrap();
    // New process epoch: the old start stamp is not comparable, so charge nothing extra.
    let next = Stamp {
        epoch: "second",
        ms: 50,
    };
    execution::recover_clock(&db, next).unwrap();
    apply(&mut db, |tx| execution::finish_known(tx, "w1", 200, next)).unwrap();
    let used: i64 = db
        .query_row(
            "SELECT used_ms FROM execution_controls WHERE run='r'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(used, 750, "recorded elapsed time never decreases");
}
