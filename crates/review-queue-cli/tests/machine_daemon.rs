use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

#[cfg(target_os = "macos")]
use std::{
    net::{TcpListener, TcpStream},
    os::unix::fs::FileTypeExt,
    path::Path,
    process::Child,
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
            source_adapter: None,
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

#[cfg(target_os = "macos")]
struct ProcessGuard {
    child: Child,
}

#[cfg(target_os = "macos")]
impl ProcessGuard {
    fn new(child: Child) -> Self {
        Self { child }
    }

    fn assert_running(&mut self, label: &str) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "{label} exited before the integration completed"
        );
    }
}

#[cfg(target_os = "macos")]
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(target_os = "macos")]
fn wait_for_socket(path: &Path, child: &mut ProcessGuard, label: &str) {
    for _ in 0..300 {
        if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket()) {
            return;
        }
        child.assert_running(label);
        thread::sleep(Duration::from_millis(10));
    }
    panic!("{label} did not create {}", path.display());
}

#[cfg(target_os = "macos")]
fn wait_for_tcp(port: u16, child: &mut ProcessGuard) {
    for _ in 0..300 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        child.assert_running("ephemeral sshd");
        thread::sleep(Duration::from_millis(10));
    }
    panic!("ephemeral sshd did not listen on port {port}");
}

