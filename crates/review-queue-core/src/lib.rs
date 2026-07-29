//! Token-free durable domain core for Review Queue.
//!
//! This crate intentionally has no Keychain or network client. The signed
//! desktop shell owns credentials and invokes this core with normalized data.

pub mod acp;
pub mod adapters;
pub mod capture;
pub mod copilot;
pub mod diff;
pub mod github;
pub mod machine;
pub mod reproduction;
pub mod socket;
pub mod store;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReviewBrief {
    pub title: String,
    #[serde(default)]
    pub what: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub approach_alternatives: String,
    #[serde(default)]
    pub testing: String,
}

impl ReviewBrief {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.title.trim().is_empty() {
            return Err(DomainError::actionable(
                "A review title is required.",
                "The review brief is preserved.",
                "Add a Title and retry capture.",
                "title_required",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RepositorySnapshot {
    pub repository_id: String,
    pub root: String,
    pub branch: String,
    pub base_sha: String,
    pub head_sha: String,
    #[serde(default)]
    pub remote_fingerprint: Option<String>,
    #[serde(default)]
    pub object_checksum: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkspaceManifest {
    pub workspace_id: String,
    pub workspace_root: String,
    pub topic: String,
    pub repositories: Vec<RepositorySnapshot>,
    pub before_fingerprint: String,
    pub after_fingerprint: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Collection {
    Local,
    Github,
    Machine,
}

impl Collection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Github => "github",
            Self::Machine => "machine",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Queued,
    ChangesRequested,
    Completed,
}

impl Lifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::ChangesRequested => "changes_requested",
            Self::Completed => "completed",
        }
    }
    pub fn active(self) -> bool {
        !matches!(self, Self::Completed)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    RequestChanges,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentRoute {
    pub id: String,
    pub adapter_kind: String,
    pub agent_id: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub status: String,
    pub last_heartbeat: DateTime<Utc>,
    /// Versioned, token-free origin metadata. Older route records deserialize
    /// without it and remain valid.
    #[serde(default)]
    pub provenance: Option<Box<AgentRouteProvenance>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentRouteProvenance {
    #[serde(default)]
    pub schema_version: Option<u32>,
    #[serde(default)]
    pub adapter_version: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub provider_version: Option<String>,
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub original_cwd: Option<String>,
    #[serde(default)]
    pub cmux_workspace: Option<String>,
    #[serde(default)]
    pub cmux_surface: Option<String>,
    #[serde(default)]
    pub reconnect_recipe: Option<String>,
    #[serde(default)]
    pub provider_resume_handle: Option<String>,
    #[serde(default)]
    pub transcript_reference: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub last_turn: Option<AgentLastTurnMetadata>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentLastTurnMetadata {
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MachineRecord {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Submission {
    pub collection: Collection,
    /// Stable source identity: workspace+topic, host/repo/PR, or machine+workspace+topic.
    pub topic_identity: String,
    pub brief: ReviewBrief,
    pub manifest: WorkspaceManifest,
    #[serde(default)]
    pub origin_route: Option<AgentRoute>,
    /// Source-specific provenance kept with the immutable review round.
    #[serde(default)]
    pub source_metadata: Option<SourceMetadata>,
}

/// Canonical stable identity shared by every local ingestion surface.
///
/// `workspace_id` is produced by capture from the canonical workspace path;
/// using it here prevents the desktop sheet and CLI from creating parallel
/// topics for the same workspace.
pub fn local_topic_identity(manifest: &WorkspaceManifest) -> String {
    format!("{}:{}", manifest.workspace_id, manifest.topic)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceMetadata {
    Github {
        host: String,
        owner: String,
        repository: String,
        pull_number: u64,
        base_sha: String,
        head_sha: String,
        state: adapters::GithubPullRequestState,
        is_draft: bool,
        #[serde(default)]
        staleness: Option<adapters::StalenessStatus>,
    },
    Machine {
        machine_id: String,
        machine_name: String,
        source_item_id: String,
        remote_workspace_id: String,
        remote_workspace_path: String,
        cursor: String,
        cached_at: DateTime<Utc>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Round {
    pub id: String,
    pub collection: Collection,
    pub topic_identity: String,
    pub manifest_hash: String,
    pub brief: ReviewBrief,
    pub manifest: WorkspaceManifest,
    pub rank: i64,
    pub lifecycle: Lifecycle,
    pub superseded_by: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub origin_route_id: Option<String>,
    /// Immutable token-free snapshot of the route that originated this round.
    ///
    /// `origin_route_id` remains useful for current liveness/heartbeat lookup,
    /// while this value preserves the capture-time provenance even if that
    /// registered route is later updated or removed.
    #[serde(default)]
    pub origin_route: Option<AgentRoute>,
    #[serde(default)]
    pub source_metadata: Option<SourceMetadata>,
}

/// A formal draft is intentionally separate from an AskTurn. Only these
/// records can enter a delivery or publish payload.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FormalComment {
    pub id: String,
    pub thread_id: String,
    pub body: String,
    #[serde(default)]
    pub anchor: Option<Anchor>,
    pub revision: i64,
    #[serde(default)]
    pub delivered_revision: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Anchor {
    pub repository_id: String,
    pub workspace_relative_path: String,
    pub side: String,
    pub start_line: u32,
    pub end_line: u32,
    pub blob_sha: String,
    pub selected_code: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DeliveryPayload {
    pub round_id: String,
    pub decision: Decision,
    pub comments: Vec<FormalComment>,
}

/// A locally persisted immutable feedback handoff. The desktop derives a
/// copyable prompt from it; only the user submits that prompt to an agent.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DurableDelivery {
    pub id: String,
    pub idempotency_key: String,
    pub payload: DeliveryPayload,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ActionableError {
    pub code: String,
    pub what_happened: String,
    pub data_safety: String,
    pub next_step: String,
}

#[derive(Debug)]
pub struct DomainError {
    pub error: ActionableError,
}

impl std::fmt::Display for ActionableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} Next: {}",
            self.what_happened, self.data_safety, self.next_step
        )
    }
}

impl std::error::Error for ActionableError {}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for DomainError {}

impl DomainError {
    pub fn actionable(
        what: impl Into<String>,
        safety: impl Into<String>,
        next: impl Into<String>,
        code: impl Into<String>,
    ) -> Self {
        Self {
            error: ActionableError {
                code: code.into(),
                what_happened: what.into(),
                data_safety: safety.into(),
                next_step: next.into(),
            },
        }
    }
}

pub fn manifest_hash(manifest: &WorkspaceManifest) -> String {
    use sha2::{Digest, Sha256};
    // Capture time describes the envelope, not the immutable source state. It
    // must not turn an unchanged resubmission into a new review round.
    let mut source_projection = manifest.clone();
    source_projection.created_at = DateTime::<Utc>::UNIX_EPOCH;
    let canonical = serde_json::to_vec(&source_projection).expect("manifest is serializable");
    format!("{:x}", Sha256::digest(canonical))
}

/// Diagnostics must fail closed when a likely credential slips into input.
pub fn redact_for_diagnostics(value: &str) -> Result<String, DomainError> {
    let token_markers = ["ghp_", "github_pat_", "oauth", "bearer ", "eyJ"];
    if token_markers
        .iter()
        .any(|m| value.to_ascii_lowercase().contains(&m.to_ascii_lowercase()))
    {
        return Err(DomainError::actionable(
            "Diagnostics contained a token-shaped value.",
            "No diagnostic was exported.",
            "Remove the secret and generate diagnostics again.",
            "token_shaped_diagnostic",
        ));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn local_topic_identity_depends_only_on_canonical_workspace_and_topic() {
        let manifest = WorkspaceManifest {
            workspace_id: "workspace-abc".into(),
            workspace_root: "/canonical/workspace".into(),
            topic: "parser-v2".into(),
            repositories: Vec::new(),
            before_fingerprint: "before".into(),
            after_fingerprint: "after".into(),
            created_at: Utc::now(),
        };
        assert_eq!(local_topic_identity(&manifest), "workspace-abc:parser-v2");
    }
}
