use agentpm::git::GitService;
use std::{
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;
fn git(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn repo() -> TempDir {
    let t = tempfile::tempdir().unwrap();
    git(t.path(), &["init", "-b", "main"]);
    git(t.path(), &["config", "user.name", "Test"]);
    git(t.path(), &["config", "user.email", "test@example.test"]);
    std::fs::write(t.path().join("tracked"), "base").unwrap();
    git(t.path(), &["add", "."]);
    git(t.path(), &["commit", "-m", "initial"]);
    t
}

#[test]
fn cleanup_preserves_edits_hidden_by_index_flags() {
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        let t = repo();
        let service = GitService::open(t.path()).unwrap();
        let a = service.allocate("r", "m", "hidden-edit", "HEAD").unwrap();
        git(&a.path, &["update-index", flag, "tracked"]);
        std::fs::write(a.path.join("tracked"), "human edit").unwrap();
        assert!(git(&a.path, &["status", "--porcelain"]).is_empty());
        assert!(service.cleanup("r", "m").is_err());
        assert_eq!(
            std::fs::read_to_string(a.path.join("tracked")).unwrap(),
            "human edit"
        );
        assert_eq!(
            git(t.path(), &["rev-parse", "hidden-edit"]),
            a.expected_head
        );
    }
}

#[test]
fn inherited_git_environment_cannot_redirect_operations() {
    let a = repo();
    let b = repo();
    let b_before = git(b.path(), &["show-ref"]);
    let index = b.path().join(".git/index");
    let index_before = std::fs::read(&index).unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_agentpm"))
            .env("GIT_DIR", b.path().join(".git"))
            .env("GIT_WORK_TREE", b.path())
            .env("GIT_COMMON_DIR", b.path().join(".git"))
            .env("GIT_INDEX_FILE", &index)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.worktree")
            .env("GIT_CONFIG_VALUE_0", b.path())
            .args(["worktree", "--repo"])
            .arg(a.path())
            .args(args)
            .output()
            .unwrap()
    };
    let allocated = invoke(&["allocate", "r", "m", "right-repo"]);
    assert!(
        allocated.status.success(),
        "{}",
        String::from_utf8_lossy(&allocated.stderr)
    );
    assert!(a.path().join(".git/agentpm-claims.sqlite3").exists());
    assert!(!b.path().join(".git/agentpm-claims.sqlite3").exists());
    assert_eq!(git(b.path(), &["show-ref"]), b_before);
    assert_eq!(std::fs::read(&index).unwrap(), index_before);
    assert!(invoke(&["cleanup", "r", "m"]).status.success());
    assert_eq!(git(b.path(), &["show-ref"]), b_before);
}

