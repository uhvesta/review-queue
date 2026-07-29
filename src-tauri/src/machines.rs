//! Desktop-owned connected-machine configuration and explicit pull commands.
//!
//! Remote access is deliberately idle until `connect_machine` is invoked.
//! OpenSSH receives only a validated config host and Unix-socket forwarding
//! paths; keys, passphrases, and SSH option blobs are never persisted or IPC'd.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    thread,
    time::Duration,
};

use chrono::Utc;
use review_queue_core::{
    Collection, Round, SourceMetadata, Submission,
    machine::{
        CacheFreshness, MachineClient, MachineConfig, MachineEndpoint, MachineHealth,
        MachineItemDetail, MachineItemIndex, MachineRecord, MachineSnapshot, UnixSocketTransport,
    },
    store::SubmissionResult,
};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::{AppState, CommandError, Confirmation};

struct RuntimeEntry {
    client: MachineClient<UnixSocketTransport>,
    last_health: Option<MachineHealth>,
    last_error: Option<review_queue_core::ActionableError>,
}

struct SshTunnel {
    child: Child,
    local_socket: PathBuf,
}

struct MachineRuntime {
    socket_dir: PathBuf,
    entries: BTreeMap<String, RuntimeEntry>,
    tunnels: BTreeMap<String, SshTunnel>,
}

