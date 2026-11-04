//! Sole-writer local fake-run command service, separate from the M0 demo.
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
const LIMIT: u64 = 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub id: String,
    pub command: Command,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Start {
        script: String,
    },
    StartControlled {
        script: String,
        project: String,
        allowance_ms: u64,
        #[serde(default)]
        repository: Option<String>,
        #[serde(default)]
        milestone: Option<String>,
    },
    Takeover {
        run: String,
    },
    Handback {
        run: String,
    },
    Controls {
        run: String,
    },
    Cancel {
        run: String,
    },
    Policy {
        slots: u32,
        project_cap: u32,
    },
    ProjectCap {
        project: String,
        cap: u32,
    },
    Replan {
        run: String,
    },
    Repair {
        run: String,
    },
    Resume {
        run: String,
    },
    Extend {
        run: String,
        additional_ms: u64,
    },
    Show {
        run: String,
    },
    Events {
        cursor: u64,
    },
    Reconcile {
        run: String,
    },
    Status {},
}
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn open(dir: &Path) -> Result<Connection, String> {
    let mut db = Connection::open(dir.join("state.sqlite")).map_err(error)?;
    crate::store::migrate(&mut db).map_err(error)?;
    Ok(db)
}
fn event(db: &Connection, run: &str, payload: Value) -> Result<(), String> {
    db.execute(
        "INSERT INTO supervisor_events(run,payload) VALUES(?1,?2)",
        params![run, payload.to_string()],
    )
    .map_err(error)?;
    Ok(())
}
fn handle(db: Arc<Mutex<Connection>>, request: Request, exe: PathBuf) -> Result<Value, String> {
    if request.version != 1 {
        return Err("protocol mismatch: supervisor requires version 1; upgrade client".into());
    }
    crate::contracts::RequestId::new(&request.id)?;
    let encoded = serde_json::to_string(&request.command).map_err(error)?;
    let mut conn = db.lock().map_err(error)?;
    reconcile(&mut conn)?;
    reconcile_handbacks(&conn)?;
    if !matches!(
        request.command,
        Command::Status {}
            | Command::Show { .. }
            | Command::Controls { .. }
            | Command::Events { .. }
    ) {
        let previous = conn.query_row(
            "SELECT command,response FROM supervisor_requests WHERE id=?1",
            [&request.id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        );
        match previous {
            Ok((command, response)) => {
                if command != encoded {
                    return Err("command ID already used for different payload".into());
                }
                return serde_json::from_str(&response).map_err(error);
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(e) => return Err(error(e)),
        }
    }
    let (project, allowance) = match &request.command {
        Command::StartControlled {
            project,
            allowance_ms,
            ..
        } => {
            crate::contracts::ProjectId::new(project)?;
            if *allowance_ms == 0 || *allowance_ms > i64::MAX as u64 {
                return Err("invalid allowance".into());
            }
            (project.clone(), *allowance_ms)
        }
        _ => ("default".into(), 7_200_000),
    };
    let binding = match &request.command {
        Command::StartControlled {
            repository,
            milestone,
            ..
        } => match (repository, milestone) {
            (Some(repo), Some(milestone)) => {
                let service = crate::git::GitService::open(Path::new(repo))?;
                let a = service.execution_checkout(&request.id, milestone)?;
                Some((
                    a.repository.to_string_lossy().into_owned(),
                    milestone.clone(),
                    a,
                ))
            }
            (None, None) => None,
            _ => return Err("repository and milestone must be supplied together".into()),
        },
        _ => None,
    };
    let is_resume = matches!(request.command, Command::Resume { .. });
    let is_handback = matches!(request.command, Command::Handback { .. });
    match request.command {
        Command::Status {} => Ok(json!({"version":1,"status":"running","pid":std::process::id()})),
        Command::Show { run } => conn.query_row("SELECT state,result,operation,pid,session,cwd,executable,created_at FROM fake_runs WHERE id=?1", [&run], |r| Ok(json!({"version":1,"run":run,"state":r.get::<_,String>(0)?,"result":r.get::<_,Option<String>>(1)?,"operation":r.get::<_,String>(2)?,"pid":r.get::<_,Option<u32>>(3)?,"session":r.get::<_,String>(4)?,"cwd":r.get::<_,String>(5)?,"executable":r.get::<_,String>(6)?,"created_at":r.get::<_,String>(7)?}))).map_err(error),
        Command::Events { cursor } => {
            let mut stmt = conn.prepare("SELECT sequence,run,payload FROM supervisor_events WHERE sequence>?1 ORDER BY sequence LIMIT 1").map_err(error)?;
            let rows = stmt.query_map([i64::try_from(cursor).map_err(|_| "cursor exceeds SQLite range")?], |r| Ok(json!({"cursor":r.get::<_,i64>(0)?,"run":r.get::<_,String>(1)?,"event":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(Value::Null)}))).map_err(error)?;
            Ok(json!({"version":1,"events":rows.collect::<Result<Vec<_>,_>>().map_err(error)?}))
        },
        Command::Takeover { run } | Command::Handback { run } => {
            let (repository, milestone, uncertain): (Option<String>,Option<String>,bool) = conn.query_row("SELECT repository,milestone,uncertain FROM execution_controls WHERE run=?1",[&run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(error)?;
            if uncertain { return Err("unknown execution identity requires human reconciliation; exclusive ownership cannot be proven".into()); }
            let repository=repository.ok_or("run has no worktree binding")?;
            let service = crate::git::GitService::open(Path::new(&repository))?;
            let milestone = milestone.ok_or("run has no milestone binding")?;
            let handback = is_handback;
            if handback {
                let controls = crate::execution::show(&conn,&run)?;
                if controls["human_owned"] != true { return Err("explicit takeover required before handback".into()); }
            }
            if !handback {
                service.takeover(&run,&milestone)?;
                conn.execute("UPDATE execution_controls SET stop=1 WHERE run=?1",[&run]).map_err(error)?;
                // Release the store lock while the owner performs bounded shutdown.
                drop(conn);
                let deadline = Instant::now()+Duration::from_secs(2);
                loop {
                    let conn = db.lock().map_err(error)?;
                    let active: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM execution_identities WHERE run=?1 AND state!='finished') OR EXISTS(SELECT 1 FROM fake_runs f WHERE id=?1 AND state IN ('queued','running') AND NOT EXISTS(SELECT 1 FROM execution_identities i WHERE i.run=f.id))",[&run],|r|r.get(0)).map_err(error)?;
                    if !active { break; }
                    if Instant::now()>=deadline { return Err("takeover pending; maintenance frozen; worker shutdown unconfirmed".into()); }
                    drop(conn);
                    thread::sleep(Duration::from_millis(10));
                }
                conn = db.lock().map_err(error)?;
            }
            let report = if handback { service.prepare_handback(&handback_operation(&conn,&request.id)?,&run,&milestone)? } else { json!({"human_owned":true}) };
            let tx = conn.transaction().map_err(error)?;
            tx.execute("UPDATE execution_controls SET human=?2 WHERE run=?1",params![run,!handback]).map_err(error)?;
            event(&tx,&run,json!({"kind":if handback {"handback_evidence_invalidated"} else {"human_takeover"},"inspection":report}))?;
            tx.execute("INSERT INTO supervisor_requests VALUES(?1,?2,?3)",params![request.id,encoded,report.to_string()]).map_err(error)?;
            if handback {tx.execute("INSERT INTO handback_transfers VALUES(?1,?2,?3,1)",params![request.id,handback_operation(&tx,&request.id)?,repository]).map_err(error)?;}
            tx.commit().map_err(error)?;
            if handback && let Some((_, e)) = reconcile_handbacks(&conn)?.into_iter().find(|(r, _)| *r == request.id) { return Err(e); }
            Ok(report)
        },
        Command::Reconcile { run } => crate::execution::show(&conn,&run),
        Command::Controls { run } => crate::execution::show(&conn, &run),
        Command::Cancel { ref run } | Command::Replan { ref run } => {
            let tx = conn.transaction().map_err(error)?;
            crate::execution::show(&tx, run)?;
            let sql = match request.command {
                Command::Cancel { .. } => "UPDATE execution_controls SET stop=1 WHERE run=?1",
                Command::Replan { .. } => "UPDATE execution_controls SET replans=replans+1 WHERE run=?1 AND replans<3",
                _ => "UPDATE execution_controls SET repairs=repairs+1 WHERE run=?1 AND repairs<3",
            };
            if tx.execute(sql,[run]).map_err(error)? != 1 { return Err("autonomous attempt limit exhausted".into()); }
            let response = crate::execution::show(&tx,run)?;
            event(&tx,run,json!({"kind":"execution_control","controls":response}))?;
            tx.execute("INSERT INTO supervisor_requests VALUES(?1,?2,?3)",params![request.id,encoded,response.to_string()]).map_err(error)?;
            tx.commit().map_err(error)?;
            Ok(response)
        },
        Command::Extend { run, additional_ms } => {
            let active: bool = conn.query_row("SELECT state IN ('queued','running') FROM fake_runs WHERE id=?1",[&run],|r|r.get(0)).map_err(error)?;
            if active { return Err("stop execution before extending its allowance".into()); }
            if additional_ms == 0 || additional_ms>i64::MAX as u64 { return Err("invalid extension".into()); }
            let controls = crate::execution::show(&conn,&run)?;
            let allowance = controls["allowance_ms"].as_u64().ok_or("unknown allowance")?;
            let next = allowance.checked_add(additional_ms).filter(|v|*v<=i64::MAX as u64).ok_or("extension overflow")?;
            let tx = conn.transaction().map_err(error)?;
            tx.execute("UPDATE execution_controls SET allowance_ms=?2 WHERE run=?1",params![run,next as i64]).map_err(error)?;
            let response = crate::execution::show(&tx,&run)?;
            event(&tx,&run,json!({"kind":"human_budget_extension","additional_ms":additional_ms,"controls":response}))?;
            tx.execute("INSERT INTO supervisor_requests VALUES(?1,?2,?3)",params![request.id,encoded,response.to_string()]).map_err(error)?;
            tx.commit().map_err(error)?;
            Ok(response)
        },
        Command::Repair { run } | Command::Resume { run } => {
            let controls = crate::execution::show(&conn,&run)?;
            if (!is_resume && controls["stop"] == true) || controls["identity_unknown"] == true || controls["human_owned"] == true { return Err("run requires human reconciliation or extension".into()); }
            let project = controls["project"].as_str().ok_or("missing project")?;
            crate::execution::admit(&conn,project)?;
            let (state, bytes, cwd, executable): (String,Option<Vec<u8>>,String,String) = conn.query_row("SELECT state,script_bytes,cwd,executable FROM fake_runs WHERE id=?1",[&run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(error)?;
            if state != "failed" && state != "finished" { return Err("attempt is still active".into()); }
            let (repo,milestone): (Option<String>,Option<String>) = conn.query_row("SELECT repository,milestone FROM execution_controls WHERE run=?1",[&run],|r|Ok((r.get(0)?,r.get(1)?))).map_err(error)?;
            let binding=if let (Some(repo),Some(milestone))=(repo,milestone) { let service=crate::git::GitService::open(Path::new(&repo))?; let a=service.execution_checkout(&run,&milestone)?; service.attach(&conn)?; Some((service,a)) } else {None};
            let bytes = bytes.ok_or("historical input unknown; cannot replay")?;
            let used = controls["used_ms"].as_u64().ok_or("unknown used time")?;
            let allowance = controls["allowance_ms"].as_u64().ok_or("unknown allowance")?;
            if used>=allowance { return Err("execution budget exhausted".into()); }
            let token=crate::execution::nonce()?;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(error)?;
            if let Some((service,a))=&binding { service.reserve_attached(&tx,a,&token)?; }
            if !is_resume && tx.execute("UPDATE execution_controls SET repairs=repairs+1 WHERE run=?1 AND repairs<3",[&run]).map_err(error)? != 1 { return Err("repair limit exhausted".into()); }
            if is_resume { tx.execute("UPDATE execution_controls SET stop=0 WHERE run=?1",[&run]).map_err(error)?; }
            tx.execute("UPDATE fake_runs SET state='queued',pid=NULL,result=NULL,session=?2 WHERE id=?1",params![run,token]).map_err(error)?;
            register_attempt(&tx,&run,project,&token,&bytes)?;
            let response = crate::execution::show(&tx,&run)?;
            event(&tx,&run,json!({"kind":"repair_intent","controls":response,"execution":token,"human_resume":is_resume}))?;
            tx.execute("INSERT INTO supervisor_requests VALUES(?1,?2,?3)",params![request.id,encoded,response.to_string()]).map_err(error)?;
            tx.commit().map_err(error)?;
            drop(conn);
            dispatch(db,Dispatch {exe:PathBuf::from(executable),run,bytes,cwd:Some(PathBuf::from(cwd)),token});
            Ok(response)
        },
        Command::ProjectCap { project, cap } => {
            crate::contracts::ProjectId::new(&project)?;
            if cap==0 || cap>3 { return Err("project cap must be 1..3".into()); }
            let tx = conn.transaction().map_err(error)?;
            tx.execute("INSERT INTO project_limits VALUES(?1,?2) ON CONFLICT(project) DO UPDATE SET cap=excluded.cap",params![project,cap]).map_err(error)?;
            let response=json!({"project":project,"cap":cap});
            tx.execute("INSERT INTO supervisor_requests VALUES(?1,?2,?3)",params![request.id,encoded,response.to_string()]).map_err(error)?;
            tx.commit().map_err(error)?;
            Ok(response)
        },
        Command::Policy { slots, project_cap } => {
            if slots == 0 || slots > 3 || project_cap == 0 || project_cap > slots { return Err("limits must be 1 <= project cap <= slots <= 3".into()); }
            let tx = conn.transaction().map_err(error)?;
            tx.execute("UPDATE execution_policy SET slots=?1,project_cap=?2",params![slots,project_cap]).map_err(error)?;
            let response = json!({"slots":slots,"project_cap":project_cap});
            tx.execute("INSERT INTO supervisor_requests VALUES(?1,?2,?3)",params![request.id,encoded,response.to_string()]).map_err(error)?;
            tx.commit().map_err(error)?;
            Ok(response)
        },
        Command::Start { script } | Command::StartControlled { script, .. } => {
            if !Path::new(&script).is_absolute() { return Err("script must be an absolute path".into()); }
            crate::execution::admit(&conn, &project)?;
            let bytes = snapshot(&script)?;
            let run = request.id;
            let response = json!({"version":1,"run":run,"state":"queued"});
            let token=crate::execution::nonce()?;
            if let Some((repo,_,_))=&binding {crate::git::GitService::open(Path::new(repo))?.attach(&conn)?;}
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(error)?;
            if let Some((repo,_,a))=&binding { crate::git::GitService::open(Path::new(repo))?.reserve_attached(&tx,a,&token)?; }
            tx.execute("INSERT INTO fake_runs(id,state,result,operation,pid,session,cwd,executable,created_at,script_bytes) VALUES(?1,'queued',NULL,?1,NULL,?1,?2,?3,strftime('%Y-%m-%dT%H:%M:%fZ','now'),?4)", params![run,binding.as_ref().map(|b|b.2.path.clone()).unwrap_or(std::env::current_dir().map_err(error)?).to_string_lossy(),exe.to_string_lossy(),bytes]).map_err(error)?;
            tx.execute("INSERT INTO execution_controls(run,project,allowance_ms) VALUES(?1,?2,?3)",params![run,project,allowance as i64]).map_err(error)?;
            if let Some((repo,milestone,_)) = &binding {
                tx.execute("UPDATE execution_controls SET repository=?2,milestone=?3 WHERE run=?1",params![run,repo,milestone]).map_err(error)?;
            }
            tx.execute("UPDATE fake_runs SET session=?2 WHERE id=?1",params![run,token]).map_err(error)?;
            register_attempt(&tx,&run,&project,&token,&bytes)?;
            event(&tx,&run,json!({"kind":"launch_intent","operation":run,"session":token}))?;
            tx.execute("INSERT INTO supervisor_requests VALUES(?1,?2,?3)",params![run,encoded,response.to_string()]).map_err(error)?;
            tx.commit().map_err(error)?;
            drop(conn);
            dispatch(db,Dispatch {exe,run,bytes,cwd:binding.map(|b|b.2.path),token});
            Ok(response)
        }
    }
}
fn register_attempt(
    db: &Connection,
    run: &str,
    project: &str,
    token: &str,
    bytes: &[u8],
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    crate::execution::reserve_at(db, run, project, token, "worker", crate::execution::now())?;
    let database = Path::new(db.path().ok_or("missing supervisor database path")?)
        .canonicalize()
        .map_err(error)?;
    let path = database
        .parent()
        .ok_or("missing data directory")?
        .join("receipts")
        .join(format!("{token}.json"));
    db.execute(
        "UPDATE execution_identities SET receipt_path=?2,snapshot_hash=?3 WHERE token=?1",
        params![
            token,
            path.to_string_lossy(),
            format!("{:x}", Sha256::digest(bytes))
        ],
    )
    .map_err(error)?;
    Ok(())
}
/// Durable, deduplicated diagnostic for a run whose reconciliation could not proceed.
fn attention(db: &Connection, run: &str, reason: &str) {
    let payload = json!({"kind":"reconciliation_attention","reason":reason}).to_string();
    let last: Option<String> = db
        .query_row(
            "SELECT payload FROM supervisor_events WHERE run=?1 ORDER BY sequence DESC LIMIT 1",
            [run],
            |r| r.get(0),
        )
        .ok();
    if last.as_deref() != Some(payload.as_str()) {
        let _ = db.execute(
            "INSERT INTO supervisor_events(run,payload) VALUES(?1,?2)",
            params![run, payload],
        );
    }
}
/// Each run is recovered on its own: an unavailable repository keeps that run's
/// reservation and is retried on the next call, never failing unrelated work.
fn recover_reservations(conn: &mut Connection) -> Result<(), String> {
    let rows=conn.prepare("SELECT f.id,f.session,c.repository,c.milestone FROM fake_runs f JOIN execution_controls c ON c.run=f.id WHERE f.state IN ('queued','running') AND c.repository IS NOT NULL AND c.milestone IS NOT NULL").map_err(error)?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
    for (run, token, repo, milestone) in rows {
        let mut restored = || -> Result<(), String> {
            crate::git::GitService::open(Path::new(&repo))?.attach(conn)?;
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(error)?;
            tx.execute("INSERT OR IGNORE INTO repo_claims.execution_reservations(run,milestone,token) SELECT run,milestone,?3 FROM repo_claims.allocations WHERE run=?1 AND milestone=?2",params![run,milestone,token]).map_err(error)?;
            tx.commit().map_err(error)
        };
        if let Err(e) = restored() {
            attention(
                conn,
                &run,
                &format!("repository reservation not restored: {e}"),
            );
        }
    }
    Ok(())
}
fn handback_operation(db: &Connection, request: &str) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let path = Path::new(db.path().ok_or("missing supervisor database")?)
        .canonicalize()
        .map_err(error)?;
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(path, request)).map_err(error)?)
    ))
}
/// Returns each pending request that could not complete, with its cause; the caller
/// decides whether a failure concerns the command in hand.
fn reconcile_handbacks(db: &Connection) -> Result<Vec<(String, String)>, String> {
    let rows = db
        .prepare("SELECT h.request_id,h.operation,h.repository,coalesce(json_extract(q.command,'$.run'),'') FROM handback_transfers h JOIN supervisor_requests q ON q.id=h.request_id WHERE h.pending=1")
        .map_err(error)?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)?;
    let mut failed = Vec::new();
    for (request, operation, repo, run) in rows {
        let done = || -> Result<(), String> {
            crate::git::GitService::open(Path::new(&repo))?
                .complete_handback(&operation, &request, db)?;
            db.execute(
                "UPDATE handback_transfers SET pending=0 WHERE request_id=?1",
                [&request],
            )
            .map_err(error)?;
            Ok(())
        };
        // The pending row (and the repository's freeze) persists for the next retry.
        if let Err(e) = done() {
            attention(db, &run, &format!("handback completion pending: {e}"));
            failed.push((request, e));
        }
    }
    Ok(failed)
}
/// Receipts are guardian-owned, nonce/hash-bound, and only published after cleanup.
/// A run whose receipt cannot be applied (e.g. unavailable repository) is skipped and
/// retried; its token is returned so recovery does not classify it as unknown.
fn reconcile(conn: &mut Connection) -> Result<Vec<String>, String> {
    let rows=conn.prepare("SELECT i.token,i.run,i.receipt_path,i.snapshot_hash,i.baseline_ms,c.repository FROM execution_identities i JOIN execution_controls c ON c.run=i.run WHERE i.state!='finished' AND i.receipt_path IS NOT NULL").map_err(error)?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,Option<String>>(5)?))).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
    let (mut deferred, mut settled, mut blocked) = (Vec::new(), Vec::new(), Vec::new());
    for (token, run, path, hash, baseline, repo) in rows {
        let receipt = match crate::receipt::Receipt::read(Path::new(&path), &token, &hash) {
            Ok(Some(r)) if r.complete => r,
            Ok(_) => continue,
            Err(e) => {
                attention(conn, &run, &format!("guardian receipt unusable: {e}"));
                continue;
            }
        };
        if let Err(e) = apply_receipt(conn, &token, &run, baseline, repo.as_deref(), receipt) {
            attention(
                conn,
                &run,
                &format!("completed execution not reconciled: {e}"),
            );
            deferred.push(token);
            blocked.push(run);
        } else {
            settled.push(run);
        }
    }
    // Known intervals are applied first; only then does the union advance to now.
    settled.sort();
    settled.dedup();
    for run in settled.iter().filter(|r| !blocked.contains(r)) {
        if let Ok(tx) = conn.transaction() {
            let _ = crate::execution::tick_at(&tx, run, crate::execution::now());
            let _ = tx.commit();
        }
    }
    Ok(deferred)
}
fn apply_receipt(
    conn: &mut Connection,
    token: &str,
    run: &str,
    baseline: i64,
    repo: Option<&str>,
    receipt: crate::receipt::Receipt,
) -> Result<(), String> {
    if let Some(repo) = repo {
        crate::git::GitService::open(Path::new(repo))?.attach(conn)?;
    }
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    crate::execution::finish_known(&tx, token, receipt.elapsed_ms, crate::execution::now())?;
    tx.execute("UPDATE execution_controls SET used_ms=max(used_ms,?2),uncertain=EXISTS(SELECT 1 FROM execution_identities WHERE run=?1 AND state='uncertain') WHERE run=?1",params![run,baseline.saturating_add(receipt.elapsed_ms.min(i64::MAX as u64) as i64)]).map_err(error)?;
    let state = if receipt.result.is_ok() {
        "finished"
    } else {
        "failed"
    };
    let result = receipt.result.unwrap_or_else(|e| e);
    tx.execute(
        "UPDATE fake_runs SET state=?2,result=?3 WHERE id=?1 AND session=?4",
        params![run, state, result, token],
    )
    .map_err(error)?;
    if repo.is_some() {
        tx.execute(
            "DELETE FROM repo_claims.execution_reservations WHERE token=?1",
            [token],
        )
        .map_err(error)?;
    }
    event(
        &tx,
        run,
        json!({"kind":state,"result":result,"execution":token}),
    )?;
    tx.commit().map_err(error)
}
fn finish_unlaunched(
    conn: &mut Connection,
    run: &str,
    token: &str,
    reason: &str,
) -> Result<(), String> {
    let repo: Option<String> = conn
        .query_row(
            "SELECT repository FROM execution_controls WHERE run=?1",
            [run],
            |r| r.get(0),
        )
        .map_err(error)?;
    if let Some(repo) = &repo {
        crate::git::GitService::open(Path::new(repo))?.attach(conn)?;
    }
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    crate::execution::transition_at(&tx, token, "finished", crate::execution::now())?;
    if repo.is_some() {
        tx.execute(
            "DELETE FROM repo_claims.execution_reservations WHERE token=?1",
            [token],
        )
        .map_err(error)?;
    }
    tx.execute(
        "UPDATE fake_runs SET state='failed',result=?2 WHERE id=?1 AND session=?3",
        params![run, reason, token],
    )
    .map_err(error)?;
    event(
        &tx,
        run,
        json!({"kind":"not_launched","execution":token,"reason":reason}),
    )?;
    tx.commit().map_err(error)
}
struct Dispatch {
    exe: PathBuf,
    run: String,
    bytes: Vec<u8>,
    cwd: Option<PathBuf>,
    token: String,
}
fn dispatch(db: Arc<Mutex<Connection>>, attempt: Dispatch) {
    let Dispatch {
        exe,
        run,
        bytes,
        cwd,
        token,
    } = attempt;
    thread::spawn(move || {
        let mut spawned = false;
        let mut launch = || -> Result<(), String> {
            let (session, path, remaining) = {
                let mut conn = db.lock().map_err(error)?;
                let session = token.clone();
                let path: String = conn
                    .query_row(
                        "SELECT receipt_path FROM execution_identities WHERE token=?1",
                        [&session],
                        |r| r.get(0),
                    )
                    .map_err(error)?;
                let tx = conn.transaction().map_err(error)?;
                crate::execution::transition_at(&tx, &session, "active", crate::execution::now())?;
                let remaining: i64 = tx
                    .query_row(
                        "SELECT max(0,allowance_ms-used_ms) FROM execution_controls WHERE run=?1",
                        [&run],
                        |r| r.get(0),
                    )
                    .map_err(error)?;
                tx.commit().map_err(error)?;
                (session, path, remaining as u64)
            };
            let child = crate::process::launch_guard(
                &exe,
                &session,
                cwd.as_deref(),
                remaining,
                &bytes,
                Some(Path::new(&path)),
            )?;
            spawned = true;
            let receipt = || -> Result<(), String> {
                let mut conn = db.lock().map_err(error)?;
                let tx = conn.transaction().map_err(error)?;
                tx.execute(
                    "UPDATE fake_runs SET state='running',pid=?2 WHERE id=?1",
                    params![run, child.id()],
                )
                .map_err(error)?;
                event(
                    &tx,
                    &run,
                    json!({"kind":"launched","pid":child.id(),"session":session}),
                )?;
                tx.commit().map_err(error)
            };
            if let Err(e) = receipt() {
                let _ = crate::process::finish_guard(child, &[], Duration::from_secs(2), || {
                    Path::new(&path).with_extension("ready").exists()
                });
                return Err(e);
            }
            let result = crate::process::finish_guard(
                child,
                &bytes,
                Duration::from_millis(remaining).saturating_add(Duration::from_secs(2)),
                || {
                    if !Path::new(&path).with_extension("ready").exists() {
                        return false;
                    }
                    db.lock()
                        .ok()
                        .and_then(|mut conn| {
                            let tx = conn.transaction().ok()?;
                            let stop = crate::execution::checkpoint(&tx, &run, 0).ok()?;
                            tx.commit().ok()?;
                            Some(stop)
                        })
                        .unwrap_or(true)
                },
            );
            let mut conn = db.lock().map_err(error)?;
            reconcile(&mut conn)?;
            result.map(|_| ())
        };
        if let Err(e) = launch() {
            eprintln!("execution {run}: {e}");
            if !spawned && let Ok(mut conn) = db.lock() {
                let _ = finish_unlaunched(&mut conn, &run, &token, &e);
            }
        }
        // Any path without authoritative completion remains reserved/uncertain.
        if let Ok(mut conn) = db.lock() {
            let deferred = reconcile(&mut conn).unwrap_or_default();
            if let Ok(tx) = conn.transaction() {
                let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM execution_identities WHERE token=?1 AND state!='finished')",[&token],|r|r.get(0)).unwrap_or(true);
                // A complete receipt awaiting its repository is known, not unknown.
                if pending && !deferred.contains(&token) {
                    let _ = crate::execution::transition_at(
                        &tx,
                        &token,
                        "uncertain",
                        crate::execution::now(),
                    );
                    let _=tx.execute("UPDATE execution_controls SET uncertain=1,stop=1,used_ms=max(used_ms,allowance_ms) WHERE run=?1",[&run]);
                }
                let _ = tx.commit();
            }
        }
    });
}
fn snapshot(path: &str) -> Result<Vec<u8>, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("snapshot {path}: {e}"))?;
    if !file.metadata().map_err(error)?.is_file() {
        return Err("snapshot requires a regular script file".into());
    }
    let mut bytes = Vec::new();
    file.take(crate::harness::MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() as u64 > crate::harness::MAX_BYTES {
        return Err("script snapshot exceeds 64 KiB".into());
    }
    Ok(bytes)
}
fn remaining(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| "absolute IPC frame deadline exceeded".into())
}
fn read(stream: &mut UnixStream, deadline: Instant) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    // Read bounded chunks without buffering past this single frame.
    let mut buf = [0; 4096];
    loop {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(error)?;
        let n = stream.read(&mut buf).map_err(error)?;
        if n == 0 {
            return Err("missing frame newline".into());
        }
        bytes.extend_from_slice(&buf[..n]);
        if bytes.len() as u64 > LIMIT {
            return Err("IPC frame exceeds limit".into());
        }
        if bytes.contains(&b'\n') {
            return Ok(bytes);
        }
    }
}
fn write(stream: &mut UnixStream, value: &Value, deadline: Instant) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(error)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > LIMIT {
        return Err("IPC frame exceeds limit".into());
    }
    let mut offset = 0;
    while offset < bytes.len() {
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(error)?;
        let n = stream.write(&bytes[offset..]).map_err(error)?;
        if n == 0 {
            return Err("closed IPC writer".into());
        }
        offset += n;
    }
    Ok(())
}
pub fn client(dir: &Path, request: Request) -> Result<Value, String> {
    let mut stream = UnixStream::connect(dir.join("supervisor.sock"))
        .map_err(|e| format!("connect supervisor: {e}; run supervisor start"))?;
    write(
        &mut stream,
        &serde_json::to_value(request).map_err(error)?,
        Instant::now() + Duration::from_secs(5),
    )?;
    let value: Value =
        serde_json::from_slice(&read(&mut stream, Instant::now() + Duration::from_secs(5))?)
            .map_err(error)?;
    if let Some(e) = value.get("error") {
        return Err(e.to_string());
    }
    Ok(value)
}
pub fn serve(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::fs::OpenOptionsExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(error)?;
    let meta = fs::symlink_metadata(dir).map_err(error)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
        return Err("data directory must be a private directory (chmod 700)".into());
    }
    let lock: File = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("supervisor.lock"))
        .map_err(error)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("supervisor already running (single-instance lock held)".into());
    }
    // All service-created files are private, including SQLite journals.
    unsafe {
        libc::umask(0o077);
    }
    let socket = dir.join("supervisor.sock");
    if socket.exists() {
        fs::remove_file(&socket).map_err(error)?;
    }
    let listener = UnixListener::bind(&socket).map_err(error)?;
    let db = Arc::new(Mutex::new(open(dir)?));
    fs::create_dir_all(dir.join("receipts")).map_err(error)?;
    {
        let mut conn = db.lock().map_err(error)?;
        crate::execution::recover_clock(&conn, crate::execution::now())?;
        let deferred = reconcile(&mut conn)?;
        crate::execution::recover(&conn, &deferred)?;
        recover_reservations(&mut conn)?;
        reconcile_handbacks(&conn)?;
    }
    let exe = std::env::current_exe()
        .map_err(error)?
        .with_file_name("fake-agent");
    // Bounded synchronous client handling; child execution uses separate threads.
    for stream in listener.incoming() {
        let mut stream = stream.map_err(error)?;
        let result = read(&mut stream, Instant::now() + Duration::from_secs(2))
            .and_then(|b| serde_json::from_slice(&b).map_err(error))
            .and_then(|r| handle(db.clone(), r, exe.clone()));
        let value = result.unwrap_or_else(|e| json!({"version":1,"error":e}));
        let _ = write(&mut stream, &value, Instant::now() + Duration::from_secs(2));
    }
    drop(lock);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocked_writer_respects_absolute_frame_deadline() {
        let (mut writer, _reader) = UnixStream::pair().unwrap();
        let size: libc::c_int = 1024;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    writer.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&size as *const libc::c_int).cast(),
                    std::mem::size_of_val(&size) as libc::socklen_t,
                )
            },
            0
        );
        let start = Instant::now();
        assert!(
            write(
                &mut writer,
                &json!({"data":"x".repeat(500_000)}),
                start + Duration::from_millis(150)
            )
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