#[cfg(target_os = "macos")]
fn checked_command(program: &str, arguments: &[&str]) {
    let output = Command::new(program).args(arguments).output().unwrap();
    assert!(
        output.status.success(),
        "{program} {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "macos")]
fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[test]
#[cfg(target_os = "macos")]
fn system_openssh_agent_tunnel_reaches_the_shipped_daemon_without_storing_credentials() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = temporary.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let repository = temporary.path().join("remote-source");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init"]);
    git(
        &repository,
        &["config", "user.email", "review@example.test"],
    );
    git(&repository, &["config", "user.name", "Review SSH Test"]);
    fs::write(repository.join("remote.txt"), "base\n").unwrap();
    git(&repository, &["add", "remote.txt"]);
    git(&repository, &["commit", "-m", "base"]);
    fs::write(repository.join("remote.txt"), "through ssh tunnel\n").unwrap();

    let manifest = capture(&CaptureRequest {
        workspace_root: repository.clone(),
        topic: "ssh-tunnel".into(),
        brief: ReviewBrief {
            title: "SSH tunnel fixture".into(),
            what: "Serve a real immutable review through the shipped daemon.".into(),
            why: "The desktop tunnel must honor the user's OpenSSH config and agent.".into(),
            approach_alternatives: "A loopback-only transport test cannot prove SSH behavior."
                .into(),
            testing: "Fetch health, index, detail, and snapshot over Unix forwarding.".into(),
        },
        origin_route_id: None,
        participating_repository_ids: Vec::new(),
        preflight_token: None,
    })
    .unwrap();
    let source_head = git_output(&repository, &["rev-parse", "HEAD"]);
    let source_status = git_output(
        &repository,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );

    let database = temporary.path().join("remote-daemon.sqlite3");
    let mut store = Store::open(&database).unwrap();
    store
        .submit(Submission {
            collection: Collection::Local,
            topic_identity: "ssh-tunnel-fixture".into(),
            brief: ReviewBrief {
                title: "SSH tunnel fixture".into(),
                what: "Serve a real immutable review through the shipped daemon.".into(),
                why: "The desktop tunnel must honor the user's OpenSSH config and agent.".into(),
                approach_alternatives: "A loopback-only transport test cannot prove SSH behavior."
                    .into(),
                testing: "Fetch health, index, detail, and snapshot over Unix forwarding.".into(),
            },
            manifest,
            origin_route: None,
            source_metadata: None,
            source_adapter: None,
        })
        .unwrap();
    drop(store);

    let remote_socket = runtime.join("remote-daemon.sock");
    let daemon = Command::new(env!("CARGO_BIN_EXE_review-queue"))
        .args(["daemon", "--db"])
        .arg(&database)
        .arg("--socket")
        .arg(&remote_socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut daemon = ProcessGuard::new(daemon);
    wait_for_socket(&remote_socket, &mut daemon, "shipped daemon");

    let host_key = temporary.path().join("sshd-host-key");
    let user_key = temporary.path().join("ssh-agent-key");
    checked_command(
        "/usr/bin/ssh-keygen",
        &[
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-f",
            host_key.to_str().unwrap(),
        ],
    );
    checked_command(
        "/usr/bin/ssh-keygen",
        &[
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-f",
            user_key.to_str().unwrap(),
        ],
    );
    let authorized_keys = temporary.path().join("authorized_keys");
    fs::copy(user_key.with_extension("pub"), &authorized_keys).unwrap();

    let agent_socket = runtime.join("ssh-agent.sock");
    let agent = Command::new("/usr/bin/ssh-agent")
        .args(["-D", "-a"])
        .arg(&agent_socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut agent = ProcessGuard::new(agent);
    wait_for_socket(&agent_socket, &mut agent, "ephemeral ssh-agent");
    let add_output = Command::new("/usr/bin/ssh-add")
        .arg(&user_key)
        .env("SSH_AUTH_SOCK", &agent_socket)
        .output()
        .unwrap();
    assert!(
        add_output.status.success(),
        "ssh-add failed: {}",
        String::from_utf8_lossy(&add_output.stderr)
    );

    let port = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let username = String::from_utf8(
        Command::new("/usr/bin/id")
            .arg("-un")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    let sshd_config = temporary.path().join("sshd_config");
    fs::write(
        &sshd_config,
        format!(
            "Port {port}\n\
             ListenAddress 127.0.0.1\n\
             AddressFamily inet\n\
             HostKey {}\n\
             PidFile {}\n\
             AuthorizedKeysFile {}\n\
             PasswordAuthentication no\n\
             KbdInteractiveAuthentication no\n\
             UsePAM no\n\
             PubkeyAuthentication yes\n\
             StrictModes no\n\
             AllowUsers {username}\n\
             LogLevel ERROR\n",
            host_key.display(),
            temporary.path().join("sshd.pid").display(),
            authorized_keys.display(),
        ),
    )
    .unwrap();
    let sshd = Command::new("/usr/sbin/sshd")
        .args(["-D", "-e", "-f"])
        .arg(&sshd_config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut sshd = ProcessGuard::new(sshd);
    wait_for_tcp(port, &mut sshd);

    let host_alias = "review-queue-ephemeral";
    let ssh_config = temporary.path().join("ssh_config");
    fs::write(
        &ssh_config,
        format!(
            "Host {host_alias}\n\
               HostName 127.0.0.1\n\
               Port {port}\n\
               User {username}\n\
               IdentityAgent {}\n\
               BatchMode yes\n\
               StrictHostKeyChecking no\n\
               UserKnownHostsFile /dev/null\n\
               LogLevel ERROR\n",
            agent_socket.display()
        ),
    )
    .unwrap();

    let local_socket = runtime.join("desktop-tunnel.sock");
    let forward = format!("{}:{}", local_socket.display(), remote_socket.display());
    let ssh = Command::new("/usr/bin/ssh")
        .arg("-F")
        .arg(&ssh_config)
        .args(["-N", "-o", "ExitOnForwardFailure=yes", "-L"])
        .arg(&forward)
        .arg(host_alias)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut ssh = ProcessGuard::new(ssh);
    wait_for_socket(&local_socket, &mut ssh, "OpenSSH Unix-socket tunnel");

    let config = MachineConfig {
        name: "Ephemeral SSH fixture".into(),
        endpoint: MachineEndpoint::Ssh {
            target: host_alias.into(),
            remote_socket: remote_socket.to_string_lossy().into_owned(),
            adapter: review_queue_core::machine::SshAdapter::SystemOpenSsh,
        },
        source_type: MachineSourceType::ReviewQueueDaemon,
    };
    let desktop_database = temporary.path().join("desktop.sqlite3");
    let desktop_store = Store::open(&desktop_database).unwrap();
    desktop_store.add_machine_config(&config).unwrap();
    drop(desktop_store);
    let persisted = fs::read(&desktop_database).unwrap();
    let private_key = fs::read(&user_key).unwrap();
    assert!(!bytes_contain(&persisted, &private_key));
    assert!(!bytes_contain(
        &persisted,
        agent_socket.to_string_lossy().as_bytes()
    ));
    assert!(!bytes_contain(
        &persisted,
        user_key.to_string_lossy().as_bytes()
    ));

    let mut client = MachineClient::new(config, UnixSocketTransport::new(&local_socket)).unwrap();
    let health = client.fetch_health(Utc::now()).unwrap();
    assert_eq!(health.protocol_version, MACHINE_PROTOCOL_VERSION);
    let index = client.fetch_index(Utc::now()).unwrap();
    assert_eq!(index.items.len(), 1);
    let item = &index.items[0];
    let detail = client
        .fetch_item_detail(&item.source_item_id, Utc::now())
        .unwrap();
    assert_eq!(detail.summary.snapshot_version, item.snapshot_version);
    let snapshot = client
        .fetch_snapshot(&item.source_item_id, &item.snapshot_version, Utc::now())
        .unwrap();
    assert_eq!(snapshot.source_item_id, item.source_item_id);
    assert_eq!(snapshot.files.len(), 1);
    assert_eq!(git_output(&repository, &["rev-parse", "HEAD"]), source_head);
    assert_eq!(
        git_output(
            &repository,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        source_status
    );
}
