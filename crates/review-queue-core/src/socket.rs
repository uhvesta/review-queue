//! The CLI protocol is deliberately small and credential-free.
//!
//! A desktop host owns the socket and passes a `Store` to this module. The
//! allowed commands are queue reads/enqueues/configuration only; operations
//! with external effects are rejected before they reach the store.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::store::{Store, SubmissionResult};
use crate::{
    ActionableError, AgentRoute, Collection, DomainError, capture::CaptureRequest,
    machine::MachineConfig,
};

/// A local CLI request is deliberately small. This prevents a peer from making
/// the desktop process allocate an unbounded line before JSON is parsed.
pub const MAX_SOCKET_REQUEST_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SocketRequest {
    List {
        collection: Option<Collection>,
        #[serde(default)]
        include_old: bool,
    },
    GetRound {
        id: String,
    },
    /// Read-only discovery for the exact subsequent capture request.
    PreflightCapture {
        request: CaptureRequest,
    },
    /// Desktop-owned atomic Git capture and SQLite ingestion.
    CaptureLocal {
        request: CaptureRequest,
    },
    AddMachine {
        config: MachineConfig,
    },
    ListMachines,
    RemoveMachine {
        id_or_name: String,
    },
    PrAdd {
        url: String,
    },
    Diagnose,
    AgentRegister {
        route: Box<AgentRoute>,
    },
    AgentHeartbeat {
        route_id: String,
        status: String,
    },
    /// Explicitly represented so rejection is stable and audit-friendly.
    Deliver {
        round_id: String,
    },
    Publish {
        round_id: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SocketResponse {
    Ok {
        data: serde_json::Value,
    },
    Error {
        error: ActionableError,
    },
    /// Internal first half of atomic local capture. `request()` acknowledges
    /// this frame and returns only the final post-commit response to callers.
    CapturePrepared {
        transaction_id: String,
        data: serde_json::Value,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct CaptureCommitAck {
    #[serde(rename = "type")]
    kind: String,
    transaction_id: String,
}

pub fn dispatch(store: &mut Store, request: SocketRequest) -> SocketResponse {
    let result = match request {
        SocketRequest::List {
            collection,
            include_old,
        } => store.list(collection, include_old).and_then(json),
        SocketRequest::GetRound { id } => store.round(&id).and_then(json),
        SocketRequest::PreflightCapture { request } => {
            store.preflight_local_capture(&request).and_then(json)
        }
        SocketRequest::CaptureLocal { request } => store
            .ingest_local_capture(&request)
            .and_then(|result| json(submission_result(result))),
        SocketRequest::AddMachine { config } => {
            store
                .add_machine_config(&config)
                .and_then(|(machine, created)| {
                    json(serde_json::json!({"machine": machine, "created": created}))
                })
        }
        SocketRequest::ListMachines => store.machines().and_then(json),
        SocketRequest::RemoveMachine { id_or_name } => store
            .remove_machine(&id_or_name)
            .and_then(|(id, removed)| json(serde_json::json!({"id": id, "removed": removed}))),
        SocketRequest::PrAdd { url } => {
            let parsed = crate::github::parse_pull_request_url(&url);
            parsed.and_then(|_| {
                Err(DomainError::actionable(
                    "The PR read capability is not connected.",
                    "No pull request was fetched or added, and no GitHub write occurred.",
                    "Open Review Queue Settings, connect PR read, then retry this command.",
                    "pr_read_capability_required",
                ))
            })
        }
        SocketRequest::Diagnose => json(serde_json::json!({
            "data_plane": "reachable",
            "credential_boundary": "desktop_only",
            "socket_operations": "read_enqueue_config_only"
        })),
        SocketRequest::AgentRegister { route } => store
            .register_route(route.as_ref())
            .and_then(|_| json(serde_json::json!({"id": route.id}))),
        SocketRequest::AgentHeartbeat { route_id, status } => store
            .heartbeat(&route_id, &status)
            .and_then(|_| json(serde_json::json!({"id": route_id}))),
        SocketRequest::Deliver { .. } | SocketRequest::Publish { .. } => {
            Err(DomainError::actionable(
                "The local CLI socket cannot deliver feedback or publish a review.",
                "No feedback was sent and no remote service was changed.",
                "Open the review in the desktop app and confirm the action there.",
                "socket_operation_forbidden",
            ))
        }
    };
    match result {
        Ok(data) => SocketResponse::Ok { data },
        Err(error) => SocketResponse::Error { error: error.error },
    }
}

fn json(value: impl Serialize) -> Result<serde_json::Value, DomainError> {
    serde_json::to_value(value).map_err(|e| {
        DomainError::actionable(
            format!("Could not serialize the local response: {e}"),
            "No review state changed.",
            "Retry the action.",
            "socket_encode_error",
        )
    })
}
fn submission_result(result: SubmissionResult) -> serde_json::Value {
    match result {
        SubmissionResult::Existing(round) => {
            serde_json::json!({"outcome":"existing", "round": round})
        }
        SubmissionResult::Created(round) => {
            serde_json::json!({"outcome":"created", "round": round})
        }
        SubmissionResult::Superseded { old_id, round } => {
            serde_json::json!({"outcome":"superseded", "old_id": old_id, "round": round})
        }
    }
}

/// Serve the standalone token-free daemon or the desktop-owned local socket.
///
/// The socket's direct parent must be an existing private (0700) directory
/// owned by this user. An existing endpoint is removed only when it is a
/// user-owned Unix-domain socket; files, directories, and symlinks are never
/// removed as "stale" socket paths.
pub fn serve(path: impl AsRef<Path>, store: Arc<Mutex<Store>>) -> anyhow::Result<()> {
    serve_inner(path.as_ref(), store, None)
}

pub type PrAddHandler = dyn Fn(String) -> SocketResponse + Send + Sync + 'static;

/// Desktop-owned socket variant. The handler runs only for the read/enqueue
/// `PrAdd` operation, allowing the signed host to use its Keychain capability
/// without ever passing a credential through this protocol.
pub fn serve_with_pr_handler(
    path: impl AsRef<Path>,
    store: Arc<Mutex<Store>>,
    pr_add: Arc<PrAddHandler>,
) -> anyhow::Result<()> {
    serve_inner(path.as_ref(), store, Some(pr_add))
}

fn serve_inner(
    path: &Path,
    store: Arc<Mutex<Store>>,
    pr_add: Option<Arc<PrAddHandler>>,
) -> anyhow::Result<()> {
    let listener = bind_listener(path)?;
    for connection in listener.incoming() {
        let stream = connection?;
        if let Err(error) = verify_peer_uid(&stream) {
            // A peer that is not this user must never reach request
            // parsing or the store. Dropping the stream is sufficient.
            eprintln!("Review Queue rejected local socket peer: {error:#}");
            continue;
        }
        let store = Arc::clone(&store);
        let pr_add = pr_add.clone();
        std::thread::spawn(move || {
            let _ = handle_stream(stream, store, pr_add);
        });
    }
    Ok(())
}

fn bind_listener(path: &Path) -> anyhow::Result<UnixListener> {
    validate_socket_path(path)?;
    remove_stale_socket(path)?;
    let listener = UnixListener::bind(path)?;
    if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
        // Do not leave a socket whose visibility is broader than the protocol promises.
        drop(listener);
        let _ = fs::remove_file(path);
        anyhow::bail!(
            "SOCKET_PERMISSION_FAILED: could not set mode 0600 on '{}': {error}",
            path.display()
        );
    }
    Ok(listener)
}

fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

fn parent_dir(path: &Path) -> anyhow::Result<&Path> {
    path.parent().filter(|parent| !parent.as_os_str().is_empty()).ok_or_else(|| {
        anyhow::anyhow!(
            "SOCKET_PATH_INVALID: socket path '{}' has no parent directory; use an absolute path or './name'",
            path.display()
        )
    })
}

fn validate_socket_path(path: &Path) -> anyhow::Result<()> {
    let parent = parent_dir(path)?;
    let metadata = fs::symlink_metadata(parent).map_err(|error| {
        anyhow::anyhow!(
            "SOCKET_PARENT_UNAVAILABLE: cannot inspect '{}': {error}. Create a private directory (chmod 700) owned by the current user.",
            parent.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!(
            "SOCKET_PARENT_UNSAFE: '{}' must be a real directory, not a symlink or non-directory",
            parent.display()
        );
    }
    let mode = metadata.permissions().mode() & 0o777;
    if metadata.uid() != current_uid() || mode != 0o700 {
        anyhow::bail!(
            "SOCKET_PARENT_UNSAFE: '{}' must be owned by uid {} and have mode 0700 (found uid {}, mode {:o})",
            parent.display(),
            current_uid(),
            metadata.uid(),
            mode
        );
    }
    Ok(())
}

fn remove_stale_socket(path: &Path) -> anyhow::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).map_err(|error| {
                anyhow::anyhow!(
                    "SOCKET_ENDPOINT_UNAVAILABLE: cannot inspect '{}': {error}",
                    path.display()
                )
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        anyhow::bail!(
            "SOCKET_ENDPOINT_UNSAFE: '{}' already exists but is not a Unix socket owned by this user; it was not removed",
            path.display()
        );
    }
    if metadata.uid() != current_uid() {
        anyhow::bail!(
            "SOCKET_ENDPOINT_UNSAFE: '{}' is owned by uid {}, not uid {}; it was not removed",
            path.display(),
            metadata.uid(),
            current_uid()
        );
    }
    fs::remove_file(path).map_err(|error| {
        anyhow::anyhow!(
            "SOCKET_STALE_REMOVE_FAILED: cannot remove the stale socket '{}': {error}",
            path.display()
        )
    })
}

fn verify_peer_uid(stream: &UnixStream) -> anyhow::Result<()> {
    let expected = current_uid();
    let actual = peer_uid(stream)?;
    if actual != expected {
        anyhow::bail!(
            "SOCKET_PEER_UNAUTHORIZED: uid {actual} connected, but this socket only accepts uid {expected}"
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> anyhow::Result<u32> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: fd is a valid Unix stream; credentials and length point to valid writable memory.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 || length != std::mem::size_of::<libc::ucred>() as libc::socklen_t {
        return Err(std::io::Error::last_os_error()).map_err(|error| {
            anyhow::anyhow!(
                "SOCKET_PEER_CREDENTIALS_FAILED: could not read Linux peer credentials: {error}"
            )
        });
    }
    Ok(credentials.uid)
}

#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> anyhow::Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: fd is a valid Unix stream; uid and gid are valid output pointers.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).map_err(|error| {
            anyhow::anyhow!(
                "SOCKET_PEER_CREDENTIALS_FAILED: could not read macOS peer credentials: {error}"
            )
        });
    }
    Ok(uid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_uid(_stream: &UnixStream) -> anyhow::Result<u32> {
    anyhow::bail!(
        "SOCKET_PEER_CREDENTIALS_UNSUPPORTED: this platform does not provide verified Unix-socket peer credentials"
    )
}

pub fn request(path: impl AsRef<Path>, request: &SocketRequest) -> anyhow::Result<SocketResponse> {
    let mut stream = UnixStream::connect(path)?;
    stream.write_all(serde_json::to_string(request)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response)?;
    let response: SocketResponse = serde_json::from_str(&response)?;
    let SocketResponse::CapturePrepared { transaction_id, .. } = response else {
        return Ok(response);
    };
    let acknowledgement = CaptureCommitAck {
        kind: "capture_commit_ack".into(),
        transaction_id,
    };
    reader
        .get_mut()
        .write_all(serde_json::to_string(&acknowledgement)?.as_bytes())?;
    reader.get_mut().write_all(b"\n")?;
    reader.get_mut().flush()?;
    let mut final_response = String::new();
    reader.read_line(&mut final_response)?;
    Ok(serde_json::from_str(&final_response)?)
}

fn handle_stream(
    stream: UnixStream,
    store: Arc<Mutex<Store>>,
    pr_add: Option<Arc<PrAddHandler>>,
) -> anyhow::Result<()> {
    let mut writer = stream;
    let mut reader = BufReader::new(writer.try_clone()?);
    let mut line = Vec::new();
    reader
        .by_ref()
        .take((MAX_SOCKET_REQUEST_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)?;
    let response = if line.len() > MAX_SOCKET_REQUEST_BYTES {
        serde_json::to_value(SocketResponse::Error {
            error: ActionableError {
                code: "socket_request_too_large".into(),
                what_happened: format!(
                    "The CLI request exceeded the {} byte limit.",
                    MAX_SOCKET_REQUEST_BYTES
                ),
                data_safety: "No review state changed.".into(),
                next_step: "Reduce the request payload and retry.".into(),
            },
        })?
    } else {
        match serde_json::from_slice::<SocketRequest>(&line) {
            Ok(SocketRequest::PrAdd { url }) if pr_add.is_some() => {
                serde_json::to_value(pr_add.expect("checked above")(url))?
            }
            Ok(SocketRequest::CaptureLocal { request }) => {
                let transaction_id = uuid::Uuid::new_v4().to_string();
                let mut locked = store
                    .lock()
                    .map_err(|_| anyhow::anyhow!("store lock poisoned"))?;
                let outcome = locked.ingest_local_capture_with_ack(&request, |result| {
                    let prepared = SocketResponse::CapturePrepared {
                        transaction_id: transaction_id.clone(),
                        data: submission_result(result.clone()),
                    };
                    let encoded = serde_json::to_string(&prepared).map_err(|_| {
                        capture_transport_error(
                            "The desktop could not encode the prepared capture response.",
                        )
                    })?;
                    writer
                        .write_all(encoded.as_bytes())
                        .and_then(|_| writer.write_all(b"\n"))
                        .and_then(|_| writer.flush())
                        .map_err(|_| {
                            capture_transport_error(
                                "The CLI disconnected before capture was acknowledged.",
                            )
                    })?;
                    let mut acknowledgement = String::new();
                    reader
                        .by_ref()
                        .take((MAX_SOCKET_REQUEST_BYTES + 1) as u64)
                        .read_line(&mut acknowledgement)
                        .map_err(|_| {
                            capture_transport_error(
                                "The capture acknowledgement could not be read from the CLI.",
                            )
                        })?;
                    if acknowledgement.len() > MAX_SOCKET_REQUEST_BYTES {
                        return Err(capture_transport_error(
                            "The capture acknowledgement exceeded the protocol limit.",
                        ));
                    }
                    let acknowledgement: CaptureCommitAck =
                        serde_json::from_str(&acknowledgement).map_err(|_| {
                            capture_transport_error(
                                "The CLI disconnected or returned an invalid capture acknowledgement.",
                            )
                        })?;
                    if acknowledgement.kind != "capture_commit_ack"
                        || acknowledgement.transaction_id != transaction_id
                    {
                        return Err(capture_transport_error(
                            "The CLI returned a mismatched capture acknowledgement.",
                        ));
                    }
                    Ok(())
                });
                serde_json::to_value(match outcome {
                    Ok(result) => SocketResponse::Ok {
                        data: submission_result(result),
                    },
                    Err(error) => SocketResponse::Error { error: error.error },
                })?
            }
            Ok(request) => {
                let mut locked = store
                    .lock()
                    .map_err(|_| anyhow::anyhow!("store lock poisoned"))?;
                serde_json::to_value(dispatch(&mut locked, request))?
            }
            Err(socket_error) => {
                match serde_json::from_slice::<crate::machine::MachineRequest>(&line) {
                    Ok(request) => {
                        let locked = store
                            .lock()
                            .map_err(|_| anyhow::anyhow!("store lock poisoned"))?;
                        serde_json::to_value(crate::machine::dispatch_store(&locked, request))?
                    }
                    Err(_) => serde_json::to_value(SocketResponse::Error {
                        error: ActionableError {
                            code: "invalid_socket_request".into(),
                            what_happened: format!("The CLI request was invalid: {socket_error}"),
                            data_safety: "No review state changed.".into(),
                            next_step:
                                "Run review-queue --help and retry with a supported command.".into(),
                        },
                    })?,
                }
            }
        }
    };
    writer.write_all(serde_json::to_string(&response)?.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn capture_transport_error(what_happened: &str) -> DomainError {
    DomainError::actionable(
        what_happened,
        "SQLite, Git refs, and the exact original staging indexes were restored.",
        "Keep the desktop app running and retry the same local submission.",
        "capture_transport_failed",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture;
    use crate::{AgentRouteProvenance, ReviewBrief, Round, local_topic_identity};
    use chrono::Utc;
    use std::{
        net::Shutdown,
        os::unix::{fs::symlink, net::UnixStream},
        path::Path,
        process::Command,
        thread,
    };

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
    }

    fn git_output(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8_lossy(&output.stdout).trim_end().into()
    }

    fn canonical_json_bytes(value: impl Serialize) -> Vec<u8> {
        serde_json::to_vec(&serde_json::to_value(value).unwrap()).unwrap()
    }

    fn capture_fixture() -> (tempfile::TempDir, CaptureRequest) {
        let workspace = tempfile::tempdir().unwrap();
        git(workspace.path(), &["init", "-q"]);
        git(
            workspace.path(),
            &["config", "user.email", "review@example.test"],
        );
        git(workspace.path(), &["config", "user.name", "Review Test"]);
        fs::write(workspace.path().join("file.txt"), "initial\n").unwrap();
        git(workspace.path(), &["add", "file.txt"]);
        git(workspace.path(), &["commit", "-qm", "initial"]);
        fs::write(workspace.path().join("file.txt"), "changed\n").unwrap();
        (
            workspace,
            CaptureRequest {
                workspace_root: Path::new(".").into(),
                topic: "parity".into(),
                brief: ReviewBrief {
                    title: "Parity".into(),
                    what: "Use one ingestion path.".into(),
                    why: String::new(),
                    approach_alternatives: String::new(),
                    testing: String::new(),
                },
                origin_route_id: None,
                participating_repository_ids: Vec::new(),
                preflight_token: None,
            },
        )
    }

    #[test]
    fn external_effects_are_not_exposed_on_socket() {
        assert!(
            serde_json::from_str::<SocketRequest>(
                r#"{"type":"deliver_feedback","round_id":"round","delivery_id":"delivery","route_id":"route","policy":"queue"}"#,
            )
            .is_err(),
            "the credential-free CLI protocol must not expose ACP delivery"
        );
        for request in [
            SocketRequest::Deliver {
                round_id: "round".into(),
            },
            SocketRequest::Publish {
                round_id: "round".into(),
            },
        ] {
            let encoded = serde_json::to_string(&request).unwrap();
            let mut store = Store::in_memory().unwrap();
            let response = dispatch(&mut store, request);
            assert!(
                matches!(response, SocketResponse::Error { error } if error.code == "socket_operation_forbidden"),
                "socket must reject {encoded}"
            );
        }
    }

    #[test]
    fn machine_add_uses_the_full_config_and_is_idempotent() {
        let config = MachineConfig {
            name: "buildbox".into(),
            endpoint: crate::machine::MachineEndpoint::Ssh {
                target: "review@buildbox".into(),
                remote_socket: "/run/review-queue.sock".into(),
                adapter: crate::machine::SshAdapter::SystemOpenSsh,
            },
            source_type: crate::machine::MachineSourceType::ReviewQueueDaemon,
        };
        let mut store = Store::in_memory().unwrap();
        let first = dispatch(
            &mut store,
            SocketRequest::AddMachine {
                config: config.clone(),
            },
        );
        let second = dispatch(&mut store, SocketRequest::AddMachine { config });
        let data = |response: SocketResponse| match response {
            SocketResponse::Ok { data } => data,
            SocketResponse::Error { error } => panic!("machine add failed: {error}"),
            SocketResponse::CapturePrepared { .. } => panic!("unexpected capture frame"),
        };
        let first = data(first);
        let second = data(second);
        assert_eq!(first["created"], true);
        assert_eq!(second["created"], false);
        assert_eq!(first["machine"]["id"], second["machine"]["id"]);
        assert_eq!(first["machine"]["config"]["endpoint"]["kind"], "ssh");
        assert_eq!(
            first["machine"]["config"]["endpoint"]["remote_socket"],
            "/run/review-queue.sock"
        );
    }

    #[test]
    fn socket_machine_add_returns_the_byte_identical_ui_record_and_existing_id() {
        let config = MachineConfig {
            name: "parity-box".into(),
            endpoint: crate::machine::MachineEndpoint::Ssh {
                target: "review@parity-box".into(),
                remote_socket: "/run/review-queue.sock".into(),
                adapter: crate::machine::SshAdapter::SystemOpenSsh,
            },
            source_type: crate::machine::MachineSourceType::ReviewQueueDaemon,
        };
        let mut store = Store::in_memory().unwrap();
        let (ui_record, created) = store.add_machine_config(&config).unwrap();
        assert!(created);

        let socket = match dispatch(&mut store, SocketRequest::AddMachine { config }) {
            SocketResponse::Ok { data } => data,
            SocketResponse::Error { error } => panic!("machine add failed: {error}"),
            SocketResponse::CapturePrepared { .. } => panic!("unexpected capture frame"),
        };
        assert_eq!(socket["created"], false);
        assert_eq!(socket["machine"]["id"], ui_record.id);
        assert_eq!(
            canonical_json_bytes(&socket["machine"]),
            canonical_json_bytes(&ui_record),
            "CLI socket and UI/core paths must canonically serialize the same machine record"
        );
    }

    #[test]
    fn socket_submit_rerun_returns_the_byte_identical_ui_round_and_existing_id() {
        let (workspace, mut request) = capture_fixture();
        request.workspace_root = workspace.path().into();
        let mut store = Store::in_memory().unwrap();
        let preflight = store.preflight_local_capture(&request).unwrap();
        request.origin_route_id = preflight.origin_route_id;
        request.participating_repository_ids = preflight.participating_repository_ids;
        request.preflight_token = Some(preflight.preflight_token);
        let ui_round = match store.ingest_local_capture(&request).unwrap() {
            SubmissionResult::Created(round) => round,
            other => panic!("UI/core first submit should create, got {other:?}"),
        };

        // A real CLI rerun performs a fresh read-only preflight before sending
        // the second capture request; preflight tokens are intentionally
        // single-use snapshots of the exact current workspace.
        request.participating_repository_ids.clear();
        request.preflight_token = None;
        let retry_preflight = store.preflight_local_capture(&request).unwrap();
        request.origin_route_id = retry_preflight.origin_route_id;
        request.participating_repository_ids = retry_preflight.participating_repository_ids;
        request.preflight_token = Some(retry_preflight.preflight_token);
        let socket = match dispatch(
            &mut store,
            SocketRequest::CaptureLocal {
                request: request.clone(),
            },
        ) {
            SocketResponse::Ok { data } => data,
            SocketResponse::Error { error } => panic!("submit rerun failed: {error}"),
            SocketResponse::CapturePrepared { .. } => panic!("unexpected capture frame"),
        };
        assert_eq!(socket["outcome"], "existing");
        assert_eq!(socket["round"]["id"], ui_round.id);
        assert_eq!(
            canonical_json_bytes(&socket["round"]),
            canonical_json_bytes(&ui_round),
            "CLI socket and UI/core paths must canonically serialize the same immutable round"
        );
    }

    #[test]
    fn socket_capture_matches_direct_ingestion_identity_and_rank_behavior() {
        let (direct_workspace, mut direct_request) = capture_fixture();
        direct_request.workspace_root = direct_workspace.path().into();
        let direct_route = AgentRoute {
            id: "route-parity".into(),
            adapter_kind: "acp".into(),
            agent_id: "agent-parity".into(),
            endpoint: None,
            session_id: Some("session-parity".into()),
            status: "busy".into(),
            last_heartbeat: Utc::now(),
            provenance: Some(Box::new(AgentRouteProvenance {
                schema_version: Some(1),
                original_cwd: Some(direct_workspace.path().to_string_lossy().into_owned()),
                cmux_workspace: Some("workspace-parity".into()),
                cmux_surface: Some("surface-parity".into()),
                ..AgentRouteProvenance::default()
            })),
        };
        let mut direct_store = Store::in_memory().unwrap();
        direct_store.register_route(&direct_route).unwrap();
        let direct_preflight = direct_store
            .preflight_local_capture(&direct_request)
            .unwrap();
        direct_request.origin_route_id = direct_preflight.origin_route_id;
        direct_request.participating_repository_ids = direct_preflight.participating_repository_ids;
        direct_request.preflight_token = Some(direct_preflight.preflight_token);
        let direct_round = match direct_store.ingest_local_capture(&direct_request).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => panic!("first direct ingestion should create"),
        };

        let (socket_workspace, mut socket_request) = capture_fixture();
        socket_request.workspace_root = socket_workspace.path().into();
        let mut socket_store = Store::in_memory().unwrap();
        let mut socket_route = direct_route.clone();
        socket_route.provenance.as_mut().unwrap().original_cwd =
            Some(socket_workspace.path().to_string_lossy().into_owned());
        socket_store.register_route(&socket_route).unwrap();
        let preflight = match dispatch(
            &mut socket_store,
            SocketRequest::PreflightCapture {
                request: socket_request.clone(),
            },
        ) {
            SocketResponse::Ok { data } => {
                serde_json::from_value::<capture::Preflight>(data).unwrap()
            }
            SocketResponse::Error { error } => panic!("preflight failed: {error}"),
            SocketResponse::CapturePrepared { .. } => {
                panic!("preflight unexpectedly entered capture handshake")
            }
        };
        socket_request.origin_route_id = preflight.origin_route_id;
        socket_request.participating_repository_ids = preflight.participating_repository_ids;
        socket_request.preflight_token = Some(preflight.preflight_token);
        let socket_round: Round = match dispatch(
            &mut socket_store,
            SocketRequest::CaptureLocal {
                request: socket_request,
            },
        ) {
            SocketResponse::Ok { data } => serde_json::from_value(data["round"].clone()).unwrap(),
            SocketResponse::Error { error } => panic!("capture failed: {error}"),
            SocketResponse::CapturePrepared { .. } => {
                panic!("in-process dispatch does not use the socket handshake")
            }
        };

        assert_eq!(socket_round.rank, direct_round.rank);
        assert_eq!(socket_round.lifecycle, direct_round.lifecycle);
        assert_eq!(
            socket_round.origin_route_id.as_deref(),
            Some("route-parity")
        );
        assert_eq!(
            direct_round.origin_route_id.as_deref(),
            Some("route-parity")
        );
        assert_eq!(
            socket_round
                .origin_route
                .as_ref()
                .and_then(|route| route.provenance.as_deref())
                .and_then(|provenance| provenance.cmux_surface.as_deref()),
            Some("surface-parity")
        );
        assert_eq!(
            socket_round.topic_identity,
            local_topic_identity(&socket_round.manifest)
        );
        assert_eq!(
            direct_round.topic_identity,
            local_topic_identity(&direct_round.manifest)
        );
        assert_eq!(
            socket_round.manifest.repositories.len(),
            direct_round.manifest.repositories.len()
        );
    }

    #[test]
    fn disconnect_before_capture_ack_rolls_back_store_ref_and_exact_index() {
        let (workspace, mut request) = capture_fixture();
        request.workspace_root = workspace.path().into();
        let preflight = capture::preflight(&request).unwrap();
        request.participating_repository_ids = preflight.participating_repository_ids;
        request.preflight_token = Some(preflight.preflight_token);
        let before = (
            git_output(workspace.path(), &["rev-parse", "HEAD"]),
            git_output(
                workspace.path(),
                &["status", "--porcelain=v1", "--untracked-files=all"],
            ),
            fs::read(workspace.path().join(".git/index")).unwrap(),
        );
        let (server, mut client) = UnixStream::pair().unwrap();
        let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        let worker_store = Arc::clone(&store);
        let worker = thread::spawn(move || handle_stream(server, worker_store, None));
        client
            .write_all(
                serde_json::to_string(&SocketRequest::CaptureLocal { request })
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();
        client.shutdown(Shutdown::Both).unwrap();
        drop(client);
        let _ = worker.join().unwrap();

        assert_eq!(
            (
                git_output(workspace.path(), &["rev-parse", "HEAD"]),
                git_output(
                    workspace.path(),
                    &["status", "--porcelain=v1", "--untracked-files=all"],
                ),
                fs::read(workspace.path().join(".git/index")).unwrap(),
            ),
            before
        );
        assert!(
            store
                .lock()
                .unwrap()
                .list(Some(Collection::Local), true)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_non_private_or_symlinked_parent_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let unsafe_parent = temporary.path().join("unsafe");
        fs::create_dir(&unsafe_parent).unwrap();
        fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o755)).unwrap();
        let error = validate_socket_path(&unsafe_parent.join("queue.sock")).unwrap_err();
        assert!(error.to_string().contains("SOCKET_PARENT_UNSAFE"));

        let link = temporary.path().join("linked");
        symlink(&unsafe_parent, &link).unwrap();
        let error = validate_socket_path(&link.join("queue.sock")).unwrap_err();
        assert!(error.to_string().contains("SOCKET_PARENT_UNSAFE"));
    }

    #[test]
    fn never_removes_regular_files_or_symlinks_as_stale_sockets() {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("queue.sock");
        fs::write(&file, "keep me").unwrap();
        assert!(remove_stale_socket(&file).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "keep me");

        let target = temporary.path().join("target");
        fs::write(&target, "still here").unwrap();
        let link = temporary.path().join("linked.sock");
        symlink(&target, &link).unwrap();
        assert!(remove_stale_socket(&link).is_err());
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "still here");
    }

    #[test]
    fn removes_only_a_user_owned_stale_socket() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("queue.sock");
        let listener = UnixListener::bind(&path).unwrap();
        drop(listener);
        remove_stale_socket(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn bound_socket_is_owner_only() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = temporary.path().join("queue.sock");
        let listener = bind_listener(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(listener);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_oversized_request_without_touching_store() {
        let (server, mut client) = UnixStream::pair().unwrap();
        let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        let worker = thread::spawn(move || handle_stream(server, store, None));
        // The server is allowed to close its read half as soon as the bounded
        // reader observes the limit. A late BrokenPipe is therefore a valid
        // part of this rejection path, not a test failure.
        let _ = client.write_all(&vec![b'x'; MAX_SOCKET_REQUEST_BYTES + 1]);
        let _ = client.write_all(b"\n");
        let _ = client.flush();
        let mut response = String::new();
        BufReader::new(client).read_line(&mut response).unwrap();
        let response: SocketResponse = serde_json::from_str(&response).unwrap();
        assert!(
            matches!(response, SocketResponse::Error { error } if error.code == "socket_request_too_large")
        );
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn desktop_can_route_pr_add_without_putting_credentials_on_the_socket() {
        let (server, mut client) = UnixStream::pair().unwrap();
        let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        let handler: Arc<PrAddHandler> = Arc::new(|url| SocketResponse::Ok {
            data: serde_json::json!({"url": url, "credential_boundary": "desktop_only"}),
        });
        let worker = thread::spawn(move || handle_stream(server, store, Some(handler)));
        client
            .write_all(
                serde_json::to_string(&SocketRequest::PrAdd {
                    url: "https://github.com/o/r/pull/1".into(),
                })
                .unwrap()
                .as_bytes(),
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();
        let mut response = String::new();
        BufReader::new(client).read_line(&mut response).unwrap();
        let response: SocketResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            response,
            SocketResponse::Ok { data }
                if data["credential_boundary"] == "desktop_only"
                    && !data.to_string().contains("token")
        ));
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn verifies_same_user_peer_credentials() {
        let (server, _client) = UnixStream::pair().unwrap();
        verify_peer_uid(&server).unwrap();
    }
}
