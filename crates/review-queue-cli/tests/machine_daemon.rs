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
        UnixSocketTransport, materialize_snapshot, materialize_snapshot_file, reproduce_snapshot,
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
    git(&repository, &["add", "file.txt"]);
    git(&repository, &["commit", "-m", "initial"]);
    fs::write(repository.join("file.txt"), "new\n").unwrap();

    let brief = ReviewBrief {
        title: "Remote fixture".into(),
        what: "A shipped-daemon compatibility fixture.".into(),
        why: String::new(),
        approach_alternatives: String::new(),
        testing: String::new(),
    };
    let manifest = capture(&CaptureRequest {
        workspace_root: repository.clone(),
        topic: "fixture".into(),
        brief: brief.clone(),
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
            brief,
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
    let snapshot = client
        .fetch_snapshot(&item.source_item_id, &item.snapshot_version, Utc::now())
        .unwrap();
    assert_eq!(snapshot.files.len(), 1);
    assert!(snapshot.files[0].base_content_base64.is_some());
    assert!(snapshot.files[0].head_content_base64.is_some());
    assert_eq!(
        materialize_snapshot(&snapshot).repositories[0].files.len(),
        1
    );
    let file = materialize_snapshot_file(
        &snapshot,
        &snapshot.files[0].repository_id,
        &snapshot.files[0].workspace_relative_path,
        "right",
    )
    .unwrap();
    assert_eq!(file.content.as_deref(), Some("new\n"));
    let reproduction = temporary.path().join("reproduction");
    reproduce_snapshot(&snapshot, &reproduction).unwrap();
    assert_eq!(
        fs::read_to_string(reproduction.join("file.txt")).unwrap(),
        "new\n"
    );

    daemon.kill().unwrap();
    daemon.wait().unwrap();
}