#[test]
fn allocation_recovery_rejects_a_moved_branch() {
    let t = repo();
    let service = GitService::open(t.path()).unwrap();
    let a = service.allocate("r", "m", "moved", "HEAD").unwrap();
    git(t.path(), &["worktree", "remove", a.path.to_str().unwrap()]);
    let db = rusqlite::Connection::open(t.path().join(".git/agentpm-claims.sqlite3")).unwrap();
    db.execute("UPDATE allocations SET state='allocating'", [])
        .unwrap();
    git(t.path(), &["commit", "--allow-empty", "-m", "new revision"]);
    let new_head = git(t.path(), &["rev-parse", "HEAD"]);
    git(
        t.path(),
        &[
            "update-ref",
            "refs/heads/moved",
            &new_head,
            &a.expected_head,
        ],
    );
    assert!(
        service
            .allocate("r", "m", "moved", &a.expected_head)
            .is_err()
    );
    assert!(!a.path.exists());
    assert_eq!(git(t.path(), &["rev-parse", "moved"]), new_head);
    assert_eq!(service.inspect("r", "m").unwrap().state, "allocating");
}
fn cli(path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agentpm"))
        .args(["worktree", "--repo"])
        .arg(path)
        .args(args)
        .output()
        .unwrap()
}
#[test]
fn cli_allocates_retries_inspects_and_cleans_without_touching_checkout() {
    let t = repo();
    std::fs::write(t.path().join("user work"), "keep").unwrap();
    let before = git(t.path(), &["status", "--porcelain"]);
    let out = cli(t.path(), &["allocate", "run", "m1", "feature/one"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let retry = cli(t.path(), &["allocate", "run", "m1", "feature/one"]);
    assert_eq!(out.stdout, retry.stdout);
    assert_eq!(cli(t.path(), &["inspect", "run", "m1"]).stdout, out.stdout);
    let second = cli(t.path(), &["allocate", "run", "m2", "feature/two"]);
    assert!(second.status.success());
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_ne!(first["path"], second["path"]);
    assert_eq!(git(t.path(), &["symbolic-ref", "--short", "HEAD"]), "main");
    assert_eq!(git(t.path(), &["status", "--porcelain"]), before);
    assert!(cli(t.path(), &["cleanup", "run", "m1"]).status.success());
    assert!(!Path::new(first["path"].as_str().unwrap()).exists());
    assert!(cli(t.path(), &["cleanup", "run", "m1"]).status.success());
    assert!(t.path().join("user work").exists());
}
#[test]
fn dirty_untracked_ignored_commits_and_unowned_survive() {
    let t = repo();
    let s = GitService::open(t.path()).unwrap();
    let a = s.allocate("r", "m", "feature", "HEAD").unwrap();
    for (name, content) in [
        ("tracked", "dirty"),
        ("untracked", "new"),
        (".gitignore", "ignored\n"),
    ] {
        std::fs::write(a.path.join(name), content).unwrap();
        assert!(s.cleanup("r", "m").is_err());
        assert!(a.path.join(name).exists());
        if name == "tracked" {
            git(&a.path, &["restore", "tracked"]);
        } else {
            std::fs::remove_file(a.path.join(name)).unwrap();
        }
    }
    // An ignored file is data too.
    git(
        t.path(),
        &[
            "config",
            "core.excludesFile",
            t.path().join("excludes").to_str().unwrap(),
        ],
    );
    std::fs::write(t.path().join("excludes"), "ignored\n").unwrap();
    std::fs::write(a.path.join("ignored"), "keep").unwrap();
    assert!(s.cleanup("r", "m").is_err());
    std::fs::remove_file(a.path.join("ignored")).unwrap();
    git(&a.path, &["commit", "--allow-empty", "-m", "unexpected"]);
    assert!(s.cleanup("r", "m").is_err());
    assert!(a.path.exists());
    assert!(s.cleanup("unknown", "m").is_err());
    git(t.path(), &["branch", "user-branch"]);
    assert!(s.allocate("r2", "m", "user-branch", "HEAD").is_err());
    assert!(s.allocate("r2", "m", "feature", "HEAD").is_err());
    assert!(s.allocate("r", "m", "other", "HEAD").is_err());
}
#[test]
fn canonical_aliases_and_competing_processes_share_claims() {
    let t = repo();
    let aliases = tempfile::tempdir().unwrap();
    let alias = aliases.path().join("alias");
    std::os::unix::fs::symlink(t.path(), &alias).unwrap();
    let mut children = vec![];
    for n in 0..6 {
        children.push(
            Command::new(env!("CARGO_BIN_EXE_agentpm"))
                .args(["worktree", "--repo"])
                .arg(if n % 2 == 0 { t.path() } else { &alias })
                .args(["allocate", &format!("r{n}"), "m", "shared"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let winners = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .filter(|o| o.status.success())
        .count();
    assert_eq!(winners, 1);
    let s = GitService::open(&alias).unwrap();
    assert!(s.allocate("different", "m", "shared", "HEAD").is_err());
    // A linked checkout also resolves to the same common repository identity.
    let a = s.allocate("unique", "m", "unique", "HEAD").unwrap();
    assert!(
        GitService::open(&a.path)
            .unwrap()
            .allocate("another", "m", "unique", "HEAD")
            .is_err()
    );
}
#[test]
fn expected_sha_and_ownership_refs_preserve_replaced_branches() {
    let t = repo();
    let s = GitService::open(t.path()).unwrap();
    let a = s.allocate("r", "m", "owned", "HEAD").unwrap();
    git(
        t.path(),
        &[
            "update-ref",
            "-d",
            "refs/agentpm/owners/r/m",
            &a.expected_head,
        ],
    );
    assert!(s.cleanup("r", "m").is_err());
    assert!(a.path.exists());
    assert_eq!(a.expected_head, git(&a.path, &["rev-parse", "HEAD"]));
}
#[test]
fn argument_values_are_not_shell_programs() {
    let t = repo();
    let s = GitService::open(t.path()).unwrap();
    let branch = "feature/$(touch-pwned);literal";
    let a = s.allocate("r", "m", branch, "HEAD").unwrap();
    assert_eq!(git(&a.path, &["symbolic-ref", "--short", "HEAD"]), branch);
    assert!(s.allocate("x", "m", "--orphan", "HEAD").is_err());
    assert!(s.allocate("x", "m", "valid", "--help").is_err());
    s.cleanup("r", "m").unwrap();
}

#[test]
fn persisted_intents_reconcile_after_external_effects() {
    let t = repo();
    let s = GitService::open(t.path()).unwrap();
    let a = s.allocate("r", "m", "recover", "HEAD").unwrap();
    let db = rusqlite::Connection::open(t.path().join(".git/agentpm-claims.sqlite3")).unwrap();
    // Simulate restart after branch ownership transaction, before checkout add.
    git(t.path(), &["worktree", "remove", a.path.to_str().unwrap()]);
    db.execute("UPDATE allocations SET state='allocating'", [])
        .unwrap();
    let recovered = cli(t.path(), &["allocate", "r", "m", "recover"]);
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert!(a.path.exists());
    // Simulate restart after checkout removal, before atomic ref deletion.
    db.execute("UPDATE allocations SET state='cleaning'", [])
        .unwrap();
    git(t.path(), &["worktree", "remove", a.path.to_str().unwrap()]);
    assert!(cli(t.path(), &["cleanup", "r", "m"]).status.success());
    assert_eq!(s.inspect("r", "m").unwrap().state, "cleaned");
}
#[test]
fn checkout_replacement_and_moved_ref_are_preserved() {
    let t = repo();
    let s = GitService::open(t.path()).unwrap();
    let a = s.allocate("r", "m", "feature", "HEAD").unwrap();
    git(&a.path, &["checkout", "--detach"]);
    assert!(s.cleanup("r", "m").is_err());
    assert!(a.path.exists());
    git(t.path(), &["commit", "--allow-empty", "-m", "new main"]);
    let new_head = git(t.path(), &["rev-parse", "HEAD"]);
    git(
        t.path(),
        &[
            "update-ref",
            "refs/heads/feature",
            &new_head,
            &a.expected_head,
        ],
    );
    assert!(s.cleanup("r", "m").is_err());
    assert_eq!(git(t.path(), &["rev-parse", "feature"]), new_head);
}

#[test]
fn cleanup_receipt_reconciles_after_refs_are_deleted() {
    let t = repo();
    let s = GitService::open(t.path()).unwrap();
    let a = s.allocate("r", "m", "cleanup-receipt", "HEAD").unwrap();
    s.cleanup("r", "m").unwrap();
    let db = rusqlite::Connection::open(t.path().join(".git/agentpm-claims.sqlite3")).unwrap();
    db.execute("UPDATE allocations SET state='cleaning'", [])
        .unwrap();
    assert_eq!(s.cleanup("r", "m").unwrap().state, "cleaned");
    assert!(!a.path.exists());
}

#[test]
fn detached_or_unborn_worktrees_get_a_diagnosis_not_raw_git_stderr() {
    let t = repo();
    let service = GitService::open(t.path()).unwrap();
    let a = service.allocate("r", "m", "diag", "HEAD").unwrap();
    git(&a.path, &["checkout", "--detach"]);
    let e = service.execution_checkout("r", "m").unwrap_err();
    assert!(e.contains("detached") && !e.contains("fatal:"), "{e}");
    git(&a.path, &["checkout", "diag"]);
    service.execution_checkout("r", "m").unwrap();
    // The branch disappears underneath the checkout.
    git(t.path(), &["update-ref", "-d", "refs/heads/diag"]);
    for e in [
        service.execution_checkout("r", "m").unwrap_err(),
        service.cleanup("r", "m").unwrap_err(),
    ] {
        assert!(
            e.contains("branch was deleted") && !e.contains("fatal:"),
            "{e}"
        );
    }
}
