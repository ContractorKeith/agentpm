//! Durable controls for the current one-attempt fake-run supervisor seam.
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
      INSERT OR IGNORE INTO execution_policy VALUES(1,3,3);")
}
pub fn recover(db: &Connection) -> Result<(), String> {
    // No adapter can reconnect yet. Never trust a persisted numeric PID, nor
    // release its slot. Charge the unknown interval conservatively in full.
    db.execute_batch(
        "BEGIN IMMEDIATE;
      INSERT INTO supervisor_events(run,payload)
      SELECT id,json_object('kind','recovery_attention','reason','execution identity cannot be reconnected; slot retained; allowance conservatively exhausted') FROM fake_runs f
      WHERE state IN ('queued','running') AND NOT EXISTS(SELECT 1 FROM execution_controls c WHERE c.run=f.id AND uncertain=1);
      INSERT OR IGNORE INTO execution_controls(run,project,allowance_ms,used_ms,stop,uncertain)
      SELECT id,'historical',7200000,7200000,1,1 FROM fake_runs WHERE state IN ('queued','running');
      UPDATE execution_controls SET uncertain=1,stop=1,used_ms=allowance_ms
      WHERE run IN (SELECT id FROM fake_runs WHERE state IN ('queued','running')); COMMIT;",
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
    let (all,local): (u32,u32) = db.query_row("SELECT count(*),coalesce(sum(CASE WHEN c.project=?1 THEN 1 ELSE 0 END),0) FROM fake_runs f LEFT JOIN execution_controls c ON c.run=f.id WHERE f.state IN ('queued','running')", [project], |r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
    if all >= slots || local >= cap {
        return Err("execution capacity exhausted; unresolved identities retain slots".into());
    }
    Ok(())
}
pub fn checkpoint(db: &Connection, run: &str, elapsed_ms: u64) -> Result<bool, String> {
    db.execute(
        "UPDATE execution_controls SET used_ms=max(used_ms,?2) WHERE run=?1",
        params![run, elapsed_ms.min(i64::MAX as u64) as i64],
    )
    .map_err(|e| e.to_string())?;
    db.query_row(
        "SELECT stop OR used_ms>=allowance_ms FROM execution_controls WHERE run=?1",
        [run],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}
