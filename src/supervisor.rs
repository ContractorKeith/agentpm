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
    Start { script: String },
    Show { run: String },
    Events { cursor: u64 },
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
    if matches!(request.command, Command::Start { .. }) {
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
    match request.command {
        Command::Status {} => Ok(json!({"version":1,"status":"running","pid":std::process::id()})),
        Command::Show { run } => conn.query_row("SELECT state,result,operation,pid,session,cwd,executable,created_at FROM fake_runs WHERE id=?1", [&run], |r| Ok(json!({"version":1,"run":run,"state":r.get::<_,String>(0)?,"result":r.get::<_,Option<String>>(1)?,"operation":r.get::<_,String>(2)?,"pid":r.get::<_,Option<u32>>(3)?,"session":r.get::<_,String>(4)?,"cwd":r.get::<_,String>(5)?,"executable":r.get::<_,String>(6)?,"created_at":r.get::<_,String>(7)?}))).map_err(error),
        Command::Events { cursor } => {
            let mut stmt = conn.prepare("SELECT sequence,run,payload FROM supervisor_events WHERE sequence>?1 ORDER BY sequence LIMIT 1").map_err(error)?;
            let rows = stmt.query_map([i64::try_from(cursor).map_err(|_| "cursor exceeds SQLite range")?], |r| Ok(json!({"cursor":r.get::<_,i64>(0)?,"run":r.get::<_,String>(1)?,"event":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(Value::Null)}))).map_err(error)?;
            Ok(json!({"version":1,"events":rows.collect::<Result<Vec<_>,_>>().map_err(error)?}))
        },
        Command::Start { script } => {
            if !Path::new(&script).is_absolute() { return Err("script must be an absolute path".into()); }
            let active: u32 = conn.query_row("SELECT count(*) FROM fake_runs WHERE state IN ('queued','running')", [], |r| r.get(0)).map_err(error)?;
            if active >= 3 { return Err("fake execution capacity is 3; uncertain runs require later recovery".into()); }
            let bytes = snapshot(&script)?;
            let run = request.id;
            let response = json!({"version":1,"run":run,"state":"queued"});
            let tx = conn.transaction().map_err(error)?;
            tx.execute("INSERT INTO fake_runs(id,state,result,operation,pid,session,cwd,executable,created_at,script_bytes) VALUES(?1,'queued',NULL,?1,NULL,?1,?2,?3,strftime('%Y-%m-%dT%H:%M:%fZ','now'),?4)", params![run,std::env::current_dir().map_err(error)?.to_string_lossy(),exe.to_string_lossy(),bytes]).map_err(error)?;
            event(&tx,&run,json!({"kind":"launch_intent","operation":run,"session":run}))?;
            tx.execute("INSERT INTO supervisor_requests VALUES(?1,?2,?3)",params![run,encoded,response.to_string()]).map_err(error)?;
            tx.commit().map_err(error)?;
            drop(conn);
            thread::spawn(move || {
                let execution = crate::process::launch_snapshot(&exe,&bytes,&run).and_then(|mut child| {
                    let receipt = || -> Result<(), String> { let mut conn = db.lock().map_err(error)?; let tx = conn.transaction().map_err(error)?;
                      tx.execute("UPDATE fake_runs SET state='running',pid=?2 WHERE id=?1",params![run,child.id()]).map_err(error)?;
                      event(&tx,&run,json!({"kind":"launched","pid":child.id(),"session":run}))?; tx.commit().map_err(error) };
                    if let Err(e) = receipt() { let _ = child.kill(); let _ = crate::process::finish(child); return Err(e); }
                    crate::process::finish(child)
                });
                let state = if execution.is_ok() { "finished" } else { "failed" };
                let result = execution.unwrap_or_else(|e| e);
                let persist = || -> Result<(),String> { let mut conn = db.lock().map_err(error)?; let tx = conn.transaction().map_err(error)?; tx.execute("UPDATE fake_runs SET state=?2,result=?3 WHERE id=?1",params![run,state,result]).map_err(error)?; event(&tx,&run,json!({"kind":state,"result":result}))?; tx.commit().map_err(error) };
                if let Err(e) = persist() { eprintln!("persist run {run}: {e}"); }
            });
            Ok(response)
        }
    }
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