pub struct MachineState(Mutex<MachineRuntime>);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MachineReproductionRequest {
    pub round_id: String,
    pub destination: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmMachineReproductionRequest {
    pub round_id: String,
    pub destination: String,
    pub confirmation: Confirmation,
}

impl MachineState {
    pub fn new(runtime_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let socket_dir = runtime_dir.as_ref().join("machines");
        fs::create_dir_all(&socket_dir)?;
        fs::set_permissions(&socket_dir, fs::Permissions::from_mode(0o700))?;
        Ok(Self(Mutex::new(MachineRuntime {
            socket_dir,
            entries: BTreeMap::new(),
            tunnels: BTreeMap::new(),
        })))
    }
}

impl Drop for MachineRuntime {
    fn drop(&mut self) {
        for (_, mut tunnel) in std::mem::take(&mut self.tunnels) {
            let _ = tunnel.child.kill();
            let _ = tunnel.child.wait();
            remove_owned_socket(&tunnel.local_socket);
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddMachineResult {
    pub machine: MachineRecord,
    pub created: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveMachineResult {
    pub id: String,
    pub removed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineStatus {
    pub machine: MachineRecord,
    pub connection: String,
    pub health: Option<MachineHealth>,
    pub cached_item_count: usize,
    pub freshness: CacheFreshness,
    pub last_error: Option<review_queue_core::ActionableError>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineIndexResult {
    pub index: MachineItemIndex,
    pub freshness: CacheFreshness,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineRoundResult {
    pub outcome: String,
    pub round: Round,
    pub snapshot: MachineSnapshot,
}

#[tauri::command]
pub fn add_machine(
    config: MachineConfig,
    state: State<'_, AppState>,
    machines: State<'_, MachineState>,
) -> Result<AddMachineResult, CommandError> {
    let (machine, created) = state
        .0
        .lock()
        .map_err(|_| state_unavailable())?
        .add_machine_config(&config)?;
    machines
        .0
        .lock()
        .map_err(|_| state_unavailable())?
        .ensure_entry(&machine)?;
    Ok(AddMachineResult { machine, created })
}

#[tauri::command]
pub fn list_machines(
    state: State<'_, AppState>,
    machines: State<'_, MachineState>,
) -> Result<Vec<MachineStatus>, CommandError> {
    let records = state
        .0
        .lock()
        .map_err(|_| state_unavailable())?
        .machine_configs()?;
    let mut runtime = machines.0.lock().map_err(|_| state_unavailable())?;
    records
        .into_iter()
        .map(|record| runtime.status(record))
        .collect()
}

#[tauri::command]
pub fn remove_machine(
    id_or_name: String,
    state: State<'_, AppState>,
    machines: State<'_, MachineState>,
) -> Result<RemoveMachineResult, CommandError> {
    let (id, removed) = state
        .0
        .lock()
        .map_err(|_| state_unavailable())?
        .remove_machine(&id_or_name)?;
    if removed {
        machines
            .0
            .lock()
            .map_err(|_| state_unavailable())?
            .remove(&id);
    }
    Ok(RemoveMachineResult { id, removed })
}

/// Explicitly starts an OpenSSH Unix-socket tunnel. Loopback configs merely
/// verify their already-local daemon socket and never spawn a process.
#[tauri::command]
pub fn connect_machine(
    id: String,
    state: State<'_, AppState>,
    machines: State<'_, MachineState>,
) -> Result<MachineStatus, CommandError> {
    let record = machine_record(&state, &id)?;
    let mut runtime = machines.0.lock().map_err(|_| state_unavailable())?;
    runtime.connect(&record)?;
    runtime.status(record)
}

#[tauri::command]
pub fn disconnect_machine(
    id: String,
    state: State<'_, AppState>,
    machines: State<'_, MachineState>,
) -> Result<MachineStatus, CommandError> {
    let record = machine_record(&state, &id)?;
    let mut runtime = machines.0.lock().map_err(|_| state_unavailable())?;
    runtime.disconnect(&record.id);
    runtime.status(record)
}

#[tauri::command]
pub fn fetch_machine_health(
    id: String,
    state: State<'_, AppState>,
    machines: State<'_, MachineState>,
) -> Result<MachineHealth, CommandError> {
    let record = machine_record(&state, &id)?;
    let mut runtime = machines.0.lock().map_err(|_| state_unavailable())?;
    runtime.require_connected(&record)?;
    let entry = runtime.ensure_entry(&record)?;
    let result = entry.client.fetch_health(Utc::now());
    match result {
        Ok(health) => {
            entry.last_health = Some(health.clone());
            entry.last_error = None;
            Ok(health)
        }
        Err(error) => {
            entry.last_error = Some(error.error.clone());
            Err(error.into())
        }
    }
}

#[tauri::command]
pub fn fetch_machine_index(
    id: String,
    state: State<'_, AppState>,
    machines: State<'_, MachineState>,
) -> Result<MachineIndexResult, CommandError> {
    let record = machine_record(&state, &id)?;
    let mut runtime = machines.0.lock().map_err(|_| state_unavailable())?;
    runtime.require_connected(&record)?;
    let entry = runtime.ensure_entry(&record)?;
    let index = entry.client.fetch_index(Utc::now()).map_err(|error| {
        entry.last_error = Some(error.error.clone());
        CommandError::from(error)
    })?;
    entry.last_error = None;
    Ok(MachineIndexResult {
        index,
        freshness: entry.client.index_freshness(Utc::now()),
    })
}

#[tauri::command]
pub fn fetch_machine_item_detail(
    id: String,
    source_item_id: String,
    state: State<'_, AppState>,
    machines: State<'_, MachineState>,
) -> Result<MachineItemDetail, CommandError> {
    let record = machine_record(&state, &id)?;
    let mut runtime = machines.0.lock().map_err(|_| state_unavailable())?;
    runtime.require_connected(&record)?;
    runtime
        .ensure_entry(&record)?
        .client
        .fetch_item_detail(&source_item_id, Utc::now())
        .map_err(Into::into)
}

#[tauri::command]
pub fn fetch_machine_snapshot(
    id: String,
    source_item_id: String,
    snapshot_version: String,
    state: State<'_, AppState>,
    machines: State<'_, MachineState>,
) -> Result<MachineSnapshot, CommandError> {
    let record = machine_record(&state, &id)?;
    let mut runtime = machines.0.lock().map_err(|_| state_unavailable())?;
    runtime.require_connected(&record)?;
    runtime
        .ensure_entry(&record)?
        .client
        .fetch_snapshot(&source_item_id, &snapshot_version, Utc::now())
        .map_err(Into::into)
}

/// Pulls detail and the selected immutable snapshot, then persists the same
/// normalized Round shape used by Local and GitHub queues.
#[tauri::command]
pub fn materialize_machine_round(
    id: String,
    source_item_id: String,
    state: State<'_, AppState>,
    machines: State<'_, MachineState>,
) -> Result<MachineRoundResult, CommandError> {
    let record = machine_record(&state, &id)?;
    let now = Utc::now();
    let (detail, snapshot, cursor) = {
        let mut runtime = machines.0.lock().map_err(|_| state_unavailable())?;
        runtime.require_connected(&record)?;
        let entry = runtime.ensure_entry(&record)?;
        let summary = entry
            .client
            .cached_index()
            .and_then(|index| {
                index
                    .items
                    .iter()
                    .find(|item| item.source_item_id == source_item_id)
                    .cloned()
            })
            .ok_or_else(|| CommandError {
                code: "machine_index_refresh_required".into(),
                message: "The requested item is not in the current machine index.".into(),
                data_safety: "No review round was created and no remote data changed.".into(),
                next_step: "Choose Refresh machine queue, then select an available item.".into(),
            })?;
        let cursor = entry
            .client
            .cached_index()
            .expect("summary came from cached index")
            .cursor
            .version
            .clone();
        let detail = entry
            .client
            .fetch_item_detail(&source_item_id, now)
            .map_err(CommandError::from)?;
        let snapshot = entry
            .client
            .fetch_snapshot(&source_item_id, &summary.snapshot_version, now)
            .map_err(CommandError::from)?;
        (detail, snapshot, cursor)
    };
    validate_materialization(&detail, &snapshot)?;
    let summary = &detail.summary;
    let submission = Submission {
        collection: Collection::Machine,
        topic_identity: format!(
            "{}:{}:{}",
            record.id, summary.remote_workspace_id, summary.topic_key
        ),
        brief: detail.brief.clone(),
        manifest: snapshot.manifest.clone(),
        origin_route: detail.origin_route.clone(),
        source_metadata: Some(SourceMetadata::Machine {
            machine_id: record.id.clone(),
            machine_name: record.config.name.clone(),
            source_item_id: summary.source_item_id.clone(),
            remote_workspace_id: summary.remote_workspace_id.clone(),
            remote_workspace_path: summary.remote_workspace_path.clone(),
            cursor,
            cached_at: now,
        }),
    };
    let mut store = state.0.lock().map_err(|_| state_unavailable())?;
    let outcome = store.submit(submission)?;
    let (outcome, round) = match outcome {
        SubmissionResult::Created(round) => ("created", round),
        SubmissionResult::Existing(round) => ("existing", round),
        SubmissionResult::Superseded { round, .. } => ("superseded", round),
    };
    store.save_machine_snapshot(&round.id, &snapshot)?;
    Ok(MachineRoundResult {
        outcome: outcome.into(),
        round,
        snapshot,
    })
}

#[tauri::command]
pub fn preview_machine_reproduction(
    request: MachineReproductionRequest,
    state: State<'_, AppState>,
) -> Result<review_queue_core::reproduction::ReproductionPreview, CommandError> {
    let store = state.0.lock().map_err(|_| state_unavailable())?;
    let snapshot = store.machine_snapshot(&request.round_id)?;
    review_queue_core::machine::preview_cached_git_reproduction(&snapshot, request.destination)
        .map_err(Into::into)
}

#[tauri::command]
pub fn materialize_machine_reproduction(
    request: ConfirmMachineReproductionRequest,
    state: State<'_, AppState>,
) -> Result<review_queue_core::reproduction::ReproductionResult, CommandError> {
    if !request.confirmation.confirmed
        || request.confirmation.token != format!("reproduce-machine:{}", request.round_id)
    {
        return Err(CommandError {
            code: "confirmation_required".into(),
            message: "Machine reproduction requires explicit confirmation.".into(),
            data_safety: "No directory or source file was created.".into(),
            next_step: format!("Confirm with 'reproduce-machine:{}'.", request.round_id),
        });
    }
    let store = state.0.lock().map_err(|_| state_unavailable())?;
    let snapshot = store.machine_snapshot(&request.round_id)?;
    review_queue_core::machine::reproduce_cached_git_snapshot(&snapshot, request.destination)
        .map_err(Into::into)
}

impl MachineRuntime {
    fn transport_path(&self, record: &MachineRecord) -> PathBuf {
        match &record.config.endpoint {
            MachineEndpoint::Loopback { socket_path } => PathBuf::from(socket_path),
            MachineEndpoint::Ssh { .. } => self.socket_dir.join(format!("{}.sock", record.id)),
        }
    }

    fn ensure_entry(&mut self, record: &MachineRecord) -> Result<&mut RuntimeEntry, CommandError> {
        let path = self.transport_path(record);
        if !self.entries.contains_key(&record.id) {
            let client = MachineClient::new(record.config.clone(), UnixSocketTransport::new(path))?;
            self.entries.insert(
                record.id.clone(),
                RuntimeEntry {
                    client,
                    last_health: None,
                    last_error: None,
                },
            );
        }
        self.entries
            .get_mut(&record.id)
            .ok_or_else(state_unavailable)
    }

    fn connect(&mut self, record: &MachineRecord) -> Result<(), CommandError> {
        self.ensure_entry(record)?;
        match &record.config.endpoint {
            MachineEndpoint::Loopback { socket_path } => {
                require_socket(Path::new(socket_path), "loopback daemon")
            }
            MachineEndpoint::Ssh {
                target,
                remote_socket,
                ..
            } => {
                if self.tunnel_alive(&record.id)? {
                    return Ok(());
                }
                let local_socket = self.transport_path(record);
                if local_socket.exists() {
                    remove_owned_socket(&local_socket);
                    if local_socket.exists() {
                        return Err(CommandError {
                            code: "machine_tunnel_socket_unsafe".into(),
                            message: "The tunnel socket path is occupied by a non-socket file."
                                .into(),
                            data_safety: "Nothing was removed and SSH was not started.".into(),
                            next_step:
                                "Remove the conflicting file from Review Queue's runtime directory, then reconnect."
                                    .into(),
                        });
                    }
                }
                let forward = format!("{}:{}", local_socket.display(), remote_socket);
                let child = Command::new("/usr/bin/ssh")
                    .args(["-N", "-o", "ExitOnForwardFailure=yes", "-L"])
                    .arg(&forward)
                    .arg(target)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|error| CommandError {
                        code: "machine_ssh_start_failed".into(),
                        message: format!("System OpenSSH could not be started: {error}."),
                        data_safety:
                            "No credentials were stored and no review or remote data changed."
                                .into(),
                        next_step:
                            "Verify /usr/bin/ssh and the SSH config host, then choose Connect again."
                                .into(),
                    })?;
                self.tunnels.insert(
                    record.id.clone(),
                    SshTunnel {
                        child,
                        local_socket: local_socket.clone(),
                    },
                );
                for _ in 0..80 {
                    if local_socket.exists() {
                        return require_socket(&local_socket, "SSH tunnel");
                    }
                    if !self.tunnel_alive(&record.id)? {
                        break;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                self.disconnect(&record.id);
                Err(CommandError {
                    code: "machine_tunnel_failed".into(),
                    message: "OpenSSH did not establish the connected-machine tunnel.".into(),
                    data_safety:
                        "No credentials were stored and no review or remote data changed.".into(),
                    next_step:
                        "Run ssh for the configured host in Terminal, start the remote daemon, then reconnect."
                            .into(),
                })
            }
        }
    }

    fn tunnel_alive(&mut self, id: &str) -> Result<bool, CommandError> {
        let Some(tunnel) = self.tunnels.get_mut(id) else {
            return Ok(false);
        };
        let exited = tunnel.child.try_wait().map_err(|error| CommandError {
            code: "machine_tunnel_status_failed".into(),
            message: format!("The OpenSSH tunnel status could not be read: {error}."),
            data_safety: "The prior local cache remains available.".into(),
            next_step: "Disconnect the machine and choose Connect again.".into(),
        })?;
        if exited.is_some() {
            let tunnel = self.tunnels.remove(id).expect("tunnel exists");
            remove_owned_socket(&tunnel.local_socket);
            return Ok(false);
        }
        Ok(true)
    }

    fn require_connected(&mut self, record: &MachineRecord) -> Result<(), CommandError> {
        match &record.config.endpoint {
            MachineEndpoint::Loopback { socket_path } => {
                require_socket(Path::new(socket_path), "loopback daemon")
            }
            MachineEndpoint::Ssh { .. } if self.tunnel_alive(&record.id)? => {
                require_socket(&self.transport_path(record), "SSH tunnel")
            }
            MachineEndpoint::Ssh { .. } => Err(CommandError {
                code: "machine_not_connected".into(),
                message: format!("{} is not connected.", record.config.name),
                data_safety:
                    "No remote request was sent and the prior local cache remains available.".into(),
                next_step: "Choose Connect, then retry this fetch.".into(),
            }),
        }
    }

    fn disconnect(&mut self, id: &str) {
        if let Some(mut tunnel) = self.tunnels.remove(id) {
            let _ = tunnel.child.kill();
            let _ = tunnel.child.wait();
            remove_owned_socket(&tunnel.local_socket);
        }
    }

    fn remove(&mut self, id: &str) {
        self.disconnect(id);
        self.entries.remove(id);
    }

    fn status(&mut self, record: MachineRecord) -> Result<MachineStatus, CommandError> {
        self.ensure_entry(&record)?;
        let connection = match &record.config.endpoint {
            MachineEndpoint::Loopback { socket_path } if Path::new(socket_path).exists() => {
                "connected"
            }
            MachineEndpoint::Loopback { .. } => "unreachable",
            MachineEndpoint::Ssh { .. } if self.tunnel_alive(&record.id)? => "connected",
            MachineEndpoint::Ssh { .. } => "disconnected",
        };
        let entry = self.ensure_entry(&record)?;
        let freshness = entry.client.index_freshness(Utc::now());
        let cached_item_count = entry
            .client
            .cached_index()
            .map_or(0, |index| index.items.len());
        Ok(MachineStatus {
            machine: record,
            connection: connection.into(),
            health: entry.last_health.clone(),
            cached_item_count,
            freshness,
            last_error: entry.last_error.clone(),
        })
    }
}

fn machine_record(state: &State<'_, AppState>, id: &str) -> Result<MachineRecord, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| state_unavailable())?
        .machine_config(id)
        .map_err(Into::into)
}

fn validate_materialization(
    detail: &MachineItemDetail,
    snapshot: &MachineSnapshot,
) -> Result<(), CommandError> {
    if detail.summary.source_item_id != snapshot.source_item_id
        || detail.summary.snapshot_version != snapshot.snapshot_version
        || detail.summary.remote_workspace_id != snapshot.manifest.workspace_id
        || detail.summary.remote_workspace_path != snapshot.manifest.workspace_root
        || detail.summary.topic_key != snapshot.manifest.topic
    {
        return Err(CommandError {
            code: "machine_materialization_mismatch".into(),
            message: "The machine detail and immutable snapshot describe different review items."
                .into(),
            data_safety: "The response was rejected and no review round was created.".into(),
            next_step:
                "Refresh the machine queue; if the mismatch remains, upgrade or repair the remote daemon."
                    .into(),
        });
    }
    Ok(())
}

fn require_socket(path: &Path, label: &str) -> Result<(), CommandError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| CommandError {
        code: "machine_daemon_unreachable".into(),
        message: format!("The configured {label} socket is not available."),
        data_safety: "No remote request was sent and the prior local cache remains available."
            .into(),
        next_step: "Start the daemon or reconnect the tunnel, then retry.".into(),
    })?;
    if !metadata.file_type().is_socket() {
        return Err(CommandError {
            code: "machine_endpoint_not_socket".into(),
            message: format!("The configured {label} path is not a Unix socket."),
            data_safety: "The path was not opened or removed.".into(),
            next_step: "Correct the endpoint to the daemon's Unix socket, then retry.".into(),
        });
    }
    Ok(())
}

fn remove_owned_socket(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    let parent_owned = path.parent().and_then(|parent| fs::metadata(parent).ok());
    if metadata.file_type().is_socket()
        && parent_owned
            .as_ref()
            .is_some_and(|parent| parent.uid() == metadata.uid())
    {
        let _ = fs::remove_file(path);
    }
}

fn state_unavailable() -> CommandError {
    CommandError {
        code: "desktop_state_unavailable".into(),
        message: "Connected-machine state is temporarily unavailable.".into(),
        data_safety: "No machine configuration or review data changed.".into(),
        next_step: "Retry; if this persists, restart Review Queue.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use review_queue_core::{
        RepositorySnapshot, WorkspaceManifest,
        machine::{
            MACHINE_PROTOCOL_VERSION, MachineCursor, MachineHealthState, MachineItemSummary,
            MachineResponse, MachineSourceType,
        },
    };
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
    };

    fn loopback_config(socket_path: String) -> MachineConfig {
        MachineConfig {
            name: "Fixture Mac".into(),
            endpoint: MachineEndpoint::Loopback { socket_path },
            source_type: MachineSourceType::ReviewQueueDaemon,
        }
    }

    #[test]
    fn loopback_transport_fetches_only_explicit_operations() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            for response in [
                MachineResponse::Health(MachineHealth {
                    protocol_version: MACHINE_PROTOCOL_VERSION,
                    daemon_version: "1.0.0".into(),
                    state: MachineHealthState::Healthy,
                    cursor: MachineCursor::new("cursor-1").unwrap(),
                }),
                MachineResponse::ItemIndex(MachineItemIndex {
                    cursor: MachineCursor::new("cursor-1").unwrap(),
                    items: vec![],
                }),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut request)
                    .unwrap();
                assert!(!request.contains("token"));
                serde_json::to_writer(&mut stream, &response).unwrap();
                stream.write_all(b"\n").unwrap();
            }
        });
        let config = loopback_config(socket.to_string_lossy().into_owned());
        let record = MachineRecord {
            id: config.machine_id().unwrap(),
            config,
        };
        let mut runtime = MachineRuntime {
            socket_dir: temp.path().join("runtime"),
            entries: BTreeMap::new(),
            tunnels: BTreeMap::new(),
        };
        runtime.require_connected(&record).unwrap();
        let entry = runtime.ensure_entry(&record).unwrap();
        assert!(entry.client.cached_index().is_none());
        entry.client.fetch_health(Utc::now()).unwrap();
        assert!(entry.client.cached_index().is_none());
        entry.client.fetch_index(Utc::now()).unwrap();
        assert!(entry.client.cached_index().is_some());
        server.join().unwrap();
    }

    #[test]
    fn materialization_rejects_cross_item_snapshot() {
        let summary = MachineItemSummary {
            source_item_id: "item-1".into(),
            remote_workspace_id: "workspace-1".into(),
            remote_workspace_path: "/work/project".into(),
            topic_key: "topic".into(),
            title: "Title".into(),
            manifest_hash: "hash".into(),
            snapshot_version: "snapshot-1".into(),
        };
        let detail = MachineItemDetail {
            summary,
            brief: review_queue_core::ReviewBrief {
                title: "Title".into(),
                what: "Description".into(),
                why: "Why".into(),
                approach_alternatives: "Approach".into(),
                testing: "Testing".into(),
            },
            repository_count: 1,
            updated_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            origin_route: None,
        };
        let snapshot = MachineSnapshot {
            source_item_id: "different".into(),
            snapshot_version: "snapshot-1".into(),
            manifest: WorkspaceManifest {
                workspace_id: "workspace-1".into(),
                workspace_root: "/work/project".into(),
                topic: "topic".into(),
                repositories: vec![RepositorySnapshot {
                    repository_id: "repo".into(),
                    root: ".".into(),
                    branch: "main".into(),
                    base_sha: "base".into(),
                    head_sha: "head".into(),
                    remote_fingerprint: None,
                    object_checksum: "checksum".into(),
                }],
                before_fingerprint: "before".into(),
                after_fingerprint: "after".into(),
                created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            },
            files: vec![],
            repository_packs: vec![],
        };
        assert_eq!(
            validate_materialization(&detail, &snapshot)
                .unwrap_err()
                .code,
            "machine_materialization_mismatch"
        );
    }
}
