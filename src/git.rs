//! Durable, repository-local milestone claims. All Git arguments bypass a shell.
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

type Result<T> = std::result::Result<T, String>;

// Hooks and tools can export Git's repository/index/config selectors. Our explicit
// repository must own every operation, including update-ref's separate launcher.
fn git_command() -> Command {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command.env("GIT_TERMINAL_PROMPT", "0");
    command
}

#[derive(Debug, Serialize)]
pub struct Allocation {
    pub protocol_version: u32,
    pub repository: PathBuf,
    pub run: String,
    pub milestone: String,
    pub branch: String,
    pub path: PathBuf,
    pub expected_head: String,
    pub state: String,
}

fn git<I, S>(cwd: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git_command()
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
fn identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 80
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(
            "run and milestone must be 1-80 ASCII letters, digits, hyphens or underscores".into(),
        );
    }
    Ok(())
}

/// Open by any checkout or symlink. The Git common directory is the identity
/// and claim namespace, independent of caller-supplied data directories.
pub struct GitService {
    common: PathBuf,
}
impl GitService {
    pub fn open(repository: &Path) -> Result<Self> {
        let repository = repository.canonicalize().map_err(|e| e.to_string())?;
        let common = PathBuf::from(git(
            &repository,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
        Ok(Self { common })
    }
    fn command(&self, args: &[&str]) -> Result<String> {
        git(&self.common, args)
    }
    fn connection(&self) -> Result<Connection> {
        let db = Connection::open(self.common.join("agentpm-claims.sqlite3"))
            .map_err(|e| e.to_string())?;
        db.busy_timeout(Duration::from_secs(10))
            .map_err(|e| e.to_string())?;
        db.execute_batch("PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS allocations (run TEXT NOT NULL, milestone TEXT NOT NULL, branch TEXT NOT NULL UNIQUE, path TEXT NOT NULL UNIQUE, head TEXT NOT NULL, state TEXT NOT NULL, PRIMARY KEY(run,milestone));").map_err(|e| e.to_string())?;
        // BEGIN IMMEDIATE is held across external effects; persisted intents survive
        // crashes, while all cooperating CLI/supervisor instances serialize here.
        db.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| e.to_string())?;
        // Connection owns the lock until drop; writes below use explicit commits.
        Ok(db)
    }
    fn read(&self, db: &Connection, run: &str, milestone: &str) -> Result<Option<Allocation>> {
        db.query_row(
            "SELECT branch,path,head,state FROM allocations WHERE run=?1 AND milestone=?2",
            params![run, milestone],
            |r| {
                Ok(Allocation {
                    protocol_version: 1,
                    repository: self.common.clone(),
                    run: run.into(),
                    milestone: milestone.into(),
                    branch: r.get(0)?,
                    path: PathBuf::from(r.get::<_, String>(1)?),
                    expected_head: r.get(2)?,
                    state: r.get(3)?,
                })
            },
        )
        .optional()
        .map_err(|e| e.to_string())
    }
    fn marker(a: &Allocation) -> String {
        format!("refs/agentpm/owners/{}/{}", a.run, a.milestone)
    }
    fn head(&self, reference: &str) -> Option<String> {
        self.command(&["rev-parse", "--verify", reference]).ok()
    }
    fn owned(&self, a: &Allocation) -> Result<()> {
        if self.head(&Self::marker(a)).as_deref() != Some(&a.expected_head) {
            return Err("ownership ref is missing or changed; preserve allocation for manual reconciliation".into());
        }
        Ok(())
    }
    // Git's ref transaction couples branch creation/deletion to the ownership
    // marker. A crash cannot leave an indistinguishable unowned branch.
    fn refs(&self, script: &str) -> Result<()> {
        use std::io::Write;
        let mut child = git_command()
            .arg("--git-dir")
            .arg(&self.common)
            .args(["update-ref", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        child
            .stdin
            .take()
            .ok_or("missing Git stdin")?
            .write_all(script.as_bytes())
            .map_err(|e| e.to_string())?;
        let output = child.wait_with_output().map_err(|e| e.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).into_owned())
        }
    }
    fn checkout_matches(&self, a: &Allocation) -> Result<()> {
        if a.path
            .symlink_metadata()
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("worktree path is a symlink".into());
        }
        let common = PathBuf::from(git(
            &a.path,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
        let gitdir = PathBuf::from(git(&a.path, ["rev-parse", "--absolute-git-dir"])?)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if common != self.common
            || !gitdir.starts_with(self.common.join("worktrees"))
            || gitdir == self.common
            || git(&a.path, ["symbolic-ref", "HEAD"])? != format!("refs/heads/{}", a.branch)
        {
            return Err("worktree identity or branch changed; refusing mutation".into());
        }
        // Verify the Git administration entry points back to this exact checkout.
        let backlink = std::fs::read_to_string(gitdir.join("gitdir")).map_err(|e| e.to_string())?;
        if PathBuf::from(backlink.trim())
            .canonicalize()
            .map_err(|e| e.to_string())?
            != a.path
                .join(".git")
                .canonicalize()
                .map_err(|e| e.to_string())?
        {
            return Err("worktree registration mismatch".into());
        }
        Ok(())
    }
    fn unchanged_checkout(&self, a: &Allocation) -> Result<()> {
        // Status and non-force removal both trust these flags and can miss edits.
        let entries = git(&a.path, ["ls-files", "-v", "-z"])?;
        if entries
            .split('\0')
            .filter_map(|entry| entry.bytes().next())
            .any(|tag| tag.is_ascii_lowercase() || tag == b'S')
        {
            return Err(
                "worktree has assume-unchanged or skip-worktree entries; preserving it".into(),
            );
        }
        if git(&a.path, ["rev-parse", "HEAD"])? != a.expected_head
            || !git(
                &a.path,
                [
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=all",
                    "--ignored",
                ],
            )?
            .is_empty()
        {
            return Err("worktree has changes or unexpected commits; preserving it".into());
        }
        Ok(())
    }
    pub fn allocate(
        &self,
        run: &str,
        milestone: &str,
        branch: &str,
        base: &str,
    ) -> Result<Allocation> {
        identifier(run)?;
        identifier(milestone)?;
        self.command(&["check-ref-format", &format!("refs/heads/{branch}")])?;
        if branch.starts_with('-') {
            return Err("branch cannot start with '-'".into());
        }
        let head = self.command(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ])?;
        let db = self.connection()?;
        if let Some(a) = self.read(&db, run, milestone)? {
            if a.branch != branch || a.expected_head != head || a.state == "cleaned" {
                return Err("request conflicts with recorded allocation".into());
            }
        } else {
            let path = self
                .common
                .join("agentpm-worktrees")
                .join(run)
                .join(milestone);
            if path.exists() || self.head(&format!("refs/heads/{branch}")).is_some() {
                return Err("branch or path already exists and is unowned".into());
            }
            db.execute(
                "INSERT INTO allocations VALUES (?1,?2,?3,?4,?5,'allocating')",
                params![
                    run,
                    milestone,
                    branch,
                    path.to_str().ok_or("non-UTF8 worktree path")?,
                    head
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        // Commit intent before effects, then reacquire serialization. The durable
        // unique row protects the claim during this short lock handoff.
        db.execute_batch("COMMIT; BEGIN IMMEDIATE")
            .map_err(|e| e.to_string())?;
        let mut a = self
            .read(&db, run, milestone)?
            .ok_or("allocation disappeared")?;
        if a.state == "cleaned" || a.state == "cleaning" {
            return Err("allocation is cleaning or cleaned".into());
        }
        let marker = Self::marker(&a);
        if self.head(&marker).is_none() {
            self.refs(&format!(
                "start\ncreate refs/heads/{} {}\ncreate {} {}\nprepare\ncommit\n",
                a.branch, a.expected_head, marker, a.expected_head
            ))?;
        }
        self.owned(&a)?;
        if self.head(&format!("refs/heads/{}", a.branch)).as_deref() != Some(&a.expected_head) {
            return Err("allocation branch moved; preserving it for reconciliation".into());
        }
        if !a.path.exists() {
            if a.state != "allocating" {
                return Err("owned checkout disappeared; manual reconciliation required".into());
            }
            std::fs::create_dir_all(a.path.parent().ok_or("missing parent")?)
                .map_err(|e| e.to_string())?;
            git(
                &self.common,
                [
                    OsStr::new("worktree"),
                    OsStr::new("add"),
                    a.path.as_os_str(),
                    OsStr::new(&a.branch),
                ],
            )?;
        }
        self.checkout_matches(&a)?;
        if git(&a.path, ["rev-parse", "HEAD"])? != a.expected_head
            || self.head(&format!("refs/heads/{}", a.branch)).as_deref() != Some(&a.expected_head)
        {
            return Err("allocation revision changed; preserving it for reconciliation".into());
        }
        db.execute(
            "UPDATE allocations SET state='allocated' WHERE run=?1 AND milestone=?2",
            params![run, milestone],
        )
        .map_err(|e| e.to_string())?;
        db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        a.state = "allocated".into();
        Ok(a)
    }
    pub fn inspect(&self, run: &str, milestone: &str) -> Result<Allocation> {
        let db = self.connection()?;
        self.read(&db, run, milestone)?
            .ok_or("unknown allocation".into())
    }
    pub fn cleanup(&self, run: &str, milestone: &str) -> Result<Allocation> {
        let db = self.connection()?;
        let mut a = self
            .read(&db, run, milestone)?
            .ok_or("unknown allocation")?;
        if a.state == "cleaned" {
            return Ok(a);
        }
        if a.state == "cleaning"
            && !a.path.exists()
            && self.head(&Self::marker(&a)).is_none()
            && self.head(&format!("refs/heads/{}", a.branch)).is_none()
        {
            db.execute(
                "UPDATE allocations SET state='cleaned' WHERE run=?1 AND milestone=?2",
                params![run, milestone],
            )
            .map_err(|e| e.to_string())?;
            db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            a.state = "cleaned".into();
            return Ok(a);
        }
        // Persist cleanup intent before removal so a crash after worktree removal
        // can finish ref cleanup without guessing ownership from an absent path.
        self.owned(&a)?;
        if a.path.exists() {
            self.checkout_matches(&a)?;
            self.unchanged_checkout(&a)?;
        } else if a.state != "cleaning" {
            return Err("worktree missing without cleanup intent".into());
        }
        if self.head(&format!("refs/heads/{}", a.branch)).as_deref() != Some(&a.expected_head) {
            return Err("branch moved; preserving it".into());
        }
        db.execute(
            "UPDATE allocations SET state='cleaning' WHERE run=?1 AND milestone=?2",
            params![run, milestone],
        )
        .map_err(|e| e.to_string())?;
        db.execute_batch("COMMIT; BEGIN IMMEDIATE")
            .map_err(|e| e.to_string())?;
        if a.path.exists() {
            self.owned(&a)?;
            self.checkout_matches(&a)?;
            self.unchanged_checkout(&a)?;
            git(
                &self.common,
                [
                    OsStr::new("worktree"),
                    OsStr::new("remove"),
                    a.path.as_os_str(),
                ],
            )?;
        }
        let marker = Self::marker(&a);
        if self.head(&marker).is_some() {
            self.refs(&format!(
                "start\ndelete refs/heads/{} {}\ndelete {} {}\nprepare\ncommit\n",
                a.branch, a.expected_head, marker, a.expected_head
            ))?;
        } else if self.head(&format!("refs/heads/{}", a.branch)).is_some() {
            return Err("ownership lost; preserving branch".into());
        }
        db.execute(
            "UPDATE allocations SET state='cleaned' WHERE run=?1 AND milestone=?2",
            params![run, milestone],
        )
        .map_err(|e| e.to_string())?;
        db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        a.state = "cleaned".into();
        Ok(a)
    }
}
