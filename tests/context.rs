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

fn minimal_manifest(root: &std::path::Path) -> Value {
    let source = root.join("input.md");
    fs::write(&source, "small input").unwrap();
    let input = json!({"path":source,"source_revision":"rev-1"});
    json!({"version":1,"roots":[root],"approved":input,"brief":input,
        "instructions":[],"dependencies":[],"roles":{},"overrides":{}})
}

#[cfg(unix)]
#[test]
fn fifo_inputs_fail_promptly_for_manifest_artifact_and_retrieval() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let data = root.join("data");
    let fifo = root.join("fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let mut manifest = minimal_manifest(&root);
    manifest["approved"]["path"] = json!(fifo);
    let manifest_path = root.join("manifest.json");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let id = "a".repeat(64);
    fs::create_dir_all(data.join("context")).unwrap();
    assert!(
        Command::new("mkfifo")
            .arg(data.join("context").join(format!("{id}.json")))
            .status()
            .unwrap()
            .success()
    );
    for args in [
        vec!["create", fifo.to_str().unwrap()],
        vec!["create", manifest_path.to_str().unwrap()],
        vec!["show", &id],
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_agentpm"))
            .args(["context", "--data-dir"])
            .arg(&data)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("nonregular input blocked the command");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("regular file"));
    }
}

#[test]
fn library_metadata_limits_reject_before_publication() {
    use agentpm::context::{ContextStore, Manifest};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let store_path = root.join("not-created");
    let store = ContextStore::new(store_path.clone());
    let mut manifest = minimal_manifest(&root);
    manifest["roles"] =
        json!({"implementer":{"backend":"codex","model":"x".repeat(9*1024*1024),"skills":[]}});
    let large: Manifest = serde_json::from_value(manifest.clone()).unwrap();
    assert!(
        store
            .create(large)
            .unwrap_err()
            .contains("manifest exceeds")
    );
    assert!(!store_path.exists());
    manifest["roles"]["implementer"]["model"] = json!("normal");
    let small: Manifest = serde_json::from_value(manifest).unwrap();
    let id = store.create(small.clone()).unwrap();
    assert!(store.show(&id).is_ok());
    let mut huge = small;
    huge.roles.get_mut("implementer").unwrap().model = "x".repeat(9 * 1024 * 1024);
    assert!(store.update(&id, &id, huge).is_err());
    assert_eq!(fs::read_dir(&store_path).unwrap().count(), 1);
    assert!(store.show(&id).is_ok());
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

#[cfg(unix)]
#[test]
fn context_first_private_directory_supports_supervisor() {
    use std::os::unix::{fs::PermissionsExt, process::CommandExt};
    use std::time::{Duration, Instant};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let data = root.join("fresh/data");
    let manifest = root.join("manifest.json");
    fs::write(
        &manifest,
        serde_json::to_vec(&minimal_manifest(&root)).unwrap(),
    )
    .unwrap();
    let mut create = Command::new(env!("CARGO_BIN_EXE_agentpm"));
    create
        .args(["context", "--data-dir"])
        .arg(&data)
        .arg("create")
        .arg(&manifest);
    unsafe {
        create.pre_exec(|| {
            libc::umask(0o022);
            Ok(())
        });
    }
    let revision = success(create.output().unwrap())["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        fs::metadata(&data).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_agentpm"))
        .arg("--data-dir")
        .arg(&data)
        .args(["supervisor", "serve"])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let ready = loop {
        if agentpm::supervisor::client(
            &data,
            agentpm::supervisor::Request {
                version: 1,
                id: "status".into(),
                command: agentpm::supervisor::Command::Status {},
            },
        )
        .is_ok()
        {
            break true;
        }
        if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let retrieved = cli(&data, &["show", &revision]);
    let _ = child.kill();
    child.wait().unwrap();
    assert!(ready, "supervisor must accept context-created directory");
    success(retrieved);
}
