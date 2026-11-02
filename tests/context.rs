use serde_json::{Value, json};
use std::{fs, process::Command};
fn cli(data: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_agentpm"))
        .args(["context", "--data-dir"])
        .arg(data)
        .args(args)
        .output()
        .unwrap()
}
fn success(out: std::process::Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
#[test]
fn immutable_context_command_round_trip_and_guards() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let data = root.join("data");
    let source = root.join("spec.md");
    fs::write(&source, "approved original").unwrap();
    let input = json!({"path": source, "source_revision": "approved-1"});
    let mut manifest = json!({"version":1,"roots":[root],"approved":input,"brief":input,"instructions":[input],"dependencies":[input],"roles":{"implementer":{"backend":"codex","model":"original","skills":[input]}},"overrides":{"implementer":{"backend":"codex","model":"selected","skills":[input]}}});
    let path = root.join("manifest.json");
    let write = |v: &Value| fs::write(&path, serde_json::to_vec(v).unwrap()).unwrap();
    write(&manifest);
    let created = success(cli(&data, &["create", path.to_str().unwrap()]));
    let id = created["revision"].as_str().unwrap();
    assert_eq!(
        created,
        success(cli(&data, &["create", path.to_str().unwrap()]))
    );
    fs::write(&source, "changed").unwrap();
    let old = success(cli(&data, &["show", id]));
    assert_eq!(
        old["package"]["artifacts"][0]["content"],
        "approved original"
    );
    assert_eq!(
        old["package"]["selected_roles"]["implementer"]["model"],
        "selected"
    );
    assert!(
        !cli(
            &data,
            &[
                "update",
                id,
                "--acknowledge",
                "wrong",
                path.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    let updated = success(cli(
        &data,
        &["update", id, "--acknowledge", id, path.to_str().unwrap()],
    ));
    let new = success(cli(&data, &["show", updated["revision"].as_str().unwrap()]));
    assert_ne!(created, updated);
    assert_eq!(new["package"]["previous"], id);
    assert_eq!(new["package"]["artifacts"][0]["content"], "changed");
    assert_eq!(old, success(cli(&data, &["show", id])));
    assert_eq!(fs::read_to_string(&source).unwrap(), "changed");
    for bad in [
        root.join("missing"),
        root.join(".env"),
        std::path::PathBuf::from("/etc/hosts"),
    ] {
        manifest["approved"]["path"] = json!(bad);
        write(&manifest);
        let out = cli(&data, &["create", path.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
    }
    manifest["approved"] = input.clone();
    #[cfg(unix)]
    {
        let link = root.join("link");
        std::os::unix::fs::symlink("/etc/hosts", &link).unwrap();
        manifest["approved"]["path"] = json!(link);
        write(&manifest);
        assert!(
            !cli(&data, &["create", path.to_str().unwrap()])
                .status
                .success()
        );
    }
    manifest["approved"] = input;
    fs::write(&source, vec![b'a'; 1024 * 1024 + 1]).unwrap();
    write(&manifest);
    assert!(
        !cli(&data, &["create", path.to_str().unwrap()])
            .status
            .success()
    );
    fs::write(data.join("context").join(format!("{id}.json")), "{}").unwrap();
    assert!(!cli(&data, &["show", id]).status.success());
}
