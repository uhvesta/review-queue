use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use chrono::Utc;
use review_queue_core::{
    AgentRoute, Collection, ReviewBrief, Submission,
    capture::{CaptureRequest, capture},
    machine::{
        MACHINE_PROTOCOL_VERSION, MachineClient, MachineConfig, MachineEndpoint, MachineSourceType,
        UnixSocketTransport, materialize_snapshot, materialize_snapshot_file,
        preview_cached_git_reproduction, reproduce_cached_git_snapshot,
    },
    store::Store,
};

fn git(root: &std::path::Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

fn git_output(root: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn shipped_daemon_serves_machine_protocol_and_complete_reviewer_snapshot() {
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("source");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init"]);
    git(
        &repository,
        &["config", "user.email", "review@example.test"],
    );
    git(&repository, &["config", "user.name", "Review Test"]);
    fs::write(repository.join("file.txt"), "old\n").unwrap();
    fs::write(
        repository.join("unchanged.txt"),
        "present at the pinned HEAD\n",
    )
    .unwrap();
    fs::write(repository.join("deleted.txt"), "removed by the review\n").unwrap();
    fs::write(repository.join("binary.bin"), [0_u8, 159, 146, 150, 255]).unwrap();
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "initial"]);
    fs::write(repository.join("file.txt"), "new\n").unwrap();
    fs::remove_file(repository.join("deleted.txt")).unwrap();
    fs::write(
        repository.join("binary.bin"),
        [255_u8, 0, 254, 1, 253, 2, 252],
    )
    .unwrap();
    fs::write(repository.join("added.txt"), "newly added\n").unwrap();

    let brief = ReviewBrief {
        title: "Remote fixture".into(),
        what: "A shipped-daemon compatibility fixture.".into(),
        why: "Reviewers must receive the canonical author context.".into(),
        approach_alternatives: "A live checkout was rejected in favor of immutable cache data."
            .into(),
        testing: "Exercise text, unchanged, deleted, and binary files.".into(),
    };
    let manifest = capture(&CaptureRequest {
        workspace_root: repository.clone(),
        topic: "fixture".into(),
        brief: brief.clone(),
        origin_route_id: None,
        participating_repository_ids: Vec::new(),
        preflight_token: None,
    })
    .unwrap();
    let database = temporary.path().join("daemon.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let route = AgentRoute {
        id: "route-fixture".into(),
        adapter_kind: "acp".into(),
        agent_id: "fixture-agent".into(),
        endpoint: Some("tcp://127.0.0.1:7777".into()),
        session_id: Some("session-fixture".into()),
        status: "idle".into(),
        last_heartbeat: Utc::now(),
        provenance: None,
    };
    store
        .submit(Submission {
            collection: Collection::Local,
            topic_identity: "fixture-workspace:fixture".into(),
            brief: brief.clone(),
            manifest,
            origin_route: Some(route.clone()),
            source_metadata: None,
        })
        .unwrap();
    drop(store);

    let runtime = temporary.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = runtime.join("daemon.sock");
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_review-queue"))
        .args(["daemon", "--db"])
        .arg(&database)
        .arg("--socket")
        .arg(&socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(socket.exists(), "daemon did not create its socket");

    let config = MachineConfig {
        name: "Fixture daemon".into(),
        endpoint: MachineEndpoint::Loopback {
            socket_path: socket.to_string_lossy().into_owned(),
        },
        source_type: MachineSourceType::ReviewQueueDaemon,
    };
    let mut client = MachineClient::new(config, UnixSocketTransport::new(&socket)).unwrap();
    let health = client.fetch_health(Utc::now()).unwrap();
    assert_eq!(health.protocol_version, MACHINE_PROTOCOL_VERSION);
    let index = client.fetch_index(Utc::now()).unwrap();
    assert_eq!(index.items.len(), 1);
    let item = &index.items[0];
    let detail = client
        .fetch_item_detail(&item.source_item_id, Utc::now())
        .unwrap();
    assert_eq!(detail.origin_route, Some(route));
    assert_eq!(detail.brief, brief);
    let snapshot = client
        .fetch_snapshot(&item.source_item_id, &item.snapshot_version, Utc::now())
        .unwrap();
    assert_eq!(snapshot.files.len(), 4);
    assert_eq!(snapshot.repository_packs.len(), 1);
    let materialized = materialize_snapshot(&snapshot);
    assert_eq!(materialized.repositories[0].files.len(), 4);
    let modified = snapshot
        .files
        .iter()
        .find(|file| file.workspace_relative_path == "file.txt")
        .unwrap();
    assert!(modified.base_content_base64.is_some());
    assert!(modified.head_content_base64.is_some());
    let file = materialize_snapshot_file(
        &snapshot,
        &modified.repository_id,
        &modified.workspace_relative_path,
        "right",
    )
    .unwrap();
    assert_eq!(file.content.as_deref(), Some("new\n"));

    daemon.kill().unwrap();
    daemon.wait().unwrap();
    let original_source = repository.to_string_lossy().into_owned();
    fs::rename(&repository, temporary.path().join("source-unavailable")).unwrap();
    assert!(!repository.exists());

    let reproduction = temporary.path().join("reproduction");
    let preview = preview_cached_git_reproduction(&snapshot, &reproduction).unwrap();
    assert!(!preview.command_bundle.contains(&original_source));
    assert!(
        preview
            .repositories
            .iter()
            .all(|repository| repository.source.starts_with("cached-machine-git-pack:"))
    );
    reproduce_cached_git_snapshot(&snapshot, &reproduction).unwrap();
    let saved_head = &snapshot.manifest.repositories[0].head_sha;
    assert_eq!(
        git_output(&reproduction, &["rev-parse", "HEAD"]),
        *saved_head
    );
    assert_eq!(git_output(&reproduction, &["status", "--porcelain"]), "");
    assert!(
        !Command::new("git")
            .arg("-C")
            .arg(&reproduction)
            .args(["symbolic-ref", "--quiet", "HEAD"])
            .status()
            .unwrap()
            .success(),
        "reproduction must be detached"
    );
    assert_eq!(
        fs::read_to_string(reproduction.join("file.txt")).unwrap(),
        "new\n"
    );
    assert_eq!(
        fs::read_to_string(reproduction.join("unchanged.txt")).unwrap(),
        "present at the pinned HEAD\n"
    );
    assert!(!reproduction.join("deleted.txt").exists());
    assert_eq!(
        fs::read(reproduction.join("binary.bin")).unwrap(),
        [255_u8, 0, 254, 1, 253, 2, 252]
    );
    assert_eq!(
        fs::read_to_string(reproduction.join("added.txt")).unwrap(),
        "newly added\n"
    );
}
