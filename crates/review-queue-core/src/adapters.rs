//! Token-free contracts shared by source, Copilot, GitHub, remote-machine,
//! and ACP adapters.  Implementations own I/O; this module only makes their
//! inputs, preconditions, and durable outputs explicit.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Anchor, Collection, Decision, DomainError};

/// Actions a source can truthfully expose to the common reviewer.  The UI
/// consumes this declaration instead of branching on a source implementation.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SourceCapability {
    OriginatingAgent,
    /// Formal feedback can be handed back to the originating agent. This is
    /// deliberately a source capability, not a queue-collection heuristic.
    #[serde(rename = "acp_delivery", alias = "manual_feedback_handoff")]
    AcpDelivery,
    Publish,
    UpstreamDiscussion,
    RemoteRefresh,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct CapabilitySet {
    pub capabilities: Vec<SourceCapability>,
}

impl CapabilitySet {
    pub fn supports(&self, capability: SourceCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let mut unique = self.capabilities.clone();
        unique.sort_unstable();
        unique.dedup();
        if unique.len() != self.capabilities.len() {
            return Err(validation_error(
                "A source capability was declared more than once.",
                "The source configuration was not accepted.",
                "Remove the duplicate capability and retry.",
                "duplicate_source_capability",
            ));
        }
        Ok(())
    }
}

/// Stable declaration made by each local, GitHub, or connected-daemon source.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SourceAdapterContract {
    pub adapter_id: String,
    pub capabilities: CapabilitySet,
    /// Approval is source-owned: a local immutable snapshot is purged after
    /// confirmation, while upstream-backed sources retain a local decision.
    #[serde(default)]
    pub approval: ApprovalDisposition,
}

impl SourceAdapterContract {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_nonempty(&self.adapter_id, "source_adapter_id_required")?;
        self.capabilities.validate()?;
        validate_token_free_fields(vec![self.adapter_id.as_str()])
    }

    pub fn supports(&self, capability: SourceCapability) -> bool {
        self.capabilities.supports(capability)
    }

    pub fn require(&self, capability: SourceCapability, action: &str) -> Result<(), DomainError> {
        if self.supports(capability) {
            return Ok(());
        }
        Err(DomainError::actionable(
            format!("This source does not support {action}."),
            "No review state or source data was changed.",
            "Choose an action offered by this review source.",
            "source_capability_required",
        ))
    }

    /// Built-in contracts are created at ingress and persisted with the
    /// round. The collection remains a queue/ranking concern only; callers
    /// should use the persisted declaration for reviewer actions.
    pub fn legacy_for_collection(collection: Collection) -> Self {
        match collection {
            Collection::Local => Self {
                adapter_id: "local_workspace_snapshot".into(),
                capabilities: CapabilitySet {
                    capabilities: vec![
                        SourceCapability::OriginatingAgent,
                        SourceCapability::AcpDelivery,
                    ],
                },
                approval: ApprovalDisposition::PurgeRound,
            },
            Collection::Github => Self {
                adapter_id: "github_pull_request_mirror".into(),
                capabilities: CapabilitySet {
                    capabilities: vec![
                        SourceCapability::Publish,
                        SourceCapability::UpstreamDiscussion,
                        SourceCapability::RemoteRefresh,
                    ],
                },
                approval: ApprovalDisposition::RecordDecision,
            },
            Collection::Machine => Self {
                adapter_id: "connected_daemon_workspace".into(),
                capabilities: CapabilitySet {
                    capabilities: vec![
                        SourceCapability::OriginatingAgent,
                        SourceCapability::RemoteRefresh,
                    ],
                },
                approval: ApprovalDisposition::RecordDecision,
            },
        }
    }
}

impl Default for SourceAdapterContract {
    fn default() -> Self {
        // This serde default is only for legacy JSON. SQLite migration uses
        // `legacy_for_collection`, which preserves the original source.
        Self::legacy_for_collection(Collection::Local)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDisposition {
    PurgeRound,
    #[default]
    RecordDecision,
}

/// The public, token-free auth vocabulary.  Values deliberately describe
/// availability, never a credential, authorization URL, or device code.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatus {
    NotConnected,
    Connected,
    Expired,
    AccountMismatch,
    ValidationFailed,
    KeychainUnavailable,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AuthSessionStatus {
    pub capability: String,
    pub status: AuthStatus,
    #[serde(default)]
    pub account_label: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub recovery_action: Option<String>,
}

impl AuthSessionStatus {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_nonempty(&self.capability, "capability_required")?;
        validate_token_free_fields(self.all_metadata())
    }

    fn all_metadata(&self) -> Vec<&str> {
        let mut values = vec![self.capability.as_str()];
        if let Some(value) = &self.account_label {
            values.push(value);
        }
        if let Some(value) = &self.recovery_action {
            values.push(value);
        }
        values.extend(self.scopes.iter().map(String::as_str));
        values
    }
}

/// A provider's discovered configuration and public authentication state.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderAdapterContract {
    pub provider_id: String,
    pub authentication: AuthSessionStatus,
    #[serde(default)]
    pub session_options: Vec<DiscoveredSessionOption>,
}

