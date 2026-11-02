//! Durable execution identities, admission and shared per-run active-union budgets.
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub fn migrate(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS execution_controls (
      run TEXT PRIMARY KEY REFERENCES fake_runs(id), project TEXT NOT NULL,
      allowance_ms INTEGER NOT NULL CHECK(allowance_ms>0), used_ms INTEGER NOT NULL DEFAULT 0 CHECK(used_ms>=0),
      stop INTEGER NOT NULL DEFAULT 0, uncertain INTEGER NOT NULL DEFAULT 0,
      repairs INTEGER NOT NULL DEFAULT 0 CHECK(repairs BETWEEN 0 AND 3), replans INTEGER NOT NULL DEFAULT 0 CHECK(replans BETWEEN 0 AND 3),
      repository TEXT, milestone TEXT, human INTEGER NOT NULL DEFAULT 0);
      CREATE TABLE IF NOT EXISTS execution_policy (id INTEGER PRIMARY KEY CHECK(id=1), slots INTEGER NOT NULL, project_cap INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS project_limits(project TEXT PRIMARY KEY, cap INTEGER NOT NULL CHECK(cap BETWEEN 1 AND 3));
      CREATE TABLE IF NOT EXISTS execution_identities(token TEXT PRIMARY KEY,run TEXT NOT NULL REFERENCES fake_runs(id),project TEXT NOT NULL,role TEXT NOT NULL CHECK(role IN ('worker','coordinator')),state TEXT NOT NULL CHECK(state IN ('reserved','active','waiting','uncertain','finished')),receipt_path TEXT,snapshot_hash TEXT,baseline_ms INTEGER NOT NULL DEFAULT 0);
      CREATE TABLE IF NOT EXISTS union_clock(run TEXT PRIMARY KEY REFERENCES fake_runs(id),epoch TEXT NOT NULL,last_ms INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS handback_transfers(request_id TEXT PRIMARY KEY REFERENCES supervisor_requests(id),operation TEXT NOT NULL UNIQUE,repository TEXT NOT NULL,pending INTEGER NOT NULL CHECK(pending IN (0,1)));
      INSERT OR IGNORE INTO execution_policy VALUES(1,3,3);")
}
pub fn recover(db: &Connection) -> Result<(), String> {
    // Authoritative completions reconcile first. Classify only unresolved
    // execution intervals; PID values and elapsed timers never prove completion.
    db.execute_batch(
        "BEGIN IMMEDIATE;
      INSERT INTO supervisor_events(run,payload)
      SELECT f.id,json_object('kind','recovery_attention','reason','unresolved execution identity; slot retained; allowance conservatively exhausted') FROM fake_runs f
      WHERE NOT EXISTS(SELECT 1 FROM execution_controls c WHERE c.run=f.id AND c.uncertain=1)
      AND (EXISTS(SELECT 1 FROM execution_identities i WHERE i.run=f.id AND i.state IN ('reserved','active','uncertain'))
           OR (f.state IN ('queued','running') AND NOT EXISTS(SELECT 1 FROM execution_identities i WHERE i.run=f.id)));
      INSERT OR IGNORE INTO execution_controls(run,project,allowance_ms,used_ms,stop,uncertain)
      SELECT id,'historical',7200000,7200000,1,1 FROM fake_runs WHERE state IN ('queued','running');
      UPDATE execution_controls SET uncertain=1,stop=1,used_ms=max(used_ms,allowance_ms)
      WHERE EXISTS(SELECT 1 FROM execution_identities i WHERE i.run=execution_controls.run AND i.state IN ('reserved','active','uncertain'))
         OR (run IN (SELECT id FROM fake_runs WHERE state IN ('queued','running')) AND NOT EXISTS(SELECT 1 FROM execution_identities i WHERE i.run=execution_controls.run));
      UPDATE execution_identities SET state='uncertain' WHERE state IN ('reserved','active'); COMMIT;",
    )
    .map_err(|e| e.to_string())
}
pub fn show(db: &Connection, run: &str) -> Result<Value, String> {
    db.query_row("SELECT project,allowance_ms,used_ms,stop,uncertain,repairs,replans,human FROM execution_controls WHERE run=?1", [run], |r| Ok(json!({"project":r.get::<_,String>(0)?,"allowance_ms":r.get::<_,i64>(1)?,"used_ms":r.get::<_,i64>(2)?,"stop":r.get::<_,bool>(3)?,"identity_unknown":r.get::<_,bool>(4)?,"repairs":r.get::<_,u32>(5)?,"replans":r.get::<_,u32>(6)?,"human_owned":r.get::<_,bool>(7)?,"usage":{"kind":"unknown"}}))).map_err(|e|e.to_string())
}
pub fn admit(db: &Connection, project: &str) -> Result<(), String> {
    let (slots, cap): (u32, u32) = db
        .query_row("SELECT slots,project_cap FROM execution_policy", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|e| e.to_string())?;
    let cap = db
        .query_row(
            "SELECT coalesce((SELECT cap FROM project_limits WHERE project=?1),?2)",
            params![project, cap],
            |r| r.get::<_, u32>(0),
        )
        .map_err(|e| e.to_string())?;
    let (all,local): (u32,u32) = db.query_row("SELECT count(*),coalesce(sum(CASE WHEN project=?1 THEN 1 ELSE 0 END),0) FROM execution_identities WHERE role='worker' AND state IN ('reserved','active','uncertain')", [project], |r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
    let historical:u32=db.query_row("SELECT count(*) FROM fake_runs f WHERE state IN ('queued','running') AND EXISTS(SELECT 1 FROM execution_controls c WHERE c.run=f.id AND c.uncertain=1) AND NOT EXISTS(SELECT 1 FROM execution_identities i WHERE i.run=f.id)",[],|r|r.get(0)).map_err(|e|e.to_string())?;
    let local_history:u32=db.query_row("SELECT count(*) FROM fake_runs f JOIN execution_controls c ON c.run=f.id WHERE f.state IN ('queued','running') AND c.uncertain=1 AND (c.project=?1 OR c.project='historical') AND NOT EXISTS(SELECT 1 FROM execution_identities i WHERE i.run=f.id)",[project],|r|r.get(0)).map_err(|e|e.to_string())?;
    let all = all + historical;
    let local = local + local_history;
    if all >= slots || local >= cap {
        return Err("execution capacity exhausted; unresolved identities retain slots".into());
    }
    Ok(())
}
/// Live stamps use one monotonic clock/epoch per service process; tests can supply exact stamps.
#[derive(Clone, Copy)]
pub struct Stamp<'a> {
    pub epoch: &'a str,
    pub ms: i64,
}
pub fn now() -> Stamp<'static> {
    static CLOCK: std::sync::OnceLock<(String, std::time::Instant)> = std::sync::OnceLock::new();
    let clock = CLOCK.get_or_init(|| {
        (
            nonce().expect("execution epoch entropy"),
            std::time::Instant::now(),
        )
    });
    Stamp {
        epoch: &clock.0,
        ms: clock.1.elapsed().as_millis().min(i64::MAX as u128) as i64,
    }
}
pub fn nonce() -> Result<String, String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub fn tick_at(db: &Connection, run: &str, stamp: Stamp<'_>) -> Result<bool, String> {
    use rusqlite::OptionalExtension;
    let prior: Option<(String, i64)> = db
        .query_row(
            "SELECT epoch,last_ms FROM union_clock WHERE run=?1",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let active:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM execution_identities WHERE run=?1 AND state IN ('active','uncertain'))",[run],|r|r.get(0)).map_err(|e|e.to_string())?;
    if let Some((epoch, last)) = prior {
        if epoch != stamp.epoch {
            return Err("clock epoch requires explicit recovery".into());
        }
        if stamp.ms < last {
            return Err("monotonic clock moved backward".into());
        }
        if active {
            db.execute("UPDATE execution_controls SET used_ms=min(9223372036854775807,used_ms+?2) WHERE run=?1",params![run,stamp.ms-last]).map_err(|e|e.to_string())?;
        }
    }
    db.execute("INSERT INTO union_clock VALUES(?1,?2,?3) ON CONFLICT(run) DO UPDATE SET epoch=excluded.epoch,last_ms=excluded.last_ms",params![run,stamp.epoch,stamp.ms]).map_err(|e|e.to_string())?;
    db.query_row(
        "SELECT stop OR uncertain OR human OR used_ms>=allowance_ms FROM execution_controls WHERE run=?1",
        [run],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}
/// Caller holds the supervisor transaction; intent and identity are one commit.
pub fn reserve_at(
    db: &Connection,
    run: &str,
    project: &str,
    token: &str,
    role: &str,
    stamp: Stamp<'_>,
) -> Result<(), String> {
    let recorded: String = db
        .query_row(
            "SELECT project FROM execution_controls WHERE run=?1",
            [run],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if recorded != project {
        return Err("execution project differs from run ownership".into());
    }
    if tick_at(db, run, stamp)? {
        return Err("execution budget exhausted or stopped".into());
    }
    if role == "worker" {
        admit(db, project)?;
    } else if role == "coordinator" {
        let active:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM execution_identities WHERE run=?1 AND role='coordinator' AND state IN ('reserved','active','uncertain'))",[run],|r|r.get(0)).map_err(|e|e.to_string())?;
        if active {
            return Err("one coordinator per run".into());
        }
    } else {
        return Err("unknown execution role".into());
    }
    db.execute("INSERT INTO execution_identities(token,run,project,role,state,baseline_ms) SELECT ?1,?2,?3,?4,'reserved',used_ms FROM execution_controls WHERE run=?2",params![token,run,project,role]).map_err(|e|e.to_string())?;
    Ok(())
}
pub fn transition_at(
    db: &Connection,
    token: &str,
    state: &str,
    stamp: Stamp<'_>,
) -> Result<(), String> {
    let (run, project, role, old): (String, String, String, String) = db
        .query_row(
            "SELECT run,project,role,state FROM execution_identities WHERE token=?1",
            [token],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|e| e.to_string())?;
    let permitted = old == state
        || matches!(
            (old.as_str(), state),
            ("reserved", "active" | "waiting" | "finished" | "uncertain")
                | ("active", "waiting" | "finished" | "uncertain")
                | ("waiting", "active" | "finished" | "uncertain")
                | ("uncertain", "finished")
        );
    if !permitted {
        return Err("invalid execution identity transition".into());
    }
    let stop = tick_at(db, &run, stamp)?;
    if state == "active" && stop {
        return Err("execution budget exhausted or stopped".into());
    }
    if state == "active" && old == "waiting" {
        if role == "worker" {
            admit(db, &project)?;
        } else {
            let other:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM execution_identities WHERE run=?1 AND role='coordinator' AND token!=?2 AND state IN ('reserved','active','uncertain'))",params![run,token],|r|r.get(0)).map_err(|e|e.to_string())?;
            if other {
                return Err("one coordinator per run".into());
            }
        }
    }
    db.execute(
        "UPDATE execution_identities SET state=?2 WHERE token=?1",
        params![token, state],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
pub fn recover_clock(db: &Connection, stamp: Stamp<'_>) -> Result<(), String> {
    db.execute(
        "UPDATE union_clock SET epoch=?1,last_ms=?2",
        params![stamp.epoch, stamp.ms],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
pub fn checkpoint(db: &Connection, run: &str, _elapsed_ms: u64) -> Result<bool, String> {
    tick_at(db, run, now())
}
