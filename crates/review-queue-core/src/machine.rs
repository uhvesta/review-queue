//! Token-free connected-machine federation.
//!
//! This module contains only validated configuration, a compact pull-first
//! protocol, and an explicit-operation client cache. The desktop shell may
//! establish an SSH tunnel with the user's existing SSH configuration and
//! agent, but credentials and tunnel processes never cross this boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ActionableError, AgentRoute, Collection, DomainError, WorkspaceManifest,
    diff::{DiffFile, DiffFileStatus, MaterializedDiff, PinnedFileContent, RepositoryDiff},
    store::Store,
};

pub const MACHINE_PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_REMOTE_SOCKET: &str = "/tmp/review-queue-daemon.sock";
pub const MAX_MACHINE_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MachineSourceType {
    ReviewQueueDaemon,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SshAdapter {
    /// Invoke the platform OpenSSH client using its existing config and agent.
    SystemOpenSsh,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineEndpoint {
    /// A Unix socket used for this-Mac development and deterministic fixtures.
    Loopback { socket_path: String },
    /// A remote daemon reached through a caller-owned OpenSSH tunnel.
    Ssh {
        /// An OpenSSH config host, optionally prefixed by `user@`.
        target: String,
        remote_socket: String,
        adapter: SshAdapter,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MachineConfig {
    pub name: String,
    pub endpoint: MachineEndpoint,
    pub source_type: MachineSourceType,
}

impl MachineConfig {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_name(&self.name)?;
        match &self.endpoint {
            MachineEndpoint::Loopback { socket_path } => {
                validate_socket_path(socket_path, "local socket")?;
            }
            MachineEndpoint::Ssh {
                target,
                remote_socket,
                ..
            } => {
                validate_ssh_target(target)?;
                validate_socket_path(remote_socket, "remote socket")?;
            }
        }
        validate_token_free(self)
    }

    /// Stable local identity. A display-name change does not conflate or
    /// duplicate the same endpoint/source pair.
    pub fn machine_id(&self) -> Result<String, DomainError> {
        self.validate()?;
        let identity = serde_json::to_vec(&(self.source_type, &self.endpoint))
            .expect("machine identity is serializable");
        Ok(format!("machine-{:x}", Sha256::digest(identity)))
    }

    pub fn normalized(mut self) -> Self {
        self.name = self.name.trim().to_owned();
        self
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MachineRecord {
    pub id: String,
    pub config: MachineConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddMachineOutcome {
    Added(MachineRecord),
    Existing(MachineRecord),
}

/// Small in-memory registry contract used by both UI and CLI persistence
/// layers. Re-adding an identical machine is an idempotent success.
#[derive(Clone, Debug, Default)]
pub struct MachineRegistry {
    by_id: BTreeMap<String, MachineRecord>,
}

impl MachineRegistry {
    pub fn add(&mut self, config: MachineConfig) -> Result<AddMachineOutcome, DomainError> {
        config.validate()?;
        let config = config.normalized();
        let id = config.machine_id()?;

        if let Some(existing) = self.by_id.get(&id) {
            if existing.config == config {
                return Ok(AddMachineOutcome::Existing(existing.clone()));
            }
            return Err(error(
                "That endpoint is already configured with another machine name.",
                "The existing machine was preserved and no configuration was saved.",
                "Use the existing machine or remove it before assigning a new name.",
                "machine_endpoint_collision",
            ));
        }
        if self
            .by_id
            .values()
            .any(|record| record.config.name.eq_ignore_ascii_case(&config.name))
        {
            return Err(error(
                "A connected machine already uses that name.",
                "No machine configuration was saved.",
                "Choose a unique machine name or re-add the existing configuration.",
                "machine_name_collision",
            ));
        }
        let record = MachineRecord { id, config };
        self.by_id.insert(record.id.clone(), record.clone());
        Ok(AddMachineOutcome::Added(record))
    }

    pub fn get(&self, id: &str) -> Option<&MachineRecord> {
        self.by_id.get(id)
    }

    pub fn records(&self) -> impl Iterator<Item = &MachineRecord> {
        self.by_id.values()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MachineCursor {
    pub version: String,
}

impl MachineCursor {
    pub fn new(version: impl Into<String>) -> Result<Self, DomainError> {
        let cursor = Self {
            version: version.into(),
        };
        cursor.validate()?;
        Ok(cursor)
    }

    fn validate(&self) -> Result<(), DomainError> {
        if self.version.trim().is_empty()
            || self.version.len() > 256
            || self.version.chars().any(char::is_whitespace)
        {
            return Err(protocol_error(
                "The machine returned an invalid cache cursor.",
                "Reconnect after the remote daemon is upgraded or repaired.",
                "machine_cursor_invalid",
            ));
        }
        validate_token_free(self)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MachineHealthState {
    Healthy,
    Degraded,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MachineHealth {
    pub protocol_version: u32,
    pub daemon_version: String,
    pub state: MachineHealthState,
    pub cursor: MachineCursor,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MachineItemSummary {
    pub source_item_id: String,
    pub remote_workspace_id: String,
    pub remote_workspace_path: String,
    pub topic_key: String,
    pub title: String,
    pub manifest_hash: String,
    pub snapshot_version: String,
}

impl MachineItemSummary {
    /// Machine ID is intentionally supplied by the local caller and is never
    /// accepted from the remote daemon.
    pub fn local_identity(&self, machine_id: &str) -> String {
        format!(
            "{machine_id}:{}:{}",
            self.remote_workspace_id, self.topic_key
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MachineItemIndex {
    pub cursor: MachineCursor,
    pub items: Vec<MachineItemSummary>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MachineItemDetail {
    pub summary: MachineItemSummary,
    pub description: String,
    pub repository_count: u32,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub origin_route: Option<AgentRoute>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MachineSnapshotFile {
    pub repository_id: String,
    pub workspace_relative_path: String,
    pub status: String,
    pub base_blob_sha: String,
    pub head_blob_sha: String,
    pub unified_diff: String,
    #[serde(default)]
    pub is_binary: bool,
    #[serde(default)]
    pub base_content_base64: Option<String>,
    #[serde(default)]
    pub head_content_base64: Option<String>,
    #[serde(default)]
    pub materialized: Option<DiffFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MachineSnapshot {
    pub source_item_id: String,
    pub snapshot_version: String,
    pub manifest: WorkspaceManifest,
    pub files: Vec<MachineSnapshotFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineRequest {
    Health {
        protocol_version: u32,
    },
    ItemIndex {
        protocol_version: u32,
        after_cursor: Option<MachineCursor>,
    },
    ItemDetail {
        protocol_version: u32,
        source_item_id: String,
    },
    Snapshot {
        protocol_version: u32,
        source_item_id: String,
        snapshot_version: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum MachineResponse {
    Health(MachineHealth),
    ItemIndex(MachineItemIndex),
    ItemDetail(MachineItemDetail),
    Snapshot(MachineSnapshot),
    Error { error: ActionableError },
}

pub trait MachineTransport {
    /// Performs exactly one caller-requested exchange. Implementations must
    /// not poll, prefetch, refresh, or retry in the background.
    fn exchange(&mut self, request: MachineRequest) -> Result<MachineResponse, DomainError>;
}

/// One-request-per-connection Unix transport used directly for loopback
/// machines and through an explicitly opened OpenSSH Unix-socket forward.
#[derive(Clone, Debug)]
pub struct UnixSocketTransport {
    socket_path: PathBuf,
    timeout: Duration,
}

impl UnixSocketTransport {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout: Duration::from_secs(10),
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl MachineTransport for UnixSocketTransport {
    fn exchange(&mut self, request: MachineRequest) -> Result<MachineResponse, DomainError> {
        validate_token_free(&request)?;
        let mut stream = UnixStream::connect(&self.socket_path).map_err(|error| {
            transport_error(
                format!("The connected-machine daemon could not be reached: {error}."),
                "Start the remote daemon or reconnect the SSH tunnel, then retry.",
                "machine_daemon_unreachable",
            )
        })?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(machine_io_error)?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(machine_io_error)?;
        serde_json::to_writer(&mut stream, &request).map_err(|_| {
            transport_error(
                "The connected-machine request could not be encoded.",
                "Upgrade or repair the desktop app, then retry.",
                "machine_request_encode_failed",
            )
        })?;
        stream.write_all(b"\n").map_err(machine_io_error)?;
        stream.flush().map_err(machine_io_error)?;

        let mut frame = Vec::new();
        let read = BufReader::new(stream)
            .take((MAX_MACHINE_FRAME_BYTES + 1) as u64)
            .read_until(b'\n', &mut frame)
            .map_err(machine_io_error)?;
        if read == 0 {
            return Err(transport_error(
                "The connected-machine daemon closed without a response.",
                "Check the remote daemon logs, then reconnect and retry.",
                "machine_response_missing",
            ));
        }
        if frame.len() > MAX_MACHINE_FRAME_BYTES {
            return Err(transport_error(
                "The connected-machine response exceeded the safe frame limit.",
                "Upgrade or repair the remote daemon before reconnecting.",
                "machine_response_too_large",
            ));
        }
        let response: MachineResponse = serde_json::from_slice(&frame).map_err(|_| {
            transport_error(
                "The connected-machine daemon returned an invalid protocol response.",
                "Check that both machines run compatible Review Queue versions, then retry.",
                "machine_response_invalid",
            )
        })?;
        validate_token_free(&response)?;
        Ok(response)
    }
}

fn machine_io_error(error: std::io::Error) -> DomainError {
    transport_error(
        format!("The connected-machine exchange failed: {error}."),
        "Check the daemon and tunnel, then retry the requested fetch.",
        "machine_transport_failed",
    )
}

fn transport_error(
    what: impl Into<String>,
    next: impl Into<String>,
    code: impl Into<String>,
) -> DomainError {
    error(
        what,
        "The previous local cache was preserved and no remote data was changed.",
        next,
        code,
    )
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CacheFreshness {
    pub cached: bool,
    pub cursor: Option<MachineCursor>,
    pub cached_at: Option<DateTime<Utc>>,
    pub age_seconds: Option<i64>,
}

#[derive(Clone, Debug)]
struct Cached<T> {
    value: T,
    cached_at: DateTime<Utc>,
}

/// Lazy, pull-first connected-machine client. Construction and cache reads
/// issue zero transport calls; every remote read is named `fetch_*`.
pub struct MachineClient<T> {
    machine: MachineRecord,
    transport: T,
    health: Option<Cached<MachineHealth>>,
    index: Option<Cached<MachineItemIndex>>,
    details: BTreeMap<String, Cached<MachineItemDetail>>,
    snapshots: BTreeMap<(String, String), Cached<MachineSnapshot>>,
}

impl<T: MachineTransport> MachineClient<T> {
    pub fn new(config: MachineConfig, transport: T) -> Result<Self, DomainError> {
        let id = config.machine_id()?;
        Ok(Self {
            machine: MachineRecord {
                id,
                config: config.normalized(),
            },
            transport,
            health: None,
            index: None,
            details: BTreeMap::new(),
            snapshots: BTreeMap::new(),
        })
    }

    pub fn machine(&self) -> &MachineRecord {
        &self.machine
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn cached_index(&self) -> Option<&MachineItemIndex> {
        self.index.as_ref().map(|cached| &cached.value)
    }

    pub fn cached_detail(&self, source_item_id: &str) -> Option<&MachineItemDetail> {
        self.details.get(source_item_id).map(|cached| &cached.value)
    }

    pub fn cached_snapshot(
        &self,
        source_item_id: &str,
        snapshot_version: &str,
    ) -> Option<&MachineSnapshot> {
        self.snapshots
            .get(&(source_item_id.to_owned(), snapshot_version.to_owned()))
            .map(|cached| &cached.value)
    }

    pub fn index_freshness(&self, now: DateTime<Utc>) -> CacheFreshness {
        match &self.index {
            Some(cached) => CacheFreshness {
                cached: true,
                cursor: Some(cached.value.cursor.clone()),
                cached_at: Some(cached.cached_at),
                age_seconds: Some(
                    now.signed_duration_since(cached.cached_at)
                        .num_seconds()
                        .max(0),
                ),
            },
            None => CacheFreshness {
                cached: false,
                cursor: None,
                cached_at: None,
                age_seconds: None,
            },
        }
    }

    pub fn fetch_health(&mut self, now: DateTime<Utc>) -> Result<MachineHealth, DomainError> {
        let response = self.exchange(MachineRequest::Health {
            protocol_version: MACHINE_PROTOCOL_VERSION,
        })?;
        let MachineResponse::Health(health) = response else {
            return Err(unexpected_response("health"));
        };
        validate_health(&health)?;
        self.health = Some(Cached {
            value: health.clone(),
            cached_at: now,
        });
        Ok(health)
    }

    pub fn fetch_index(&mut self, now: DateTime<Utc>) -> Result<MachineItemIndex, DomainError> {
        let after_cursor = self
            .index
            .as_ref()
            .map(|cached| cached.value.cursor.clone());
        let response = self.exchange(MachineRequest::ItemIndex {
            protocol_version: MACHINE_PROTOCOL_VERSION,
            after_cursor,
        })?;
        let MachineResponse::ItemIndex(index) = response else {
            return Err(unexpected_response("item index"));
        };
        validate_index(&index)?;
        self.index = Some(Cached {
            value: index.clone(),
            cached_at: now,
        });
        Ok(index)
    }

    pub fn fetch_item_detail(
        &mut self,
        source_item_id: &str,
        now: DateTime<Utc>,
    ) -> Result<MachineItemDetail, DomainError> {
        validate_id(source_item_id, "source item ID")?;
        let response = self.exchange(MachineRequest::ItemDetail {
            protocol_version: MACHINE_PROTOCOL_VERSION,
            source_item_id: source_item_id.to_owned(),
        })?;
        let MachineResponse::ItemDetail(detail) = response else {
            return Err(unexpected_response("item detail"));
        };
        validate_detail(&detail)?;
        if detail.summary.source_item_id != source_item_id {
            return Err(protocol_error(
                "The machine returned detail for a different queue item.",
                "Reconnect and retry after checking the remote daemon version.",
                "machine_item_identity_mismatch",
            ));
        }
        self.details.insert(
            source_item_id.to_owned(),
            Cached {
                value: detail.clone(),
                cached_at: now,
            },
        );
        Ok(detail)
    }

    pub fn fetch_snapshot(
        &mut self,
        source_item_id: &str,
        snapshot_version: &str,
        now: DateTime<Utc>,
    ) -> Result<MachineSnapshot, DomainError> {
        validate_id(source_item_id, "source item ID")?;
        validate_id(snapshot_version, "snapshot version")?;
        let response = self.exchange(MachineRequest::Snapshot {
            protocol_version: MACHINE_PROTOCOL_VERSION,
            source_item_id: source_item_id.to_owned(),
            snapshot_version: snapshot_version.to_owned(),
        })?;
        let MachineResponse::Snapshot(snapshot) = response else {
            return Err(unexpected_response("snapshot"));
        };
        validate_snapshot(&snapshot)?;
        if snapshot.source_item_id != source_item_id
            || snapshot.snapshot_version != snapshot_version
        {
            return Err(protocol_error(
                "The machine returned a different snapshot than requested.",
                "Reconnect and retry after checking the remote daemon version.",
                "machine_snapshot_identity_mismatch",
            ));
        }
        self.snapshots.insert(
            (source_item_id.to_owned(), snapshot_version.to_owned()),
            Cached {
                value: snapshot.clone(),
                cached_at: now,
            },
        );
        Ok(snapshot)
    }

    fn exchange(&mut self, request: MachineRequest) -> Result<MachineResponse, DomainError> {
        validate_token_free(&request)?;
        let response = self.transport.exchange(request)?;
        validate_token_free(&response)?;
        if let MachineResponse::Error { error } = response {
            return Err(DomainError { error });
        }
        Ok(response)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoopbackCounters {
    pub health: u64,
    pub item_index: u64,
    pub item_detail: u64,
    pub snapshot: u64,
}

/// Deterministic token-free transport used by the standalone-daemon contract
/// suite. It models a tunnel failure without storing any SSH material.
#[derive(Clone, Debug)]
pub struct LoopbackFakeTransport {
    pub health: MachineHealth,
    pub index: MachineItemIndex,
    pub details: BTreeMap<String, MachineItemDetail>,
    pub snapshots: BTreeMap<(String, String), MachineSnapshot>,
    counters: LoopbackCounters,
    fail_next_tunnel: bool,
    request_log: Vec<MachineRequest>,
}

impl LoopbackFakeTransport {
    pub fn new(
        health: MachineHealth,
        index: MachineItemIndex,
        details: Vec<MachineItemDetail>,
        snapshots: Vec<MachineSnapshot>,
    ) -> Result<Self, DomainError> {
        validate_health(&health)?;
        validate_index(&index)?;
        let mut details_by_id = BTreeMap::new();
        for detail in details {
            validate_detail(&detail)?;
            details_by_id.insert(detail.summary.source_item_id.clone(), detail);
        }
        let mut snapshots_by_id = BTreeMap::new();
        for snapshot in snapshots {
            validate_snapshot(&snapshot)?;
            snapshots_by_id.insert(
                (
                    snapshot.source_item_id.clone(),
                    snapshot.snapshot_version.clone(),
                ),
                snapshot,
            );
        }
        Ok(Self {
            health,
            index,
            details: details_by_id,
            snapshots: snapshots_by_id,
            counters: LoopbackCounters::default(),
            fail_next_tunnel: false,
            request_log: vec![],
        })
    }

    pub fn counters(&self) -> LoopbackCounters {
        self.counters
    }

    pub fn request_log(&self) -> &[MachineRequest] {
        &self.request_log
    }

    pub fn fail_next_tunnel(&mut self) {
        self.fail_next_tunnel = true;
    }
}

impl MachineTransport for LoopbackFakeTransport {
    fn exchange(&mut self, request: MachineRequest) -> Result<MachineResponse, DomainError> {
        validate_token_free(&request)?;
        self.request_log.push(request.clone());
        if self.fail_next_tunnel {
            self.fail_next_tunnel = false;
            return Err(error(
                "The SSH tunnel to the connected machine failed.",
                "Cached rounds remain available and no remote data was changed.",
                "Check the SSH target and remote daemon, then reconnect and retry.",
                "machine_tunnel_failed",
            ));
        }
        match request {
            MachineRequest::Health { protocol_version } => {
                validate_version(protocol_version)?;
                self.counters.health += 1;
                Ok(MachineResponse::Health(self.health.clone()))
            }
            MachineRequest::ItemIndex {
                protocol_version, ..
            } => {
                validate_version(protocol_version)?;
                self.counters.item_index += 1;
                Ok(MachineResponse::ItemIndex(self.index.clone()))
            }
            MachineRequest::ItemDetail {
                protocol_version,
                source_item_id,
            } => {
                validate_version(protocol_version)?;
                self.counters.item_detail += 1;
                self.details
                    .get(&source_item_id)
                    .cloned()
                    .map(MachineResponse::ItemDetail)
                    .ok_or_else(|| {
                        error(
                            "The requested remote queue item no longer exists.",
                            "No local cache or remote data was changed.",
                            "Refresh the machine queue and choose an available item.",
                            "machine_item_not_found",
                        )
                    })
            }
            MachineRequest::Snapshot {
                protocol_version,
                source_item_id,
                snapshot_version,
            } => {
                validate_version(protocol_version)?;
                self.counters.snapshot += 1;
                self.snapshots
                    .get(&(source_item_id, snapshot_version))
                    .cloned()
                    .map(MachineResponse::Snapshot)
                    .ok_or_else(|| {
                        error(
                            "The requested remote snapshot is no longer available.",
                            "No local cache or remote data was changed.",
                            "Refresh the item and materialize its current snapshot.",
                            "machine_snapshot_not_found",
                        )
                    })
            }
        }
    }
}

/// Credential-free protocol implementation used by the shipped standalone
/// daemon. It exposes only normalized queue metadata and immutable captured
/// source. No Keychain, SSH configuration, or provider credential is read.
pub fn dispatch_store(store: &Store, request: MachineRequest) -> MachineResponse {
    match dispatch_store_result(store, request) {
        Ok(response) => response,
        Err(error) => MachineResponse::Error { error: error.error },
    }
}

fn dispatch_store_result(
    store: &Store,
    request: MachineRequest,
) -> Result<MachineResponse, DomainError> {
    validate_token_free(&request)?;
    match request {
        MachineRequest::Health { protocol_version } => {
            validate_version(protocol_version)?;
            let rounds = served_rounds(store)?;
            Ok(MachineResponse::Health(MachineHealth {
                protocol_version: MACHINE_PROTOCOL_VERSION,
                daemon_version: env!("CARGO_PKG_VERSION").into(),
                state: MachineHealthState::Healthy,
                cursor: served_cursor(&rounds)?,
            }))
        }
        MachineRequest::ItemIndex {
            protocol_version, ..
        } => {
            validate_version(protocol_version)?;
            let rounds = served_rounds(store)?;
            Ok(MachineResponse::ItemIndex(MachineItemIndex {
                cursor: served_cursor(&rounds)?,
                items: rounds.iter().map(round_summary).collect(),
            }))
        }
        MachineRequest::ItemDetail {
            protocol_version,
            source_item_id,
        } => {
            validate_version(protocol_version)?;
            let round = served_round(store, &source_item_id)?;
            let origin_route = round
                .origin_route_id
                .as_deref()
                .map(|id| store.route(id))
                .transpose()?;
            Ok(MachineResponse::ItemDetail(MachineItemDetail {
                summary: round_summary(&round),
                description: round.brief.what.clone(),
                repository_count: round.manifest.repositories.len() as u32,
                updated_at: round.created_at,
                origin_route,
            }))
        }
        MachineRequest::Snapshot {
            protocol_version,
            source_item_id,
            snapshot_version,
        } => {
            validate_version(protocol_version)?;
            let round = served_round(store, &source_item_id)?;
            if snapshot_version != round.manifest_hash {
                return Err(error(
                    "The requested remote snapshot version is no longer available.",
                    "No local or remote review state was changed.",
                    "Refresh the machine index and request its current snapshot.",
                    "machine_snapshot_version_not_found",
                ));
            }
            Ok(MachineResponse::Snapshot(snapshot_from_round(&round)?))
        }
    }
}

fn served_rounds(store: &Store) -> Result<Vec<crate::Round>, DomainError> {
    store.list(Some(Collection::Local), false)
}

fn served_round(store: &Store, id: &str) -> Result<crate::Round, DomainError> {
    let round = store.round(id)?;
    if round.collection != Collection::Local || round.superseded_by.is_some() {
        return Err(error(
            "The requested remote queue item is not available from this daemon.",
            "No local or remote review state was changed.",
            "Refresh the connected-machine queue.",
            "machine_item_not_found",
        ));
    }
    Ok(round)
}

fn round_summary(round: &crate::Round) -> MachineItemSummary {
    MachineItemSummary {
        source_item_id: round.id.clone(),
        remote_workspace_id: round.manifest.workspace_id.clone(),
        remote_workspace_path: round.manifest.workspace_root.clone(),
        topic_key: round.manifest.topic.clone(),
        title: round.brief.title.clone(),
        manifest_hash: round.manifest_hash.clone(),
        snapshot_version: round.manifest_hash.clone(),
    }
}

fn served_cursor(rounds: &[crate::Round]) -> Result<MachineCursor, DomainError> {
    let identity = rounds
        .iter()
        .map(|round| (&round.id, &round.manifest_hash))
        .collect::<Vec<_>>();
    let digest = Sha256::digest(serde_json::to_vec(&identity).expect("cursor input serializes"));
    MachineCursor::new(format!("{digest:x}"))
}

pub fn snapshot_from_round(round: &crate::Round) -> Result<MachineSnapshot, DomainError> {
    let materialized = crate::diff::materialize_round(round)?;
    let mut files = Vec::new();
    for repository in &materialized.repositories {
        for file in &repository.files {
            let path = file
                .new_path
                .as_deref()
                .or(file.old_path.as_deref())
                .unwrap_or_default();
            let base_content_base64 = file
                .old_path
                .as_deref()
                .map(|path| {
                    crate::diff::materialize_file(round, &repository.repository_id, path, "left")
                })
                .transpose()?
                .map(|content| content.content_base64);
            let head_content_base64 = file
                .new_path
                .as_deref()
                .map(|path| {
                    crate::diff::materialize_file(round, &repository.repository_id, path, "right")
                })
                .transpose()?
                .map(|content| content.content_base64);
            files.push(MachineSnapshotFile {
                repository_id: repository.repository_id.clone(),
                workspace_relative_path: path.into(),
                status: match file.status {
                    DiffFileStatus::Added => "added",
                    DiffFileStatus::Deleted => "deleted",
                    DiffFileStatus::Modified => "modified",
                }
                .into(),
                base_blob_sha: file.old_blob_sha.clone().unwrap_or_else(|| "0".repeat(40)),
                head_blob_sha: file.new_blob_sha.clone().unwrap_or_else(|| "0".repeat(40)),
                unified_diff: file.patch.clone(),
                is_binary: file.is_binary,
                base_content_base64,
                head_content_base64,
                materialized: Some(file.clone()),
            });
        }
    }
    Ok(MachineSnapshot {
        source_item_id: round.id.clone(),
        snapshot_version: round.manifest_hash.clone(),
        manifest: round.manifest.clone(),
        files,
    })
}

pub fn materialize_snapshot(snapshot: &MachineSnapshot) -> MaterializedDiff {
    let repositories = snapshot
        .manifest
        .repositories
        .iter()
        .map(|repository| RepositoryDiff {
            repository_id: repository.repository_id.clone(),
            root: repository.root.clone(),
            base_sha: repository.base_sha.clone(),
            head_sha: repository.head_sha.clone(),
            files: snapshot
                .files
                .iter()
                .filter(|file| file.repository_id == repository.repository_id)
                .map(|file| {
                    file.materialized.clone().unwrap_or_else(|| DiffFile {
                        repository_id: file.repository_id.clone(),
                        old_path: (file.status != "added")
                            .then(|| file.workspace_relative_path.clone()),
                        new_path: (file.status != "deleted")
                            .then(|| file.workspace_relative_path.clone()),
                        old_blob_sha: (file.status != "added").then(|| file.base_blob_sha.clone()),
                        new_blob_sha: (file.status != "deleted")
                            .then(|| file.head_blob_sha.clone()),
                        status: match file.status.as_str() {
                            "added" => DiffFileStatus::Added,
                            "deleted" => DiffFileStatus::Deleted,
                            _ => DiffFileStatus::Modified,
                        },
                        is_binary: file.is_binary,
                        patch: file.unified_diff.clone(),
                        hunks: Vec::new(),
                    })
                })
                .collect(),
        })
        .collect();
    MaterializedDiff { repositories }
}

pub fn materialize_snapshot_file(
    snapshot: &MachineSnapshot,
    repository_id: &str,
    path: &str,
    side: &str,
) -> Result<PinnedFileContent, DomainError> {
    let file = snapshot
        .files
        .iter()
        .find(|file| file.repository_id == repository_id && file.workspace_relative_path == path)
        .ok_or_else(|| {
            error(
                "The requested file is not part of this cached machine snapshot.",
                "No remote request was made and no review state was changed.",
                "Choose a file from the machine round.",
                "machine_snapshot_file_not_found",
            )
        })?;
    let (blob_sha, encoded) = match side.to_ascii_lowercase().as_str() {
        "left" | "base" => (&file.base_blob_sha, file.base_content_base64.as_deref()),
        "right" | "head" => (&file.head_blob_sha, file.head_content_base64.as_deref()),
        _ => {
            return Err(error(
                "The requested machine snapshot side is invalid.",
                "No remote request was made and no review state was changed.",
                "Choose LEFT/base or RIGHT/head.",
                "invalid_review_side",
            ));
        }
    };
    let encoded = encoded.ok_or_else(|| {
        error(
            "That file does not exist on the requested snapshot side.",
            "No remote request was made and no review state was changed.",
            "Choose a side where the file exists.",
            "pinned_file_not_found",
        )
    })?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| {
            protocol_error(
                "The cached machine file blob is invalid.",
                "Reconnect and materialize the round again.",
                "machine_blob_invalid",
            )
        })?;
    let content = String::from_utf8(bytes).ok();
    Ok(PinnedFileContent {
        repository_id: repository_id.into(),
        path: path.into(),
        side: side.to_ascii_uppercase(),
        blob_sha: blob_sha.clone(),
        is_binary: content.is_none(),
        content,
        content_base64: encoded.into(),
    })
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MachineReproductionPreview {
    pub destination: String,
    pub file_count: usize,
    pub writes_original_workspace: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MachineReproductionResult {
    pub destination: String,
    pub file_count: usize,
}

pub fn preview_snapshot_reproduction(
    snapshot: &MachineSnapshot,
    destination: impl AsRef<Path>,
) -> Result<MachineReproductionPreview, DomainError> {
    let destination = destination.as_ref();
    if !destination.is_absolute() || destination.exists() {
        return Err(error(
            "Machine reproduction needs a new absolute destination.",
            "No directory or source file was created.",
            "Choose a clean path that does not already exist.",
            "machine_reproduction_destination_not_clean",
        ));
    }
    Ok(MachineReproductionPreview {
        destination: destination.to_string_lossy().into_owned(),
        file_count: snapshot
            .files
            .iter()
            .filter(|file| file.head_content_base64.is_some())
            .count(),
        writes_original_workspace: false,
    })
}

pub fn reproduce_snapshot(
    snapshot: &MachineSnapshot,
    destination: impl AsRef<Path>,
) -> Result<MachineReproductionResult, DomainError> {
    let preview = preview_snapshot_reproduction(snapshot, &destination)?;
    let destination = destination.as_ref();
    let parent = destination.parent().ok_or_else(|| {
        error(
            "The reproduction destination has no parent directory.",
            "No directory or source file was created.",
            "Choose a new absolute destination under an existing directory.",
            "machine_reproduction_parent_required",
        )
    })?;
    let staging = tempfile::Builder::new()
        .prefix(".review-queue-machine-")
        .tempdir_in(parent)
        .map_err(|_| {
            error(
                "Review Queue could not create a staging directory for reproduction.",
                "The original workspace and cached snapshot are unchanged.",
                "Check destination permissions and retry.",
                "machine_reproduction_staging_failed",
            )
        })?;
    for file in &snapshot.files {
        let Some(encoded) = &file.head_content_base64 else {
            continue;
        };
        let repository = snapshot
            .manifest
            .repositories
            .iter()
            .find(|repository| repository.repository_id == file.repository_id)
            .ok_or_else(|| {
                protocol_error(
                    "The cached machine file references an unknown repository.",
                    "Reconnect and materialize the round again.",
                    "machine_snapshot_repository_mismatch",
                )
            })?;
        let root = safe_reproduction_root(repository);
        let output = staging
            .path()
            .join(root)
            .join(&file.workspace_relative_path);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|_| {
                error(
                    "Review Queue could not create a reproduced source directory.",
                    "The original workspace and cached snapshot are unchanged.",
                    "Check destination permissions and retry.",
                    "machine_reproduction_write_failed",
                )
            })?;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| {
                protocol_error(
                    "The cached machine file blob is invalid.",
                    "Reconnect and materialize the round again.",
                    "machine_blob_invalid",
                )
            })?;
        fs::write(output, bytes).map_err(|_| {
            error(
                "Review Queue could not write a reproduced source file.",
                "The original workspace and cached snapshot are unchanged.",
                "Check destination permissions and retry.",
                "machine_reproduction_write_failed",
            )
        })?;
    }
    let staging_path = staging.keep();
    fs::rename(&staging_path, destination).map_err(|_| {
        let _ = fs::remove_dir_all(&staging_path);
        error(
            "Review Queue could not finalize the reproduced workspace.",
            "The original workspace and cached snapshot are unchanged.",
            "Choose another clean destination and retry.",
            "machine_reproduction_finalize_failed",
        )
    })?;
    Ok(MachineReproductionResult {
        destination: preview.destination,
        file_count: preview.file_count,
    })
}

fn safe_reproduction_root(repository: &crate::RepositorySnapshot) -> PathBuf {
    let root = Path::new(&repository.root);
    if !root.is_absolute()
        && !root
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return root.into();
    }
    PathBuf::from(
        repository
            .repository_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>(),
    )
}

/// Rejects credential-bearing fields and recognizable secret values from
/// machine config and every request/response envelope.
pub fn validate_token_free<T: Serialize>(value: &T) -> Result<(), DomainError> {
    let value = serde_json::to_value(value).expect("machine protocol value is serializable");
    scan_json(&value)
}

fn scan_json(value: &serde_json::Value) -> Result<(), DomainError> {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let key = key.to_ascii_lowercase();
                let forbidden = [
                    "credential",
                    "credentials",
                    "access_token",
                    "refresh_token",
                    "github_token",
                    "copilot_token",
                    "password",
                    "passphrase",
                    "private_key",
                    "identity_file",
                ];
                if forbidden.contains(&key.as_str()) {
                    return Err(secret_error());
                }
                scan_json(value)?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                scan_json(value)?;
            }
        }
        serde_json::Value::String(value) => {
            let lower = value.to_ascii_lowercase();
            let token_shaped = lower.contains("ghp_")
                || lower.contains("github_pat_")
                || lower.contains("bearer ")
                || lower.contains("authorization:")
                || lower.contains("-----begin private key-----")
                || lower.contains("-----begin openssh private key-----");
            if token_shaped {
                return Err(secret_error());
            }
        }
        _ => {}
    }
    Ok(())
}

fn secret_error() -> DomainError {
    error(
        "Connected-machine data contained credential-shaped material.",
        "The machine configuration or protocol message was rejected before transport.",
        "Remove credentials; use the existing SSH config and agent, then retry.",
        "machine_credentials_forbidden",
    )
}

fn validate_name(name: &str) -> Result<(), DomainError> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 80
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
    {
        return Err(error(
            "The connected-machine name is invalid.",
            "No machine configuration was saved.",
            "Use 1–80 letters, numbers, spaces, dots, dashes, or underscores.",
            "machine_name_invalid",
        ));
    }
    Ok(())
}

fn validate_ssh_target(target: &str) -> Result<(), DomainError> {
    if target.trim() != target
        || target.is_empty()
        || target.len() > 255
        || target.starts_with('-')
        || target.contains("://")
        || target.contains(':')
        || target.contains('/')
        || target.contains('?')
        || target.contains('#')
        || target.chars().any(char::is_whitespace)
        || target.matches('@').count() > 1
        || !target
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@'))
    {
        return Err(error(
            "The connected-machine SSH target is invalid.",
            "No machine configuration was saved and SSH was not started.",
            "Use an OpenSSH config host, optionally as user@host; put ports and keys in SSH config.",
            "machine_ssh_target_invalid",
        ));
    }
    Ok(())
}

fn validate_socket_path(value: &str, label: &str) -> Result<(), DomainError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 1024
        || !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::CurDir | Component::Prefix(_)
            )
        })
    {
        return Err(error(
            format!("The connected-machine {label} path is invalid."),
            "No machine configuration was saved.",
            "Use an absolute Unix-socket path without '.' or '..' components.",
            "machine_endpoint_invalid",
        ));
    }
    Ok(())
}

fn validate_version(version: u32) -> Result<(), DomainError> {
    if version == MACHINE_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(protocol_error(
            "The connected machine uses an incompatible protocol version.",
            "Upgrade the desktop app or remote daemon so their protocol versions match.",
            "machine_protocol_version_mismatch",
        ))
    }
}

fn validate_health(health: &MachineHealth) -> Result<(), DomainError> {
    validate_version(health.protocol_version)?;
    validate_id(&health.daemon_version, "daemon version")?;
    health.cursor.validate()?;
    validate_token_free(health)
}

fn validate_index(index: &MachineItemIndex) -> Result<(), DomainError> {
    index.cursor.validate()?;
    let mut ids = BTreeSet::new();
    let mut identities = BTreeSet::new();
    for item in &index.items {
        validate_summary(item)?;
        if !ids.insert(&item.source_item_id)
            || !identities.insert((&item.remote_workspace_id, &item.topic_key))
        {
            return Err(protocol_error(
                "The machine item index contains duplicate identities.",
                "Refresh after repairing or upgrading the remote daemon.",
                "machine_index_duplicate",
            ));
        }
    }
    validate_token_free(index)
}

fn validate_summary(item: &MachineItemSummary) -> Result<(), DomainError> {
    validate_id(&item.source_item_id, "source item ID")?;
    validate_id(&item.remote_workspace_id, "remote workspace ID")?;
    validate_id(&item.topic_key, "topic key")?;
    validate_id(&item.manifest_hash, "manifest hash")?;
    validate_id(&item.snapshot_version, "snapshot version")?;
    if item.title.trim().is_empty() || item.remote_workspace_path.trim().is_empty() {
        return Err(protocol_error(
            "A machine queue item is missing its title or remote workspace path.",
            "Refresh after repairing or upgrading the remote daemon.",
            "machine_item_invalid",
        ));
    }
    validate_token_free(item)
}

fn validate_detail(detail: &MachineItemDetail) -> Result<(), DomainError> {
    validate_summary(&detail.summary)?;
    validate_token_free(detail)
}

fn validate_snapshot(snapshot: &MachineSnapshot) -> Result<(), DomainError> {
    validate_id(&snapshot.source_item_id, "source item ID")?;
    validate_id(&snapshot.snapshot_version, "snapshot version")?;
    let mut files = BTreeSet::new();
    for file in &snapshot.files {
        validate_id(&file.repository_id, "repository ID")?;
        validate_id(&file.base_blob_sha, "base blob SHA")?;
        validate_id(&file.head_blob_sha, "head blob SHA")?;
        if file.workspace_relative_path.trim().is_empty()
            || file.workspace_relative_path.starts_with('/')
            || file
                .workspace_relative_path
                .split('/')
                .any(|component| component == "..")
            || !files.insert((&file.repository_id, &file.workspace_relative_path))
        {
            return Err(protocol_error(
                "The machine snapshot contains an invalid or duplicate file path.",
                "Refresh after repairing or upgrading the remote daemon.",
                "machine_snapshot_invalid",
            ));
        }
    }
    validate_token_free(snapshot)
}

fn validate_id(value: &str, label: &str) -> Result<(), DomainError> {
    if value.trim().is_empty()
        || value.len() > 512
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(protocol_error(
            format!("The machine returned an invalid {label}."),
            "Refresh after repairing or upgrading the remote daemon.",
            "machine_protocol_value_invalid",
        ));
    }
    validate_token_free(&value)
}

fn unexpected_response(expected: &str) -> DomainError {
    protocol_error(
        format!("The machine returned the wrong response to the {expected} request."),
        "Reconnect after checking or upgrading the remote daemon.",
        "machine_protocol_response_mismatch",
    )
}

fn protocol_error(
    what: impl Into<String>,
    next: impl Into<String>,
    code: impl Into<String>,
) -> DomainError {
    error(
        what,
        "The response was rejected and the previous local cache was preserved.",
        next,
        code,
    )
}

fn error(
    what: impl Into<String>,
    safety: impl Into<String>,
    next: impl Into<String>,
    code: impl Into<String>,
) -> DomainError {
    DomainError::actionable(what, safety, next, code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RepositorySnapshot, WorkspaceManifest};
    use chrono::{Duration, TimeZone};

    fn config() -> MachineConfig {
        MachineConfig {
            name: "Build Mac".into(),
            endpoint: MachineEndpoint::Ssh {
                target: "review-build".into(),
                remote_socket:
                    "/Users/reviewer/Library/Application Support/Review Queue/daemon.sock".into(),
                adapter: SshAdapter::SystemOpenSsh,
            },
            source_type: MachineSourceType::ReviewQueueDaemon,
        }
    }

    fn summary() -> MachineItemSummary {
        MachineItemSummary {
            source_item_id: "remote-round-1".into(),
            remote_workspace_id: "workspace-1".into(),
            remote_workspace_path: "/work/project".into(),
            topic_key: "feature".into(),
            title: "Review feature".into(),
            manifest_hash: "manifest-1".into(),
            snapshot_version: "snapshot-1".into(),
        }
    }

    fn fixture() -> LoopbackFakeTransport {
        let cursor = MachineCursor::new("cursor-1").unwrap();
        let item = summary();
        let manifest = WorkspaceManifest {
            workspace_id: "workspace-1".into(),
            workspace_root: "/work/project".into(),
            topic: "feature".into(),
            repositories: vec![RepositorySnapshot {
                repository_id: "repo".into(),
                root: ".".into(),
                branch: "feature".into(),
                base_sha: "base".into(),
                head_sha: "head".into(),
                remote_fingerprint: None,
                object_checksum: "checksum".into(),
            }],
            before_fingerprint: "before".into(),
            after_fingerprint: "after".into(),
            created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        };
        LoopbackFakeTransport::new(
            MachineHealth {
                protocol_version: MACHINE_PROTOCOL_VERSION,
                daemon_version: "1.0.0".into(),
                state: MachineHealthState::Healthy,
                cursor: cursor.clone(),
            },
            MachineItemIndex {
                cursor,
                items: vec![item.clone()],
            },
            vec![MachineItemDetail {
                summary: item,
                description: "A remote review".into(),
                repository_count: 1,
                updated_at: Utc.timestamp_opt(1_700_000_100, 0).unwrap(),
                origin_route: None,
            }],
            vec![MachineSnapshot {
                source_item_id: "remote-round-1".into(),
                snapshot_version: "snapshot-1".into(),
                manifest,
                files: vec![MachineSnapshotFile {
                    repository_id: "repo".into(),
                    workspace_relative_path: "src/lib.rs".into(),
                    status: "modified".into(),
                    base_blob_sha: "base-blob".into(),
                    head_blob_sha: "head-blob".into(),
                    unified_diff: "@@ -1 +1 @@".into(),
                    is_binary: false,
                    base_content_base64: None,
                    head_content_base64: None,
                    materialized: None,
                }],
            }],
        )
        .unwrap()
    }

    #[test]
    fn validates_config_fields_without_accepting_ssh_material() {
        config().validate().unwrap();

        let mut bad = config();
        bad.name = "prod\nmac".into();
        assert_eq!(
            bad.validate().unwrap_err().error.code,
            "machine_name_invalid"
        );

        let mut bad = config();
        if let MachineEndpoint::Ssh { target, .. } = &mut bad.endpoint {
            *target = "-oIdentityFile=/tmp/key".into();
        }
        assert_eq!(
            bad.validate().unwrap_err().error.code,
            "machine_ssh_target_invalid"
        );

        let mut bad = config();
        if let MachineEndpoint::Ssh { remote_socket, .. } = &mut bad.endpoint {
            *remote_socket = "/tmp/../secret".into();
        }
        assert_eq!(
            bad.validate().unwrap_err().error.code,
            "machine_endpoint_invalid"
        );

        let credential_bearing = serde_json::json!({
            "name": "Build Mac",
            "endpoint": {
                "kind": "ssh",
                "target": "review-build",
                "remote_socket": "/tmp/review-queue.sock",
                "adapter": "system_open_ssh",
                "passphrase": "must-not-cross-ipc"
            },
            "source_type": "review_queue_daemon"
        });
        assert!(
            serde_json::from_value::<MachineConfig>(credential_bearing).is_err(),
            "unknown credential fields must be rejected at deserialization"
        );
    }

    #[test]
    fn registry_add_is_idempotent_and_identity_is_stable() {
        let mut registry = MachineRegistry::default();
        let id = match registry.add(config()).unwrap() {
            AddMachineOutcome::Added(record) => record.id,
            _ => panic!("first add must create"),
        };
        let repeated = registry.add(config()).unwrap();
        assert!(matches!(repeated, AddMachineOutcome::Existing(_)));
        assert_eq!(registry.records().count(), 1);
        assert_eq!(registry.get(&id).unwrap().config.name, "Build Mac");

        let mut collision = config();
        collision.name = "Another name".into();
        assert_eq!(
            registry.add(collision).unwrap_err().error.code,
            "machine_endpoint_collision"
        );
    }

    /// A connected machine supplies immutable cached blobs, not a usable
    /// local path. Copilot therefore receives a clean, app-owned directory
    /// even if a remote snapshot advertises an absolute source checkout.
    #[test]
    fn copilot_machine_materialization_never_uses_remote_workspace_as_cwd() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("copilot-session");
        let remote_workspace = "/Volumes/build-agent/private/project";
        let snapshot = MachineSnapshot {
            source_item_id: "remote-round-copilot".into(),
            snapshot_version: "snapshot-copilot-1".into(),
            manifest: WorkspaceManifest {
                workspace_id: "remote-workspace".into(),
                workspace_root: remote_workspace.into(),
                topic: "feature/cached-review".into(),
                repositories: vec![RepositorySnapshot {
                    repository_id: "machine/repo".into(),
                    root: remote_workspace.into(),
                    branch: "feature/cached-review".into(),
                    base_sha: "base".into(),
                    head_sha: "head".into(),
                    remote_fingerprint: None,
                    object_checksum: "head".into(),
                }],
                before_fingerprint: "base".into(),
                after_fingerprint: "head".into(),
                created_at: Utc::now(),
            },
            files: vec![MachineSnapshotFile {
                repository_id: "machine/repo".into(),
                workspace_relative_path: "src/review.rs".into(),
                status: "modified".into(),
                base_blob_sha: "base-blob".into(),
                head_blob_sha: "head-blob".into(),
                unified_diff: "-before\n+after".into(),
                is_binary: false,
                base_content_base64: Some("YmVmb3Jl".into()),
                head_content_base64: Some("YWZ0ZXI=".into()),
                materialized: None,
            }],
        };

        let preview = preview_snapshot_reproduction(&snapshot, &destination).unwrap();
        assert!(!preview.writes_original_workspace);
        reproduce_snapshot(&snapshot, &destination).unwrap();

        assert_eq!(
            std::fs::read_to_string(destination.join("machine_repo/src/review.rs")).unwrap(),
            "after"
        );
        assert!(!destination.join("Volumes").exists());
        assert!(
            !destination.to_string_lossy().contains(remote_workspace),
            "the clean Copilot cwd must not be a connected-machine path"
        );
    }

    #[test]
    fn item_detail_and_snapshot_are_lazy_explicit_fetches() {
        let transport = fixture();
        let mut client = MachineClient::new(config(), transport).unwrap();
        assert_eq!(client.transport().counters(), LoopbackCounters::default());
        assert!(client.cached_index().is_none());

        let now = Utc.timestamp_opt(1_700_000_200, 0).unwrap();
        client.fetch_index(now).unwrap();
        assert_eq!(client.transport().counters().item_index, 1);
        assert_eq!(client.transport().counters().item_detail, 0);
        assert_eq!(client.transport().counters().snapshot, 0);
        assert!(client.cached_detail("remote-round-1").is_none());

        client.fetch_item_detail("remote-round-1", now).unwrap();
        assert_eq!(client.transport().counters().item_detail, 1);
        assert_eq!(client.transport().counters().snapshot, 0);

        client
            .fetch_snapshot("remote-round-1", "snapshot-1", now)
            .unwrap();
        assert_eq!(client.transport().counters().snapshot, 1);
        assert!(
            client
                .cached_snapshot("remote-round-1", "snapshot-1")
                .is_some()
        );
        // Cache reads perform no transport operation.
        let before = client.transport().counters();
        let _ = client.cached_index();
        let _ = client.cached_detail("remote-round-1");
        assert_eq!(client.transport().counters(), before);
    }

    #[test]
    fn reports_cached_cursor_and_nonnegative_age() {
        let mut client = MachineClient::new(config(), fixture()).unwrap();
        let cached_at = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        assert_eq!(
            client.index_freshness(cached_at),
            CacheFreshness {
                cached: false,
                cursor: None,
                cached_at: None,
                age_seconds: None,
            }
        );
        client.fetch_index(cached_at).unwrap();
        let freshness = client.index_freshness(cached_at + Duration::seconds(37));
        assert_eq!(freshness.age_seconds, Some(37));
        assert_eq!(freshness.cursor.unwrap().version, "cursor-1");
        assert_eq!(
            client
                .index_freshness(cached_at - Duration::seconds(4))
                .age_seconds,
            Some(0)
        );
    }

    #[test]
    fn tunnel_failure_is_actionable_and_retry_recovers() {
        let mut transport = fixture();
        transport.fail_next_tunnel();
        let mut client = MachineClient::new(config(), transport).unwrap();
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let failure = client.fetch_index(now).unwrap_err();
        assert_eq!(failure.error.code, "machine_tunnel_failed");
        assert!(failure.error.next_step.contains("reconnect"));
        assert!(client.cached_index().is_none());

        let recovered = client.fetch_index(now).unwrap();
        assert_eq!(recovered.items.len(), 1);
        assert_eq!(client.transport().counters().item_index, 1);
    }

    #[test]
    fn token_scan_rejects_config_and_protocol_secrets() {
        let mut bad = config();
        if let MachineEndpoint::Ssh { target, .. } = &mut bad.endpoint {
            *target = "ghp_not-a-real-token".into();
        }
        assert!(matches!(
            bad.validate().unwrap_err().error.code.as_str(),
            "machine_credentials_forbidden" | "machine_ssh_target_invalid"
        ));

        let mut index = fixture().index;
        index.items[0].title = "Authorization: Bearer leaked-value".into();
        assert_eq!(
            validate_index(&index).unwrap_err().error.code,
            "machine_credentials_forbidden"
        );

        #[derive(Serialize)]
        struct ForbiddenField<'a> {
            access_token: &'a str,
        }
        assert_eq!(
            validate_token_free(&ForbiddenField {
                access_token: "opaque"
            })
            .unwrap_err()
            .error
            .code,
            "machine_credentials_forbidden"
        );
    }
}