impl ProviderAdapterContract {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_nonempty(&self.provider_id, "provider_adapter_id_required")?;
        self.authentication.validate()?;
        for option in &self.session_options {
            option.validate()?;
        }
        validate_token_free_fields(vec![self.provider_id.as_str()])
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionOptionKind {
    Select,
    Text,
    Boolean,
    Number,
}

/// An option discovered from the provider at connection time.  `supported`
/// and `unavailable_reason` prevent an adapter from pretending a choice took.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DiscoveredSessionOption {
    pub key: String,
    pub label: String,
    pub kind: SessionOptionKind,
    #[serde(default)]
    pub values: Vec<String>,
    #[serde(default)]
    pub selected: Option<String>,
    pub supported: bool,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
}

impl DiscoveredSessionOption {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_nonempty(&self.key, "session_option_key_required")?;
        require_nonempty(&self.label, "session_option_label_required")?;
        if self.supported && self.unavailable_reason.is_some() {
            return Err(validation_error(
                "A supported option cannot have an unavailable reason.",
                "No option change was applied.",
                "Refresh provider capabilities.",
                "contradictory_option_support",
            ));
        }
        if !self.supported
            && self
                .unavailable_reason
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
        {
            return Err(validation_error(
                "An unsupported option needs an explanation.",
                "No option change was applied.",
                "Provide the provider's unavailable reason.",
                "missing_option_unavailable_reason",
            ));
        }
        if matches!(self.kind, SessionOptionKind::Select)
            && self.supported
            && self.values.is_empty()
        {
            return Err(validation_error(
                "A selectable provider option has no available values.",
                "No option change was applied.",
                "Refresh provider capabilities.",
                "empty_select_option_values",
            ));
        }
        if let Some(selected) = &self.selected
            && matches!(self.kind, SessionOptionKind::Select)
            && !self.values.contains(selected)
        {
            return Err(validation_error(
                "The selected option value is not offered by the provider.",
                "No option change was applied.",
                "Select one of the discovered values.",
                "unknown_selected_option_value",
            ));
        }
        let mut metadata = vec![self.key.as_str(), self.label.as_str()];
        metadata.extend(self.values.iter().map(String::as_str));
        if let Some(value) = &self.selected {
            metadata.push(value);
        }
        if let Some(value) = &self.unavailable_reason {
            metadata.push(value);
        }
        validate_token_free_fields(metadata)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationSessionState {
    CanContinue,
    HistoryOnly,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AskConversation {
    pub id: String,
    pub round_id: String,
    pub session_state: ConversationSessionState,
    #[serde(default)]
    pub history_only_reason: Option<String>,
    #[serde(default)]
    pub provider_session_label: Option<String>,
    #[serde(default)]
    pub options: Vec<DiscoveredSessionOption>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub archived_at: Option<DateTime<Utc>>,
}

impl AskConversation {
    pub fn validate_prompt_allowed(&self) -> Result<(), DomainError> {
        if self.session_state == ConversationSessionState::HistoryOnly || self.archived_at.is_some()
        {
            return Err(validation_error(
                "This chat is history only and cannot accept another prompt.",
                "Its transcript and formal comments are preserved.",
                "Clear chat on the active round to start a fresh conversation.",
                "history_only_conversation",
            ));
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        require_nonempty(&self.id, "conversation_id_required")?;
        require_nonempty(&self.round_id, "conversation_round_required")?;
        if self.session_state == ConversationSessionState::HistoryOnly
            && self
                .history_only_reason
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
        {
            return Err(validation_error(
                "A history-only chat needs a visible reason.",
                "The transcript is preserved.",
                "Record why its provider session cannot continue.",
                "history_only_reason_required",
            ));
        }
        for option in &self.options {
            option.validate()?;
        }
        validate_token_free_fields(
            self.history_only_reason
                .iter()
                .chain(self.provider_session_label.iter())
                .map(String::as_str)
                .collect(),
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AskTurnState {
    Queued,
    Streaming,
    Completed,
    Cancelled,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AskTurn {
    pub id: String,
    pub conversation_id: String,
    pub idempotency_key: String,
    pub prompt: String,
    #[serde(default)]
    pub anchor: Option<Anchor>,
    /// Exact option values used for this prompt, not merely current choices.
    pub option_values: BTreeMap<String, String>,
    pub state: AskTurnState,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub failure_reason: Option<String>,
    /// Materialized text received so far. Chunks remain separately durable so
    /// an interrupted stream can be rendered without ever replaying a prompt.
    #[serde(default)]
    pub response_text: String,
}

impl AskTurn {
    pub fn validate_for_conversation(
        &self,
        conversation: &AskConversation,
    ) -> Result<(), DomainError> {
        if self.conversation_id != conversation.id {
            return Err(validation_error(
                "The prompt belongs to a different conversation.",
                "No prompt was sent and both transcripts are preserved.",
                "Create the prompt from the active conversation.",
                "ask_turn_conversation_mismatch",
            ));
        }
        conversation.validate_prompt_allowed()?;
        self.validate()
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        require_nonempty(&self.id, "ask_turn_id_required")?;
        require_nonempty(&self.conversation_id, "ask_turn_conversation_required")?;
        require_nonempty(&self.idempotency_key, "ask_turn_idempotency_required")?;
        require_nonempty(&self.prompt, "ask_turn_prompt_required")?;
        if matches!(self.state, AskTurnState::Failed | AskTurnState::Interrupted)
            && self
                .failure_reason
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
        {
            return Err(validation_error(
                "A failed or interrupted prompt needs a recovery reason.",
                "The prompt is preserved and was not resent.",
                "Record the provider failure and offer Retry as a new prompt.",
                "ask_turn_failure_reason_required",
            ));
        }
        let mut values = vec![
            self.id.as_str(),
            self.conversation_id.as_str(),
            self.idempotency_key.as_str(),
            self.prompt.as_str(),
        ];
        values.extend(
            self.option_values
                .iter()
                .flat_map(|(key, value)| [key.as_str(), value.as_str()]),
        );
        if let Some(reason) = &self.failure_reason {
            values.push(reason);
        }
        values.push(self.response_text.as_str());
        validate_token_free_fields(values)?;
        if let Some(anchor) = &self.anchor {
            validate_anchor(anchor)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubPullRequestMetadata {
    pub host: String,
    pub owner: String,
    pub repository: String,
    pub pull_number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub base_sha: String,
    pub head_sha: String,
    pub state: GithubPullRequestState,
    pub is_draft: bool,
    #[serde(default)]
    pub web_url: Option<String>,
}

impl GithubPullRequestMetadata {
    pub fn topic_identity(&self) -> String {
        format!(
            "{}/{}/{}#{}",
            self.host, self.owner, self.repository, self.pull_number
        )
    }
    pub fn validate(&self) -> Result<(), DomainError> {
        for value in [
            &self.host,
            &self.owner,
            &self.repository,
            &self.title,
            &self.base_sha,
            &self.head_sha,
        ] {
            require_nonempty(value, "github_metadata_required")?;
        }
        if self.pull_number == 0 {
            return Err(validation_error(
                "A pull request number must be positive.",
                "No GitHub mirror was created.",
                "Provide the pull request number.",
                "github_pr_number_required",
            ));
        }
        validate_token_free_fields(
            [
                self.host.as_str(),
                self.owner.as_str(),
                self.repository.as_str(),
                self.title.as_str(),
                self.body.as_str(),
                self.base_sha.as_str(),
                self.head_sha.as_str(),
            ]
            .into_iter()
            .chain(self.web_url.iter().map(String::as_str))
            .collect(),
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GithubPullRequestState {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportedCommentKind {
    #[default]
    Unknown,
    ReviewThreadComment,
    PullRequestComment,
    ReviewSummary,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ImportedComment {
    pub id: String,
    pub thread_id: String,
    pub body: String,
    pub upstream_author: String,
    pub upstream_created_at: DateTime<Utc>,
    pub source_url: String,
    #[serde(default)]
    pub kind: ImportedCommentKind,
    /// GitHub exposes resolution only for review threads. `None` means the
    /// upstream object has no resolution concept or legacy data predates it.
    #[serde(default)]
    pub upstream_resolved: Option<bool>,
    #[serde(default)]
    pub upstream_review_state: Option<String>,
    #[serde(default)]
    pub anchor: Option<Anchor>,
}

impl ImportedComment {
    pub fn validate(&self) -> Result<(), DomainError> {
        for value in [
            &self.id,
            &self.thread_id,
            &self.upstream_author,
            &self.source_url,
        ] {
            require_nonempty(value, "imported_comment_required")?;
        }
        if self.kind != ImportedCommentKind::ReviewSummary && self.body.trim().is_empty() {
            return Err(validation_error(
                "An imported discussion comment is empty.",
                "The upstream refresh was not persisted.",
                "Refresh the GitHub discussion again.",
                "imported_comment_required",
            ));
        }
        validate_token_free_fields(
            [
                self.id.as_str(),
                self.thread_id.as_str(),
                self.body.as_str(),
                self.upstream_author.as_str(),
                self.source_url.as_str(),
            ]
            .into_iter()
            .chain(self.upstream_review_state.iter().map(String::as_str))
            .collect(),
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct StalenessStatus {
    pub pinned_head_sha: String,
    pub observed_head_sha: String,
    pub checked_at: DateTime<Utc>,
}

impl StalenessStatus {
    pub fn is_stale(&self) -> bool {
        self.pinned_head_sha != self.observed_head_sha
    }
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_token_free_fields(vec![
            self.pinned_head_sha.as_str(),
            self.observed_head_sha.as_str(),
        ])
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GithubReviewEvent {
    Approve,
    RequestChanges,
}

pub fn publish_event_for_decision(decision: Decision) -> GithubReviewEvent {
    match decision {
        Decision::Approve => GithubReviewEvent::Approve,
        Decision::RequestChanges => GithubReviewEvent::RequestChanges,
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublishCommentDisposition {
    Inline,
    ReplyToImportedThread,
    ReviewBody,
    BodyFallback,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PublishPreviewComment {
    pub formal_comment_id: String,
    pub disposition: PublishCommentDisposition,
    #[serde(default)]
    pub fallback_reference: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PublishPreview {
    pub target: GithubPullRequestMetadata,
    pub decision: Decision,
    pub event: GithubReviewEvent,
    pub comments: Vec<PublishPreviewComment>,
}

impl PublishPreview {
    pub fn from_decision(
        target: GithubPullRequestMetadata,
        decision: Decision,
        comments: Vec<PublishPreviewComment>,
    ) -> Self {
        Self {
            target,
            decision,
            event: publish_event_for_decision(decision),
            comments,
        }
    }
    pub fn validate(&self) -> Result<(), DomainError> {
        self.target.validate()?;
        if self.event != publish_event_for_decision(self.decision) {
            return Err(validation_error(
                "The publish event does not match the recorded decision.",
                "No publish request was created.",
                "Rebuild the preview from the recorded decision.",
                "publish_event_decision_mismatch",
            ));
        }
        for comment in &self.comments {
            require_nonempty(&comment.formal_comment_id, "publish_comment_id_required")?;
            if comment.disposition == PublishCommentDisposition::BodyFallback
                && comment
                    .fallback_reference
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
            {
                return Err(validation_error(
                    "A body fallback must disclose its path and line reference.",
                    "No publish request was created.",
                    "Add the fallback reference to the preview.",
                    "publish_fallback_reference_required",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteMachineIndexEntry {
    pub machine_id: String,
    pub display_name: String,
    pub ssh_endpoint: String,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub health: RemoteMachineHealth,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteMachineHealth {
    Healthy,
    Degraded,
    Disconnected,
    Unreachable,
    InvalidConfiguration,
}

impl RemoteMachineIndexEntry {
    pub fn validate(&self) -> Result<(), DomainError> {
        for value in [&self.machine_id, &self.display_name, &self.ssh_endpoint] {
            require_nonempty(value, "remote_machine_field_required")?;
        }
        validate_token_free_fields(vec![
            self.machine_id.as_str(),
            self.display_name.as_str(),
            self.ssh_endpoint.as_str(),
        ])
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteCacheStatus {
    pub machine_id: String,
    pub cache_key: String,
    pub cached_at: DateTime<Utc>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    pub health_at_cache_time: RemoteMachineHealth,
}

impl RemoteCacheStatus {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_token_free_fields(vec![self.machine_id.as_str(), self.cache_key.as_str()])
    }
}

/// Reject data that might leak into a token-free queue, cache, daemon, or
/// adapter log.  This is intentionally shared by every contract above.
pub fn validate_token_free_fields(fields: Vec<&str>) -> Result<(), DomainError> {
    for field in fields {
        crate::redact_for_diagnostics(field)?;
    }
    Ok(())
}

fn require_nonempty(value: &str, code: &str) -> Result<(), DomainError> {
    if value.trim().is_empty() {
        return Err(validation_error(
            "A required adapter field is empty.",
            "Nothing was persisted or sent.",
            "Provide the required field and retry.",
            code,
        ));
    }
    Ok(())
}

fn validate_anchor(anchor: &Anchor) -> Result<(), DomainError> {
    if anchor.start_line == 0 || anchor.end_line < anchor.start_line {
        return Err(validation_error(
            "The ask anchor has an invalid line range.",
            "No prompt was sent and the draft is preserved.",
            "Select a valid source range and retry.",
            "invalid_ask_anchor_range",
        ));
    }
    for field in [
        &anchor.repository_id,
        &anchor.workspace_relative_path,
        &anchor.side,
        &anchor.blob_sha,
        &anchor.selected_code,
    ] {
        require_nonempty(field, "ask_anchor_field_required")?;
    }
    validate_token_free_fields(vec![
        anchor.repository_id.as_str(),
        anchor.workspace_relative_path.as_str(),
        anchor.side.as_str(),
        anchor.blob_sha.as_str(),
        anchor.selected_code.as_str(),
    ])
}

fn validation_error(what: &str, safety: &str, next: &str, code: &str) -> DomainError {
    DomainError::actionable(what, safety, next, code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn conversation(state: ConversationSessionState) -> AskConversation {
        AskConversation {
            id: "chat-1".into(),
            round_id: "round-1".into(),
            session_state: state,
            history_only_reason: (state == ConversationSessionState::HistoryOnly)
                .then(|| "provider session ended".into()),
            provider_session_label: Some("existing sign-in".into()),
            options: vec![],
            created_at: Utc::now(),
            archived_at: None,
        }
    }

    #[test]
    fn decision_maps_to_exact_github_event() {
        assert_eq!(
            publish_event_for_decision(Decision::Approve),
            GithubReviewEvent::Approve
        );
        assert_eq!(
            publish_event_for_decision(Decision::RequestChanges),
            GithubReviewEvent::RequestChanges
        );
    }

    #[test]
    fn history_only_conversation_rejects_prompt() {
        let turn = AskTurn {
            id: "turn-1".into(),
            conversation_id: "chat-1".into(),
            idempotency_key: "idem-1".into(),
            prompt: "Is this safe?".into(),
            anchor: None,
            option_values: BTreeMap::new(),
            state: AskTurnState::Queued,
            created_at: Utc::now(),
            completed_at: None,
            failure_reason: None,
            response_text: String::new(),
        };
        assert_eq!(
            turn.validate_for_conversation(&conversation(ConversationSessionState::HistoryOnly))
                .unwrap_err()
                .error
                .code,
            "history_only_conversation"
        );
    }

    #[test]
    fn adapter_metadata_rejects_token_shaped_values() {
        let status = AuthSessionStatus {
            capability: "github_pr_read".into(),
            status: AuthStatus::Connected,
            account_label: Some("ghp_should_not_be_here".into()),
            scopes: vec!["pull_requests:read".into()],
            expires_at: None,
            recovery_action: None,
        };
        assert_eq!(
            status.validate().unwrap_err().error.code,
            "token_shaped_diagnostic"
        );
    }

    #[test]
    fn built_in_source_contracts_are_capability_complete_and_token_free() {
        let local = SourceAdapterContract::legacy_for_collection(Collection::Local);
        assert!(local.supports(SourceCapability::OriginatingAgent));
        assert!(local.supports(SourceCapability::AcpDelivery));
        assert_eq!(local.approval, ApprovalDisposition::PurgeRound);

        let github = SourceAdapterContract::legacy_for_collection(Collection::Github);
        assert!(github.supports(SourceCapability::Publish));
        assert!(github.supports(SourceCapability::UpstreamDiscussion));
        assert!(github.supports(SourceCapability::RemoteRefresh));
        assert_eq!(github.approval, ApprovalDisposition::RecordDecision);

        let machine = SourceAdapterContract::legacy_for_collection(Collection::Machine);
        assert!(machine.supports(SourceCapability::RemoteRefresh));
        assert!(!machine.supports(SourceCapability::Publish));
        machine.validate().unwrap();
    }

    #[test]
    fn legacy_manual_feedback_wire_value_remains_readable_as_acp_delivery() {
        let contract: SourceAdapterContract = serde_json::from_value(serde_json::json!({
            "adapter_id": "local_workspace_snapshot",
            "capabilities": { "capabilities": ["manual_feedback_handoff"] }
        }))
        .unwrap();
        assert!(contract.supports(SourceCapability::AcpDelivery));
    }
}
