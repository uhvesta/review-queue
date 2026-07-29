//! Credential-owning GitHub pull-request integration.
//!
//! Tokens are loaded from capability-scoped Keychain records immediately
//! before an authenticated request. They are never accepted from IPC, written
//! to SQLite, returned in errors, or included in Debug output.

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex},
};

use base64::Engine as _;
use chrono::Utc;
use reqwest::{StatusCode, blocking::Client};
use review_queue_core::socket::SocketResponse;
use review_queue_core::{
    Anchor, Collection, DomainError, RepositorySnapshot, ReviewBrief, Submission,
    WorkspaceManifest,
    adapters::{
        GithubPullRequestMetadata, GithubPullRequestState, ImportedComment, ImportedCommentKind,
        PublishCommentDisposition, StalenessStatus,
    },
    github::{
        GithubAdapter, GithubMaterializedFile, GithubOpenedPullRequest, GithubPublishAttempt,
        GithubPublishReceipt, GithubPublishRequest, GithubPublishStatus, GithubPullRequestLocator,
        GithubQueuePayload, GithubReplyReceipt, GithubReplyRequest, GithubTransport,
    },
    store::{Store, SubmissionResult},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tauri::State;

use crate::commands::{AppState, CommandError, Confirmation, SubmitLocalResult};
use review_queue_desktop::keychain_vault::{
    AppCredentialRecord, Capability, CredentialVault, MacOsKeychainBackend,
};

const API_VERSION: &str = "2022-11-28";

trait CredentialSource {
    fn credential(&self, capability: Capability) -> Result<AppCredentialRecord, CommandError>;
}

struct KeychainCredentialSource {
    vault: CredentialVault<MacOsKeychainBackend>,
}

impl Default for KeychainCredentialSource {
    fn default() -> Self {
        Self {
            vault: CredentialVault::new(MacOsKeychainBackend::new()),
        }
    }
}

impl CredentialSource for KeychainCredentialSource {
    fn credential(&self, capability: Capability) -> Result<AppCredentialRecord, CommandError> {
        let record = self
            .vault
            .get_app_credential(capability)
            .map_err(|_| credential_error(capability))?
            .ok_or_else(|| credential_error(capability))?;
        if record
            .expires_at_unix_seconds
            .is_some_and(|expires| expires <= Utc::now().timestamp())
        {
            return Err(credential_error(capability));
        }
        Ok(record)
    }
}

trait GithubApi {
    fn resolve(
        &mut self,
        token: &str,
        locator: &GithubPullRequestLocator,
    ) -> Result<GithubPullRequestMetadata, DomainError>;
    fn files(
        &mut self,
        token: &str,
        locator: &GithubPullRequestLocator,
    ) -> Result<Vec<GithubMaterializedFile>, DomainError>;
    fn comments(
        &mut self,
        token: &str,
        locator: &GithubPullRequestLocator,
    ) -> Result<Vec<ImportedComment>, DomainError>;
    fn publish(
        &mut self,
        token: &str,
        request: &GithubPublishRequest,
    ) -> Result<GithubPublishReceipt, DomainError>;
    fn reply(
        &mut self,
        token: &str,
        request: &GithubReplyRequest,
    ) -> Result<GithubReplyReceipt, DomainError>;
}

struct CredentialedTransport<'a, A> {
    api: &'a mut A,
    token: &'a str,
}

impl<A: GithubApi> GithubTransport for CredentialedTransport<'_, A> {
    fn resolve_metadata(
        &mut self,
        locator: &GithubPullRequestLocator,
    ) -> Result<GithubPullRequestMetadata, DomainError> {
        self.api.resolve(self.token, locator)
    }
    fn materialize_files(
        &mut self,
        locator: &GithubPullRequestLocator,
    ) -> Result<Vec<GithubMaterializedFile>, DomainError> {
        self.api.files(self.token, locator)
    }
    fn import_comments(
        &mut self,
        locator: &GithubPullRequestLocator,
    ) -> Result<Vec<ImportedComment>, DomainError> {
        self.api.comments(self.token, locator)
    }
    fn publish_review(
        &mut self,
        request: &GithubPublishRequest,
    ) -> Result<GithubPublishReceipt, DomainError> {
        self.api.publish(self.token, request)
    }
    fn publish_reply(
        &mut self,
        request: &GithubReplyRequest,
    ) -> Result<GithubReplyReceipt, DomainError> {
        self.api.reply(self.token, request)
    }
}

struct GithubDesktop<C, A> {
    credentials: C,
    api: A,
}

#[derive(Clone, Debug, Serialize)]
pub struct GithubCommentRefreshResult {
    pub imported: Vec<ImportedComment>,
    pub staleness: StalenessStatus,
}

impl<C: CredentialSource, A: GithubApi> GithubDesktop<C, A> {
    fn queue(&mut self, store: &mut Store, url: &str) -> Result<SubmitLocalResult, CommandError> {
        let credential = self.credentials.credential(Capability::PrRead)?;
        let mut adapter = GithubAdapter::new(CredentialedTransport {
            api: &mut self.api,
            token: &credential.access_token,
        });
        let payload = adapter.queue_from_url(url)?;
        let submission = submission(&payload);
        let result = store.submit(submission)?;
        let (outcome, round, superseded_round_id) = submission_result(result);
        store.save_github_round(&round.id, &payload)?;
        Ok(SubmitLocalResult {
            outcome,
            round,
            superseded_round_id,
        })
    }

    fn open(
        &mut self,
        store: &Store,
        round_id: &str,
    ) -> Result<GithubOpenedPullRequest, CommandError> {
        let credential = self.credentials.credential(Capability::PrRead)?;
        let state = store.github_round(round_id)?;
        if state.payload.source_materialized && !state.files.is_empty() {
            return Ok(GithubOpenedPullRequest {
                payload: state.payload,
                files: state.files,
            });
        }
        let mut adapter = GithubAdapter::new(CredentialedTransport {
            api: &mut self.api,
            token: &credential.access_token,
        });
        let staleness = adapter.check_staleness(&state.payload, Utc::now())?;
        store.save_github_staleness(round_id, &staleness)?;
        if staleness.is_stale() {
            return Err(DomainError::actionable(
                "The pull request head changed before its source was opened.",
                "No source cache was written and all local review state is preserved.",
                "Refresh into a superseding round before opening the source.",
                "github_round_stale",
            )
            .into());
        }
        let opened = adapter.open_files(&state.payload)?;
        store.save_github_files(round_id, &opened.files)?;
        // Persist the materialized marker only after all file reads succeed.
        store.save_github_round(round_id, &opened.payload)?;
        Ok(opened)
    }

    fn refresh_comments(
        &mut self,
        store: &Store,
        round_id: &str,
    ) -> Result<GithubCommentRefreshResult, CommandError> {
        let credential = self.credentials.credential(Capability::PrRead)?;
        let state = store.github_round(round_id)?;
        let mut adapter = GithubAdapter::new(CredentialedTransport {
            api: &mut self.api,
            token: &credential.access_token,
        });
        let staleness = adapter.check_staleness(&state.payload, Utc::now())?;
        store.save_github_staleness(round_id, &staleness)?;
        let refresh = adapter.refresh_comments(&state.payload, Vec::new())?;
        store.save_github_comments(round_id, &refresh.imported)?;
        Ok(GithubCommentRefreshResult {
            imported: refresh.imported,
            staleness,
        })
    }

    fn staleness(
        &mut self,
        store: &Store,
        round_id: &str,
    ) -> Result<StalenessStatus, CommandError> {
        let credential = self.credentials.credential(Capability::PrRead)?;
        let state = store.github_round(round_id)?;
        let mut adapter = GithubAdapter::new(CredentialedTransport {
            api: &mut self.api,
            token: &credential.access_token,
        });
        let status = adapter.check_staleness(&state.payload, Utc::now())?;
        store.save_github_staleness(round_id, &status)?;
        Ok(status)
    }

    fn refresh_round(
        &mut self,
        store: &mut Store,
        round_id: &str,
    ) -> Result<SubmitLocalResult, CommandError> {
        let old_round = store.round(round_id)?;
        let old_state = store.github_round(round_id)?;
        let credential = self.credentials.credential(Capability::PrRead)?;
        let metadata = self
            .api
            .resolve(&credential.access_token, &old_state.payload.locator)?;
        let status = StalenessStatus {
            pinned_head_sha: old_state.payload.metadata.head_sha.clone(),
            observed_head_sha: metadata.head_sha.clone(),
            checked_at: Utc::now(),
        };
        store.save_github_staleness(round_id, &status)?;
        let payload = GithubQueuePayload {
            locator: old_state.payload.locator,
            metadata,
            source_materialized: false,
        };
        let result = store.submit(Submission {
            collection: Collection::Github,
            topic_identity: payload.metadata.topic_identity(),
            brief: old_round.brief,
            manifest: manifest(&payload),
            origin_route: None,
            source_metadata: Some(github_source_metadata(&payload, Some(status.clone()))),
        })?;
        let (outcome, round, superseded_round_id) = submission_result(result);
        store.save_github_round(&round.id, &payload)?;
        Ok(SubmitLocalResult {
            outcome,
            round,
            superseded_round_id,
        })
    }

    fn prepare_publish(
        &mut self,
        store: &Store,
        round_id: &str,
    ) -> Result<GithubPublishAttempt, CommandError> {
        let credential = self.credentials.credential(Capability::PrPublish)?;
        let state = store.github_round(round_id)?;
        ensure_fresh_open(
            self.api
                .resolve(&credential.access_token, &state.payload.locator)?,
            &state.payload,
        )?;
        if let Some(existing) = store.github_publish_attempt(round_id)? {
            return Ok(existing);
        }
        let decision = store.decision(round_id)?;
        let comments = store
            .formal_comments(round_id)?
            .into_iter()
            .filter(|comment| !comment.body.trim_start().starts_with("/ask"))
            .collect::<Vec<_>>();
        let imported_threads = state
            .imported_comments
            .iter()
            // GitHub exposes a reply endpoint only for imported inline
            // review comments. Other imported discussions retain the
            // reference in the review body instead of failing publish.
            .filter(|comment| {
                comment
                    .thread_id
                    .strip_prefix("github-inline-")
                    .and_then(|value| value.parse::<u64>().ok())
                    .is_some_and(|value| value > 0)
            })
            .map(|comment| comment.thread_id.clone())
            .collect::<BTreeSet<_>>();
        let adapter = GithubAdapter::new(CredentialedTransport {
            api: &mut self.api,
            token: &credential.access_token,
        });
        let (preview, mut request) =
            adapter.preflight_publish(&state.payload, decision, &comments, &imported_threads)?;
        let mut replies = Vec::new();
        for publish_comment in request.comments.iter().filter(|comment| {
            comment.disposition == PublishCommentDisposition::ReplyToImportedThread
        }) {
            let upstream_comment_id = publish_comment
                .thread_id
                .strip_prefix("github-inline-")
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| CommandError {
                    code: "github_upstream_reply_unsupported".into(),
                    message: "This imported GitHub discussion does not support a threaded reply."
                        .into(),
                    data_safety:
                        "No GitHub write was made and the formal reply remains saved locally."
                            .into(),
                    next_step:
                        "Reply to an imported inline review thread, or create a PR-level formal comment."
                            .into(),
                })?;
            let formal_revision = comments
                .iter()
                .find(|comment| comment.id == publish_comment.formal_comment_id)
                .map(|comment| comment.revision)
                .ok_or_else(github_publish_state_error)?;
            replies.push(GithubReplyRequest {
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                target: state.payload.metadata.clone(),
                formal_comment_id: publish_comment.formal_comment_id.clone(),
                formal_revision,
                upstream_comment_id,
                body: publish_comment.body.clone(),
            });
        }
        request.comments.retain(|comment| {
            comment.disposition != PublishCommentDisposition::ReplyToImportedThread
        });
        store
            .prepare_github_publish(round_id, &preview, &request, &replies)
            .map_err(Into::into)
    }

    fn publish(
        &mut self,
        store: &Store,
        attempt_id: &str,
        confirmation: &Confirmation,
    ) -> Result<GithubPublishAttempt, CommandError> {
        if !confirmation.confirmed || confirmation.token != format!("publish-github:{attempt_id}") {
            return Err(CommandError {
                code: "confirmation_required".into(),
                message: "Publishing a GitHub review requires explicit confirmation.".into(),
                data_safety: "No GitHub write was made and all drafts remain saved.".into(),
                next_step: format!(
                    "Review the exact preview and confirm with 'publish-github:{attempt_id}'."
                ),
            });
        }
        let attempt = store
            .github_publish_attempt_by_public_id(attempt_id)?
            .ok_or_else(github_publish_state_error)?;
        if !matches!(
            attempt.status,
            GithubPublishStatus::Prepared | GithubPublishStatus::Completed
        ) {
            return Err(github_publish_state_error());
        }
        let credential = self.credentials.credential(Capability::PrPublish)?;
        let state = store.github_round(&attempt.round_id)?;
        ensure_fresh_open(
            self.api
                .resolve(&credential.access_token, &state.payload.locator)?,
            &state.payload,
        )?;
        let mut adapter = GithubAdapter::new(CredentialedTransport {
            api: &mut self.api,
            token: &credential.access_token,
        });
        if attempt.status == GithubPublishStatus::Prepared {
            store.mark_github_publish_posting(attempt_id)?;
            match adapter.publish(&attempt.request) {
                Ok(receipt) => {
                    store.complete_github_publish(attempt_id, &receipt.review_id)?;
                }
                Err(error) => {
                    let _ = store.mark_github_publish_unknown(attempt_id);
                    return Err(error.into());
                }
            }
        }
        let refreshed = store
            .github_publish_attempt_by_public_id(attempt_id)?
            .ok_or_else(github_publish_state_error)?;
        for reply in refreshed.replies {
            match reply.status {
                GithubPublishStatus::Completed => continue,
                GithubPublishStatus::Prepared => {}
                GithubPublishStatus::Posting | GithubPublishStatus::Unknown => {
                    return Err(github_publish_state_error());
                }
            }
            store.mark_github_reply_posting(&reply.id)?;
            match adapter.publish_reply(&reply.request) {
                Ok(receipt) => {
                    store.complete_github_reply(&reply.id, &receipt.comment_id)?;
                }
                Err(error) => {
                    let _ = store.mark_github_reply_unknown(&reply.id);
                    return Err(error.into());
                }
            }
        }
        store
            .github_publish_attempt_by_public_id(attempt_id)?
            .ok_or_else(github_publish_state_error)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueuePullRequestRequest {
    pub url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GithubPublishCommandRequest {
    pub attempt_id: String,
    pub confirmation: Confirmation,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GithubReproductionRequest {
    pub round_id: String,
    pub destination: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmGithubReproductionRequest {
    pub round_id: String,
    pub destination: String,
    pub confirmation: Confirmation,
}

#[tauri::command]
pub fn github_queue_pull_request(
    request: QueuePullRequestRequest,
    state: State<'_, AppState>,
) -> Result<SubmitLocalResult, CommandError> {
    let mut desktop = system_desktop()?;
    let mut store = state.0.lock().map_err(|_| state_unavailable())?;
    desktop.queue(&mut store, &request.url)
}

#[tauri::command]
pub fn github_open_pull_request(
    round_id: String,
    state: State<'_, AppState>,
) -> Result<GithubOpenedPullRequest, CommandError> {
    let mut desktop = system_desktop()?;
    let store = state.0.lock().map_err(|_| state_unavailable())?;
    desktop.open(&store, &round_id)
}

#[tauri::command]
pub fn github_refresh_comments(
    round_id: String,
    state: State<'_, AppState>,
) -> Result<GithubCommentRefreshResult, CommandError> {
    let mut desktop = system_desktop()?;
    let store = state.0.lock().map_err(|_| state_unavailable())?;
    desktop.refresh_comments(&store, &round_id)
}

#[tauri::command]
pub fn github_check_staleness(
    round_id: String,
    state: State<'_, AppState>,
) -> Result<StalenessStatus, CommandError> {
    let mut desktop = system_desktop()?;
    let store = state.0.lock().map_err(|_| state_unavailable())?;
    desktop.staleness(&store, &round_id)
}

#[tauri::command]
pub fn github_refresh_round(
    round_id: String,
    state: State<'_, AppState>,
) -> Result<SubmitLocalResult, CommandError> {
    let mut desktop = system_desktop()?;
    let mut store = state.0.lock().map_err(|_| state_unavailable())?;
    desktop.refresh_round(&mut store, &round_id)
}

#[tauri::command]
pub fn github_prepare_publish(
    round_id: String,
    state: State<'_, AppState>,
) -> Result<GithubPublishAttempt, CommandError> {
    let mut desktop = system_desktop()?;
    let store = state.0.lock().map_err(|_| state_unavailable())?;
    desktop.prepare_publish(&store, &round_id)
}

#[tauri::command]
pub fn github_publish(
    request: GithubPublishCommandRequest,
    state: State<'_, AppState>,
) -> Result<GithubPublishAttempt, CommandError> {
    let mut desktop = system_desktop()?;
    let store = state.0.lock().map_err(|_| state_unavailable())?;
    desktop.publish(&store, &request.attempt_id, &request.confirmation)
}

#[tauri::command]
pub fn github_preview_reproduction(
    request: GithubReproductionRequest,
    state: State<'_, AppState>,
) -> Result<review_queue_core::machine::MachineReproductionPreview, CommandError> {
    let store = state.0.lock().map_err(|_| state_unavailable())?;
    let round = store.round(&request.round_id)?;
    let github = store.github_round(&request.round_id)?;
    review_queue_core::github::preview_reproduction(&round.manifest, &github, request.destination)
        .map_err(Into::into)
}

#[tauri::command]
pub fn github_materialize_reproduction(
    request: ConfirmGithubReproductionRequest,
    state: State<'_, AppState>,
) -> Result<review_queue_core::machine::MachineReproductionResult, CommandError> {
    if !request.confirmation.confirmed
        || request.confirmation.token != format!("reproduce-github:{}", request.round_id)
    {
        return Err(CommandError {
            code: "confirmation_required".into(),
            message: "GitHub reproduction requires explicit confirmation.".into(),
            data_safety: "No directory or source file was created.".into(),
            next_step: format!("Confirm with 'reproduce-github:{}'.", request.round_id),
        });
    }
    let store = state.0.lock().map_err(|_| state_unavailable())?;
    let round = store.round(&request.round_id)?;
    let github = store.github_round(&request.round_id)?;
    review_queue_core::github::reproduce(&round.manifest, &github, request.destination)
        .map_err(Into::into)
}

pub fn queue_pull_request_from_cli(store: &Arc<Mutex<Store>>, url: String) -> SocketResponse {
    let result = system_desktop().and_then(|mut desktop| {
        let mut store = store.lock().map_err(|_| state_unavailable())?;
        desktop.queue(&mut store, &url)
    });
    match result {
        Ok(result) => match serde_json::to_value(result) {
            Ok(data) => SocketResponse::Ok { data },
            Err(_) => SocketResponse::Error {
                error: review_queue_core::ActionableError {
                    code: "socket_encode_error".into(),
                    what_happened: "Review Queue could not encode the PR queue result.".into(),
                    data_safety:
                        "The pull request may already be queued; no GitHub write occurred.".into(),
                    next_step: "Run review-queue ls github to inspect the queue.".into(),
                },
            },
        },
        Err(error) => SocketResponse::Error {
            error: review_queue_core::ActionableError {
                code: error.code,
                what_happened: error.message,
                data_safety: error.data_safety,
                next_step: error.next_step,
            },
        },
    }
}

fn system_desktop() -> Result<GithubDesktop<KeychainCredentialSource, RestGithubApi>, CommandError>
{
    Ok(GithubDesktop {
        credentials: KeychainCredentialSource::default(),
        api: RestGithubApi::github_com()?,
    })
}

fn submission(payload: &GithubQueuePayload) -> Submission {
    Submission {
        collection: Collection::Github,
        topic_identity: payload.metadata.topic_identity(),
        brief: ReviewBrief {
            title: payload.metadata.title.clone(),
            what: payload.metadata.body.clone(),
            why: String::new(),
            approach_alternatives: String::new(),
            testing: String::new(),
        },
        manifest: manifest(payload),
        origin_route: None,
        source_metadata: Some(github_source_metadata(payload, None)),
    }
}

fn github_source_metadata(
    payload: &GithubQueuePayload,
    staleness: Option<StalenessStatus>,
) -> review_queue_core::SourceMetadata {
    review_queue_core::SourceMetadata::Github {
        host: payload.metadata.host.clone(),
        owner: payload.metadata.owner.clone(),
        repository: payload.metadata.repository.clone(),
        pull_number: payload.metadata.pull_number,
        base_sha: payload.metadata.base_sha.clone(),
        head_sha: payload.metadata.head_sha.clone(),
        state: payload.metadata.state,
        is_draft: payload.metadata.is_draft,
        staleness,
    }
}

fn manifest(payload: &GithubQueuePayload) -> WorkspaceManifest {
    let metadata = &payload.metadata;
    WorkspaceManifest {
        workspace_id: metadata.topic_identity(),
        workspace_root: metadata.web_url.clone().unwrap_or_default(),
        topic: format!("PR #{}", metadata.pull_number),
        repositories: vec![RepositorySnapshot {
            repository_id: format!("{}/{}", metadata.owner, metadata.repository),
            root: String::new(),
            branch: format!("pull/{}", metadata.pull_number),
            base_sha: metadata.base_sha.clone(),
            head_sha: metadata.head_sha.clone(),
            remote_fingerprint: metadata.web_url.clone(),
            object_checksum: metadata.head_sha.clone(),
            capture_metadata: None,
        }],
        before_fingerprint: metadata.base_sha.clone(),
        after_fingerprint: metadata.head_sha.clone(),
        created_at: Utc::now(),
    }
}

fn submission_result(
    result: SubmissionResult,
) -> (String, review_queue_core::Round, Option<String>) {
    match result {
        SubmissionResult::Existing(round) => ("existing".into(), round, None),
        SubmissionResult::Created(round) => ("created".into(), round, None),
        SubmissionResult::Superseded { old_id, round } => {
            ("superseded".into(), round, Some(old_id))
        }
    }
}

fn ensure_fresh_open(
    observed: GithubPullRequestMetadata,
    pinned: &GithubQueuePayload,
) -> Result<(), DomainError> {
    if observed.state != GithubPullRequestState::Open {
        return Err(DomainError::actionable(
            "The pull request is no longer open.",
            "No GitHub review was published and local drafts are preserved.",
            "Refresh the round and review the pull request's current state.",
            "github_pull_request_not_open",
        ));
    }
    if observed.head_sha != pinned.metadata.head_sha {
        return Err(DomainError::actionable(
            "The pull request head changed after this review round was captured.",
            "No GitHub review was published and local drafts are preserved.",
            "Refresh to create a superseding round before publishing.",
            "github_round_stale",
        ));
    }
    Ok(())
}

fn credential_error(capability: Capability) -> CommandError {
    CommandError {
        code: format!("{}_capability_required", capability.account()),
        message: format!(
            "The {} capability is not connected or its Keychain record is unavailable.",
            capability.account()
        ),
        data_safety: "No GitHub request was made and no review state changed.".into(),
        next_step: "Open Settings, connect this capability, and retry.".into(),
    }
}

fn state_unavailable() -> CommandError {
    CommandError {
        code: "desktop_state_unavailable".into(),
        message: "Review Queue desktop state is temporarily unavailable.".into(),
        data_safety: "No GitHub request was made and no review state changed.".into(),
        next_step: "Wait briefly and retry.".into(),
    }
}

fn github_publish_state_error() -> CommandError {
    CommandError {
        code: "github_publish_state_conflict".into(),
        message: "This GitHub publish attempt cannot be posted again.".into(),
        data_safety: "No additional GitHub write was made and local drafts are preserved.".into(),
        next_step:
            "If its outcome is unknown, inspect the pull request before preparing a new review."
                .into(),
    }
}

struct RestGithubApi {
    client: Client,
    api_base: reqwest::Url,
}

impl RestGithubApi {
    fn github_com() -> Result<Self, CommandError> {
        Self::new("https://api.github.com/")
    }

    fn new(api_base: &str) -> Result<Self, CommandError> {
        let api_base = reqwest::Url::parse(api_base).map_err(|_| CommandError {
            code: "github_api_configuration_invalid".into(),
            message: "The GitHub API endpoint is invalid.".into(),
            data_safety: "No GitHub request was made.".into(),
            next_step: "Reinstall Review Queue from a verified release.".into(),
        })?;
        let client = Client::builder().build().map_err(|_| CommandError {
            code: "github_client_unavailable".into(),
            message: "Review Queue could not initialize its GitHub client.".into(),
            data_safety: "No GitHub request was made.".into(),
            next_step: "Check system networking and retry.".into(),
        })?;
        Ok(Self { client, api_base })
    }

    fn url(&self, segments: &[&str]) -> Result<reqwest::Url, DomainError> {
        let mut url = self.api_base.clone();
        url.path_segments_mut()
            .map_err(|_| github_read_error("github_api_url_invalid"))?
            .clear()
            .extend(segments);
        Ok(url)
    }

    fn get<T: DeserializeOwned>(&self, token: &str, url: reqwest::Url) -> Result<T, DomainError> {
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("User-Agent", "Review-Queue")
            .send()
            .map_err(|_| github_read_error("github_request_failed"))?;
        if !response.status().is_success() {
            return Err(github_status_error(response.status()));
        }
        response
            .json()
            .map_err(|_| github_read_error("github_response_invalid"))
    }

    fn graphql<T: DeserializeOwned>(
        &self,
        token: &str,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<T, DomainError> {
        let response = self
            .client
            .post(self.url(&["graphql"])?)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("User-Agent", "Review-Queue")
            .json(&serde_json::json!({"query": query, "variables": variables}))
            .send()
            .map_err(|_| github_read_error("github_graphql_request_failed"))?;
        if !response.status().is_success() {
            return Err(github_status_error(response.status()));
        }
        let envelope: GraphqlEnvelope<T> = response
            .json()
            .map_err(|_| github_read_error("github_graphql_response_invalid"))?;
        if !envelope.errors.is_empty() {
            return Err(github_read_error("github_graphql_response_error"));
        }
        envelope
            .data
            .ok_or_else(|| github_read_error("github_graphql_response_missing_data"))
    }

    fn review_thread_resolution(
        &self,
        token: &str,
        locator: &GithubPullRequestLocator,
    ) -> Result<HashMap<u64, bool>, DomainError> {
        const QUERY: &str = r#"
          query ReviewQueueThreads($owner: String!, $name: String!, $number: Int!, $cursor: String) {
            repository(owner: $owner, name: $name) {
              pullRequest(number: $number) {
                reviewThreads(first: 100, after: $cursor) {
                  nodes {
                    isResolved
                    comments(first: 1) { nodes { databaseId } }
                  }
                  pageInfo { hasNextPage endCursor }
                }
              }
            }
          }
        "#;
        let pull_number = i64::try_from(locator.pull_number)
            .map_err(|_| github_read_error("github_pull_number_out_of_range"))?;
        let mut cursor: Option<String> = None;
        let mut result = HashMap::new();
        loop {
            let data: ReviewThreadsData = self.graphql(
                token,
                QUERY,
                serde_json::json!({
                    "owner": locator.owner,
                    "name": locator.repository,
                    "number": pull_number,
                    "cursor": cursor,
                }),
            )?;
            let connection = data
                .repository
                .and_then(|repository| repository.pull_request)
                .map(|pull| pull.review_threads)
                .ok_or_else(|| github_read_error("github_review_threads_missing"))?;
            for thread in connection.nodes {
                for comment in thread.comments.nodes {
                    if let Some(id) = comment.database_id {
                        result.insert(id, thread.is_resolved);
                    }
                }
            }
            if !connection.page_info.has_next_page {
                break;
            }
            cursor = connection.page_info.end_cursor;
            if cursor.is_none() {
                return Err(github_read_error("github_review_threads_cursor_missing"));
            }
        }
        Ok(result)
    }

    fn content(
        &self,
        token: &str,
        locator: &GithubPullRequestLocator,
        path: &str,
        revision: &str,
    ) -> Result<MaterializedBlob, DomainError> {
        let mut segments = vec![
            "repos",
            locator.owner.as_str(),
            locator.repository.as_str(),
            "contents",
        ];
        segments.extend(path.split('/'));
        let mut url = self.url(&segments)?;
        url.query_pairs_mut().append_pair("ref", revision);
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("User-Agent", "Review-Queue")
            .send()
            .map_err(|_| github_read_error("github_blob_request_failed"))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(MaterializedBlob {
                sha: "0".repeat(40),
                text: Some(String::new()),
                content_base64: None,
                is_binary: false,
            });
        }
        if !response.status().is_success() {
            return Err(github_status_error(response.status()));
        }
        let blob: ContentResponse = response
            .json()
            .map_err(|_| github_read_error("github_blob_response_invalid"))?;
        let content_base64 = blob.content.replace(['\n', '\r'], "");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&content_base64)
            .map_err(|_| github_read_error("github_blob_base64_invalid"))?;
        let text = String::from_utf8(bytes).ok();
        Ok(MaterializedBlob {
            sha: blob.sha,
            is_binary: text.is_none(),
            text,
            content_base64: Some(content_base64),
        })
    }
}

#[derive(Deserialize)]
struct PullResponse {
    number: u64,
    title: String,
    body: Option<String>,
    state: String,
    merged: Option<bool>,
    draft: Option<bool>,
    html_url: String,
    base: RefResponse,
    head: RefResponse,
}
#[derive(Deserialize)]
struct RefResponse {
    sha: String,
}
#[derive(Deserialize)]
struct PullFileResponse {
    filename: String,
    previous_filename: Option<String>,
    status: String,
    #[serde(rename = "patch")]
    _patch: Option<String>,
}
#[derive(Deserialize)]
struct ContentResponse {
    sha: String,
    content: String,
}
struct MaterializedBlob {
    sha: String,
    text: Option<String>,
    content_base64: Option<String>,
    is_binary: bool,
}
#[derive(Deserialize)]
struct InlineCommentResponse {
    id: u64,
    body: String,
    user: UserResponse,
    created_at: String,
    html_url: String,
    path: String,
    line: Option<u32>,
    original_line: Option<u32>,
    side: Option<String>,
    commit_id: String,
    in_reply_to_id: Option<u64>,
}
#[derive(Deserialize)]
struct IssueCommentResponse {
    id: u64,
    body: String,
    user: UserResponse,
    created_at: String,
    html_url: String,
}
#[derive(Deserialize)]
struct PullReviewResponse {
    id: u64,
    body: Option<String>,
    user: UserResponse,
    submitted_at: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    html_url: String,
    state: String,
}
#[derive(Deserialize)]
struct UserResponse {
    login: String,
}
#[derive(Deserialize)]
struct GraphqlEnvelope<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
struct ReviewThreadsData {
    repository: Option<ReviewThreadsRepository>,
}
#[derive(Deserialize)]
struct ReviewThreadsRepository {
    #[serde(rename = "pullRequest")]
    pull_request: Option<ReviewThreadsPullRequest>,
}
#[derive(Deserialize)]
struct ReviewThreadsPullRequest {
    #[serde(rename = "reviewThreads")]
    review_threads: ReviewThreadConnection,
}
#[derive(Deserialize)]
struct ReviewThreadConnection {
    nodes: Vec<ReviewThreadNode>,
    #[serde(rename = "pageInfo")]
    page_info: GraphqlPageInfo,
}
#[derive(Deserialize)]
struct ReviewThreadNode {
    #[serde(rename = "isResolved")]
    is_resolved: bool,
    comments: ReviewThreadCommentConnection,
}
#[derive(Deserialize)]
struct ReviewThreadCommentConnection {
    nodes: Vec<ReviewThreadCommentNode>,
}
#[derive(Deserialize)]
struct ReviewThreadCommentNode {
    #[serde(rename = "databaseId")]
    database_id: Option<u64>,
}
#[derive(Deserialize)]
struct GraphqlPageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}
#[derive(Deserialize)]
struct ReviewResponse {
    id: u64,
}

impl GithubApi for RestGithubApi {
    fn resolve(
        &mut self,
        token: &str,
        locator: &GithubPullRequestLocator,
    ) -> Result<GithubPullRequestMetadata, DomainError> {
        if locator.host != "github.com" {
            return Err(DomainError::actionable(
                "This build connects only to github.com pull requests.",
                "No remote request was made.",
                "Use a github.com pull request URL.",
                "github_host_unsupported",
            ));
        }
        let response: PullResponse = self.get(
            token,
            self.url(&[
                "repos",
                &locator.owner,
                &locator.repository,
                "pulls",
                &locator.pull_number.to_string(),
            ])?,
        )?;
        let state = if response.merged.unwrap_or(false) {
            GithubPullRequestState::Merged
        } else if response.state == "open" {
            GithubPullRequestState::Open
        } else {
            GithubPullRequestState::Closed
        };
        Ok(GithubPullRequestMetadata {
            host: locator.host.clone(),
            owner: locator.owner.clone(),
            repository: locator.repository.clone(),
            pull_number: response.number,
            title: response.title,
            body: response.body.unwrap_or_default(),
            base_sha: response.base.sha,
            head_sha: response.head.sha,
            state,
            is_draft: response.draft.unwrap_or(false),
            web_url: Some(response.html_url),
        })
    }

    fn files(
        &mut self,
        token: &str,
        locator: &GithubPullRequestLocator,
    ) -> Result<Vec<GithubMaterializedFile>, DomainError> {
        let metadata = self.resolve(token, locator)?;
        let mut files = Vec::new();
        for page in 1.. {
            let mut url = self.url(&[
                "repos",
                &locator.owner,
                &locator.repository,
                "pulls",
                &locator.pull_number.to_string(),
                "files",
            ])?;
            url.query_pairs_mut()
                .append_pair("per_page", "100")
                .append_pair("page", &page.to_string());
            let batch: Vec<PullFileResponse> = self.get(token, url)?;
            let done = batch.len() < 100;
            files.extend(batch);
            if done {
                break;
            }
        }
        files
            .into_iter()
            .map(|file| {
                let base_path = file.previous_filename.as_deref().unwrap_or(&file.filename);
                let base = self.content(token, locator, base_path, &metadata.base_sha)?;
                let head = self.content(token, locator, &file.filename, &metadata.head_sha)?;
                let unified_diff =
                    full_unified_diff(&file.filename, base.text.as_deref(), head.text.as_deref());
                Ok(GithubMaterializedFile {
                    path: file.filename,
                    status: file.status,
                    base_blob_sha: base.sha,
                    head_blob_sha: head.sha,
                    is_binary: base.is_binary || head.is_binary,
                    base_content: base.text,
                    head_content: head.text,
                    base_content_base64: base.content_base64,
                    head_content_base64: head.content_base64,
                    unified_diff,
                })
            })
            .collect()
    }

    fn comments(
        &mut self,
        token: &str,
        locator: &GithubPullRequestLocator,
    ) -> Result<Vec<ImportedComment>, DomainError> {
        let mut inline = Vec::new();
        let mut issue = Vec::new();
        let mut reviews = Vec::new();
        for page in 1.. {
            let mut url = self.url(&[
                "repos",
                &locator.owner,
                &locator.repository,
                "pulls",
                &locator.pull_number.to_string(),
                "comments",
            ])?;
            url.query_pairs_mut()
                .append_pair("per_page", "100")
                .append_pair("page", &page.to_string());
            let batch: Vec<InlineCommentResponse> = self.get(token, url)?;
            let done = batch.len() < 100;
            inline.extend(batch);
            if done {
                break;
            }
        }
        for page in 1.. {
            let mut url = self.url(&[
                "repos",
                &locator.owner,
                &locator.repository,
                "issues",
                &locator.pull_number.to_string(),
                "comments",
            ])?;
            url.query_pairs_mut()
                .append_pair("per_page", "100")
                .append_pair("page", &page.to_string());
            let batch: Vec<IssueCommentResponse> = self.get(token, url)?;
            let done = batch.len() < 100;
            issue.extend(batch);
            if done {
                break;
            }
        }
        for page in 1.. {
            let mut url = self.url(&[
                "repos",
                &locator.owner,
                &locator.repository,
                "pulls",
                &locator.pull_number.to_string(),
                "reviews",
            ])?;
            url.query_pairs_mut()
                .append_pair("per_page", "100")
                .append_pair("page", &page.to_string());
            let batch: Vec<PullReviewResponse> = self.get(token, url)?;
            let done = batch.len() < 100;
            reviews.extend(batch);
            if done {
                break;
            }
        }
        let resolution = self.review_thread_resolution(token, locator)?;
        let mut imported = inline
            .into_iter()
            .map(|comment| {
                let line = comment.line.or(comment.original_line).unwrap_or(1);
                let root_comment_id = comment.in_reply_to_id.unwrap_or(comment.id);
                let upstream_resolved = resolution
                    .get(&root_comment_id)
                    .or_else(|| resolution.get(&comment.id))
                    .copied();
                Ok(ImportedComment {
                    id: format!("github-inline-{}", comment.id),
                    thread_id: format!("github-inline-{root_comment_id}"),
                    body: comment.body,
                    upstream_author: comment.user.login,
                    upstream_created_at: parse_github_time(&comment.created_at)?,
                    source_url: comment.html_url,
                    kind: ImportedCommentKind::ReviewThreadComment,
                    upstream_resolved,
                    upstream_review_state: None,
                    anchor: Some(Anchor {
                        repository_id: format!("{}/{}", locator.owner, locator.repository),
                        workspace_relative_path: comment.path,
                        side: comment
                            .side
                            .unwrap_or_else(|| "RIGHT".into())
                            .to_lowercase(),
                        start_line: line,
                        end_line: line,
                        blob_sha: comment.commit_id,
                        selected_code: String::new(),
                    }),
                })
            })
            .collect::<Result<Vec<_>, DomainError>>()?;
        imported.extend(
            issue
                .into_iter()
                .map(|comment| {
                    Ok(ImportedComment {
                        id: format!("github-pr-{}", comment.id),
                        thread_id: format!("github-pr-{}", comment.id),
                        body: comment.body,
                        upstream_author: comment.user.login,
                        upstream_created_at: parse_github_time(&comment.created_at)?,
                        source_url: comment.html_url,
                        kind: ImportedCommentKind::PullRequestComment,
                        upstream_resolved: None,
                        upstream_review_state: None,
                        anchor: None,
                    })
                })
                .collect::<Result<Vec<_>, DomainError>>()?,
        );
        for review in reviews {
            let Some(created_at) = review
                .submitted_at
                .as_deref()
                .or(review.created_at.as_deref())
            else {
                // Pending reviews are not yet durable upstream summaries.
                continue;
            };
            let upstream_created_at = parse_github_time(created_at)?;
            imported.push(ImportedComment {
                id: format!("github-review-{}", review.id),
                thread_id: format!("github-review-{}", review.id),
                body: review.body.unwrap_or_default(),
                upstream_author: review.user.login,
                upstream_created_at,
                source_url: review.html_url,
                kind: ImportedCommentKind::ReviewSummary,
                upstream_resolved: None,
                upstream_review_state: Some(review.state.to_ascii_lowercase()),
                anchor: None,
            });
        }
        Ok(imported)
    }

    fn publish(
        &mut self,
        token: &str,
        request: &GithubPublishRequest,
    ) -> Result<GithubPublishReceipt, DomainError> {
        let mut body_parts = Vec::new();
        let mut inline: Vec<serde_json::Value> = Vec::new();
        for comment in &request.comments {
            match comment.disposition {
                PublishCommentDisposition::Inline => {
                    let anchor = comment
                        .anchor
                        .as_ref()
                        .ok_or_else(|| github_write_error("github_inline_anchor_missing"))?;
                    let mut inline_comment = serde_json::json!({
                        "path": anchor.workspace_relative_path,
                        "line": anchor.end_line,
                        "side": anchor.side.to_ascii_uppercase(),
                        "body": comment.body,
                    });
                    if anchor.start_line != anchor.end_line
                        && let Some(fields) = inline_comment.as_object_mut()
                    {
                        fields.insert("start_line".into(), anchor.start_line.into());
                        fields.insert("start_side".into(), anchor.side.to_ascii_uppercase().into());
                    }
                    inline.push(inline_comment);
                }
                PublishCommentDisposition::BodyFallback => body_parts.push(format!(
                    "{}\n\nLocation: {}",
                    comment.body,
                    comment.fallback_reference.as_deref().unwrap_or("unknown")
                )),
                PublishCommentDisposition::ReplyToImportedThread => {
                    return Err(github_write_error("github_reply_requires_reply_endpoint"));
                }
                PublishCommentDisposition::ReviewBody => body_parts.push(comment.body.clone()),
            }
        }
        let event = match request.event {
            review_queue_core::adapters::GithubReviewEvent::Approve => "APPROVE",
            review_queue_core::adapters::GithubReviewEvent::RequestChanges => "REQUEST_CHANGES",
        };
        let url = self.url(&[
            "repos",
            &request.target.owner,
            &request.target.repository,
            "pulls",
            &request.target.pull_number.to_string(),
            "reviews",
        ])?;
        let payload = serde_json::json!({
            "commit_id": request.target.head_sha,
            "event": event,
            "body": body_parts.join("\n\n---\n\n"),
            "comments": inline,
        });
        let response = self
            .client
            .post(url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("User-Agent", "Review-Queue")
            .json(&payload)
            .send()
            .map_err(|_| github_write_error("github_publish_request_failed"))?;
        if !response.status().is_success() {
            return Err(github_status_write_error(response.status()));
        }
        let review: ReviewResponse = response
            .json()
            .map_err(|_| github_write_error("github_publish_response_invalid"))?;
        Ok(GithubPublishReceipt {
            review_id: review.id.to_string(),
            idempotency_key: request.idempotency_key.clone(),
        })
    }

    fn reply(
        &mut self,
        token: &str,
        request: &GithubReplyRequest,
    ) -> Result<GithubReplyReceipt, DomainError> {
        let url = self.url(&[
            "repos",
            &request.target.owner,
            &request.target.repository,
            "pulls",
            &request.target.pull_number.to_string(),
            "comments",
            &request.upstream_comment_id.to_string(),
            "replies",
        ])?;
        let response = self
            .client
            .post(url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("User-Agent", "Review-Queue")
            .json(&serde_json::json!({"body": request.body}))
            .send()
            .map_err(|_| github_write_error("github_reply_request_failed"))?;
        if !response.status().is_success() {
            return Err(github_status_write_error(response.status()));
        }
        let comment: ReviewResponse = response
            .json()
            .map_err(|_| github_write_error("github_reply_response_invalid"))?;
        Ok(GithubReplyReceipt {
            comment_id: comment.id.to_string(),
            idempotency_key: request.idempotency_key.clone(),
        })
    }
}

fn parse_github_time(value: &str) -> Result<chrono::DateTime<Utc>, DomainError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|_| github_read_error("github_comment_time_invalid"))
}

fn full_unified_diff(path: &str, base: Option<&str>, head: Option<&str>) -> String {
    let (Some(base), Some(head)) = (base, head) else {
        return "Binary file changed".into();
    };
    similar::TextDiff::from_lines(base, head)
        .unified_diff()
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

fn github_read_error(code: &str) -> DomainError {
    DomainError::actionable(
        "Review Queue could not read the requested GitHub data.",
        "No GitHub write occurred and local review data is unchanged.",
        "Check the PR read connection and retry.",
        code,
    )
}
fn github_write_error(code: &str) -> DomainError {
    DomainError::actionable(
        "Review Queue could not confirm the GitHub review result.",
        "The attempt is marked unknown and will not be posted again automatically.",
        "Inspect the pull request before deciding whether another review is needed.",
        code,
    )
}
fn github_status_error(status: StatusCode) -> DomainError {
    DomainError::actionable(
        format!("GitHub rejected a read request with HTTP status {status}."),
        "No GitHub write occurred and local review data is unchanged.",
        "Check PR visibility and reconnect PR read if needed.",
        "github_read_rejected",
    )
}
fn github_status_write_error(status: StatusCode) -> DomainError {
    DomainError::actionable(
        format!("GitHub rejected the review request with HTTP status {status}."),
        "No automatic retry will occur; local drafts remain saved.",
        "Inspect the pull request and capability scopes before preparing another review.",
        "github_publish_rejected",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        io::{Read, Write},
        net::TcpListener,
    };

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        let mut expected_len = None;
        loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "connection closed before request completed");
            request.extend_from_slice(&buffer[..read]);
            if expected_len.is_none()
                && let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
            {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_len = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(str::trim)
                            .and_then(|value| value.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                expected_len = Some(header_end + 4 + content_len);
            }
            if expected_len.is_some_and(|length| request.len() >= length) {
                break;
            }
        }
        String::from_utf8(request).unwrap()
    }

    fn respond_json(stream: &mut std::net::TcpStream, body: serde_json::Value) {
        let body = body.to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    }

    #[derive(Default)]
    struct FakeCredentials {
        records: HashMap<Capability, AppCredentialRecord>,
        reads: RefCell<Vec<Capability>>,
    }
    impl CredentialSource for FakeCredentials {
        fn credential(&self, capability: Capability) -> Result<AppCredentialRecord, CommandError> {
            self.reads.borrow_mut().push(capability);
            self.records
                .get(&capability)
                .cloned()
                .ok_or_else(|| credential_error(capability))
        }
    }

    #[derive(Default)]
    struct FakeApi {
        metadata_reads: usize,
        file_reads: usize,
        comment_reads: usize,
        publish_writes: usize,
        reply_writes: usize,
        tokens: Vec<String>,
        head: String,
        imported_comments: Vec<ImportedComment>,
    }
    impl FakeApi {
        fn metadata(&self, locator: &GithubPullRequestLocator) -> GithubPullRequestMetadata {
            GithubPullRequestMetadata {
                host: locator.host.clone(),
                owner: locator.owner.clone(),
                repository: locator.repository.clone(),
                pull_number: locator.pull_number,
                title: "PR".into(),
                body: "Body".into(),
                base_sha: "base".into(),
                head_sha: self.head.clone(),
                state: GithubPullRequestState::Open,
                is_draft: false,
                web_url: Some("https://github.com/o/r/pull/1".into()),
            }
        }
    }
    impl GithubApi for FakeApi {
        fn resolve(
            &mut self,
            token: &str,
            locator: &GithubPullRequestLocator,
        ) -> Result<GithubPullRequestMetadata, DomainError> {
            self.metadata_reads += 1;
            self.tokens.push(token.into());
            Ok(self.metadata(locator))
        }
        fn files(
            &mut self,
            token: &str,
            _: &GithubPullRequestLocator,
        ) -> Result<Vec<GithubMaterializedFile>, DomainError> {
            self.file_reads += 1;
            self.tokens.push(token.into());
            Ok(vec![GithubMaterializedFile {
                path: "a.rs".into(),
                status: "modified".into(),
                base_blob_sha: "base-blob".into(),
                head_blob_sha: "head-blob".into(),
                is_binary: false,
                base_content: Some("old".into()),
                head_content: Some("new".into()),
                base_content_base64: Some("b2xk".into()),
                head_content_base64: Some("bmV3".into()),
                unified_diff: "-old\n+new".into(),
            }])
        }
        fn comments(
            &mut self,
            token: &str,
            _: &GithubPullRequestLocator,
        ) -> Result<Vec<ImportedComment>, DomainError> {
            self.comment_reads += 1;
            self.tokens.push(token.into());
            Ok(self.imported_comments.clone())
        }
        fn publish(
            &mut self,
            token: &str,
            request: &GithubPublishRequest,
        ) -> Result<GithubPublishReceipt, DomainError> {
            self.publish_writes += 1;
            self.tokens.push(token.into());
            Ok(GithubPublishReceipt {
                review_id: "7".into(),
                idempotency_key: request.idempotency_key.clone(),
            })
        }
        fn reply(
            &mut self,
            token: &str,
            request: &GithubReplyRequest,
        ) -> Result<GithubReplyReceipt, DomainError> {
            self.reply_writes += 1;
            self.tokens.push(token.into());
            Ok(GithubReplyReceipt {
                comment_id: format!("reply-{}", self.reply_writes),
                idempotency_key: request.idempotency_key.clone(),
            })
        }
    }

    fn desktop() -> GithubDesktop<FakeCredentials, FakeApi> {
        let record = |token: &str| AppCredentialRecord {
            access_token: token.into(),
            account_label: None,
            scopes: Vec::new(),
            expires_at_unix_seconds: None,
        };
        GithubDesktop {
            credentials: FakeCredentials {
                records: HashMap::from([
                    (Capability::PrRead, record("read-secret")),
                    (Capability::PrPublish, record("publish-secret")),
                ]),
                reads: RefCell::new(Vec::new()),
            },
            api: FakeApi {
                head: "head-a".into(),
                ..FakeApi::default()
            },
        }
    }

    #[test]
    fn intake_is_metadata_only_and_open_is_lazy_and_cached() {
        let mut store = Store::in_memory().unwrap();
        let mut desktop = desktop();
        let queued = desktop
            .queue(&mut store, "https://github.com/o/r/pull/1")
            .unwrap();
        assert_eq!(desktop.api.metadata_reads, 1);
        assert_eq!(desktop.api.file_reads, 0);
        assert!(matches!(
            store.round(&queued.round.id).unwrap().source_metadata,
            Some(review_queue_core::SourceMetadata::Github {
                ref host,
                ref owner,
                ref repository,
                pull_number: 1,
                ref base_sha,
                ref head_sha,
                staleness: None,
                ..
            }) if host == "github.com"
                && owner == "o"
                && repository == "r"
                && base_sha == "base"
                && head_sha == "head-a"
        ));
        desktop.open(&store, &queued.round.id).unwrap();
        desktop.open(&store, &queued.round.id).unwrap();
        assert_eq!(desktop.api.file_reads, 1);
        assert_eq!(desktop.api.metadata_reads, 2);
        assert_eq!(
            desktop.api.tokens,
            vec!["read-secret", "read-secret", "read-secret"]
        );
        assert!(matches!(
            store.github_round(&queued.round.id).unwrap().last_staleness,
            Some(ref status) if !status.is_stale() && status.observed_head_sha == "head-a"
        ));
        assert!(
            !serde_json::to_string(&store.github_round(&queued.round.id).unwrap())
                .unwrap()
                .contains("secret")
        );
    }

    #[test]
    fn refresh_is_pull_only_persists_staleness_and_preserves_local_drafts() {
        let mut store = Store::in_memory().unwrap();
        let mut desktop = desktop();
        let queued = desktop
            .queue(&mut store, "https://github.com/o/r/pull/1")
            .unwrap();
        let draft = store
            .create_formal_comment(&queued.round.id, "round", "Keep this local draft.", None)
            .unwrap();
        desktop.api.imported_comments = vec![ImportedComment {
            id: "resolved-comment".into(),
            thread_id: "github-inline-17".into(),
            body: "Already handled.".into(),
            upstream_author: "octo".into(),
            upstream_created_at: Utc::now(),
            source_url: "https://github.com/o/r/pull/1#discussion_r17".into(),
            kind: ImportedCommentKind::ReviewThreadComment,
            upstream_resolved: Some(true),
            upstream_review_state: None,
            anchor: None,
        }];
        let first = desktop.refresh_comments(&store, &queued.round.id).unwrap();
        assert!(!first.staleness.is_stale());
        assert_eq!(first.imported, desktop.api.imported_comments);
        assert_eq!(
            store.formal_comments(&queued.round.id).unwrap(),
            vec![draft.clone()]
        );
        assert_eq!(desktop.api.publish_writes, 0);
        assert_eq!(desktop.api.reply_writes, 0);

        desktop.api.head = "head-b".into();
        let stale = desktop.refresh_comments(&store, &queued.round.id).unwrap();
        assert!(stale.staleness.is_stale());
        assert_eq!(
            store.formal_comments(&queued.round.id).unwrap(),
            vec![draft]
        );
        assert!(matches!(
            store.github_round(&queued.round.id).unwrap().last_staleness,
            Some(ref status) if status.observed_head_sha == "head-b"
        ));
        assert_eq!(desktop.api.publish_writes, 0);
        assert_eq!(desktop.api.reply_writes, 0);

        let refreshed = desktop.refresh_round(&mut store, &queued.round.id).unwrap();
        assert_eq!(refreshed.outcome, "superseded");
        assert_eq!(
            refreshed.superseded_round_id.as_deref(),
            Some(queued.round.id.as_str())
        );
        assert!(matches!(
            store.round(&queued.round.id).unwrap().source_metadata,
            Some(review_queue_core::SourceMetadata::Github {
                staleness: Some(ref status),
                ..
            }) if status.observed_head_sha == "head-b"
        ));
    }

    #[test]
    fn first_open_refuses_stale_head_before_source_cache_and_never_writes() {
        let mut store = Store::in_memory().unwrap();
        let mut desktop = desktop();
        let queued = desktop
            .queue(&mut store, "https://github.com/o/r/pull/1")
            .unwrap();
        desktop.api.head = "head-b".into();
        let error = desktop.open(&store, &queued.round.id).unwrap_err();
        assert_eq!(error.code, "github_round_stale");
        assert_eq!(desktop.api.file_reads, 0);
        assert_eq!(desktop.api.publish_writes, 0);
        assert_eq!(desktop.api.reply_writes, 0);
        let state = store.github_round(&queued.round.id).unwrap();
        assert!(!state.payload.source_materialized);
        assert!(state.files.is_empty());
        assert!(matches!(
            state.last_staleness,
            Some(ref status) if status.pinned_head_sha == "head-a"
                && status.observed_head_sha == "head-b"
        ));
    }

    #[test]
    fn publish_requires_separate_capability_confirmation_and_posts_once() {
        let mut store = Store::in_memory().unwrap();
        let mut desktop = desktop();
        let queued = desktop
            .queue(&mut store, "https://github.com/o/r/pull/1")
            .unwrap();
        store
            .create_formal_comment(&queued.round.id, "ask", "/ask explain this", None)
            .unwrap();
        store
            .save_github_comments(
                &queued.round.id,
                &[ImportedComment {
                    id: "upstream-1".into(),
                    thread_id: "github-inline-101".into(),
                    body: "Existing upstream comment".into(),
                    upstream_author: "octo".into(),
                    upstream_created_at: Utc::now(),
                    source_url: "https://github.com/o/r/pull/1#discussion".into(),
                    kind: ImportedCommentKind::ReviewThreadComment,
                    upstream_resolved: Some(false),
                    upstream_review_state: None,
                    anchor: None,
                }],
            )
            .unwrap();
        store
            .create_formal_comment(
                &queued.round.id,
                "github-inline-101",
                "A formal reply",
                None,
            )
            .unwrap();
        store.approve_remote(&queued.round.id).unwrap();
        let attempt = desktop.prepare_publish(&store, &queued.round.id).unwrap();
        assert!(attempt.request.comments.is_empty());
        assert_eq!(attempt.replies.len(), 1);
        assert_eq!(attempt.replies[0].request.upstream_comment_id, 101);
        assert_eq!(
            attempt.preview.comments[0].disposition,
            PublishCommentDisposition::ReplyToImportedThread
        );
        assert!(attempt.preview.comments[0].fallback_reference.is_none());
        assert!(!serde_json::to_string(&attempt).unwrap().contains("/ask"));
        assert_eq!(
            desktop
                .publish(
                    &store,
                    &attempt.id,
                    &Confirmation {
                        confirmed: false,
                        token: String::new()
                    }
                )
                .unwrap_err()
                .code,
            "confirmation_required"
        );
        let confirmation = Confirmation {
            confirmed: true,
            token: format!("publish-github:{}", attempt.id),
        };
        let completed = desktop.publish(&store, &attempt.id, &confirmation).unwrap();
        assert_eq!(completed.status, GithubPublishStatus::Completed);
        desktop.publish(&store, &attempt.id, &confirmation).unwrap();
        assert_eq!(desktop.api.publish_writes, 1);
        assert_eq!(desktop.api.reply_writes, 1);
        assert!(desktop.api.tokens.contains(&"publish-secret".into()));
    }

    #[test]
    fn rest_transport_sends_bearer_only_as_header_and_normalizes_metadata() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET /repos/o/r/pulls/1 HTTP/1.1\r\n"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer wire-secret\r\n")
            );
            assert!(!request.starts_with("GET /wire-secret"));
            let body = serde_json::json!({
                "number": 1,
                "title": "Wire PR",
                "body": "Body",
                "state": "open",
                "merged": false,
                "draft": false,
                "html_url": "https://github.com/o/r/pull/1",
                "base": {"sha": "base"},
                "head": {"sha": "head"}
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let mut api = RestGithubApi::new(&format!("http://{address}/")).unwrap();
        let metadata = api
            .resolve(
                "wire-secret",
                &GithubPullRequestLocator {
                    host: "github.com".into(),
                    owner: "o".into(),
                    repository: "r".into(),
                    pull_number: 1,
                },
            )
            .unwrap();
        assert_eq!(metadata.title, "Wire PR");
        assert_eq!(metadata.state, GithubPullRequestState::Open);
        server.join().unwrap();
    }

    #[test]
    fn rest_comment_import_reads_summaries_and_graphql_thread_resolution_without_writes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for step in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("authorization: bearer wire-secret\r\n")
                );
                match step {
                    0 => {
                        assert!(request.starts_with(
                            "GET /repos/o/r/pulls/1/comments?per_page=100&page=1 HTTP/1.1\r\n"
                        ));
                        respond_json(
                            &mut stream,
                            serde_json::json!([{
                                "id": 11,
                                "body": "Resolve this.",
                                "user": {"login": "reviewer"},
                                "created_at": "2026-07-29T00:00:00Z",
                                "html_url": "https://github.com/o/r/pull/1#discussion_r11",
                                "path": "src/a.rs",
                                "line": 7,
                                "original_line": null,
                                "side": "RIGHT",
                                "commit_id": "head-a",
                                "in_reply_to_id": null
                            }]),
                        );
                    }
                    1 => {
                        assert!(request.starts_with(
                            "GET /repos/o/r/issues/1/comments?per_page=100&page=1 HTTP/1.1\r\n"
                        ));
                        respond_json(
                            &mut stream,
                            serde_json::json!([{
                                "id": 21,
                                "body": "PR-level note.",
                                "user": {"login": "maintainer"},
                                "created_at": "2026-07-29T00:01:00Z",
                                "html_url": "https://github.com/o/r/pull/1#issuecomment-21"
                            }]),
                        );
                    }
                    2 => {
                        assert!(request.starts_with(
                            "GET /repos/o/r/pulls/1/reviews?per_page=100&page=1 HTTP/1.1\r\n"
                        ));
                        respond_json(
                            &mut stream,
                            serde_json::json!([{
                                "id": 31,
                                "body": "",
                                "user": {"login": "approver"},
                                "submitted_at": "2026-07-29T00:02:00Z",
                                "created_at": "2026-07-29T00:02:00Z",
                                "html_url": "https://github.com/o/r/pull/1#pullrequestreview-31",
                                "state": "APPROVED"
                            }]),
                        );
                    }
                    3 => {
                        assert!(request.starts_with("POST /graphql HTTP/1.1\r\n"));
                        let body = request.split_once("\r\n\r\n").unwrap().1;
                        assert!(body.contains("query ReviewQueueThreads"));
                        assert!(body.contains("reviewThreads"));
                        assert!(!body.contains("mutation"));
                        assert!(body.contains("\"owner\":\"o\""));
                        assert!(body.contains("\"name\":\"r\""));
                        assert!(body.contains("\"number\":1"));
                        respond_json(
                            &mut stream,
                            serde_json::json!({
                                "data": {
                                    "repository": {
                                        "pullRequest": {
                                            "reviewThreads": {
                                                "nodes": [{
                                                    "isResolved": true,
                                                    "comments": {
                                                        "nodes": [{"databaseId": 11}]
                                                    }
                                                }],
                                                "pageInfo": {
                                                    "hasNextPage": false,
                                                    "endCursor": null
                                                }
                                            }
                                        }
                                    }
                                }
                            }),
                        );
                    }
                    _ => unreachable!(),
                }
            }
        });
        let mut api = RestGithubApi::new(&format!("http://{address}/")).unwrap();
        let imported = api
            .comments(
                "wire-secret",
                &GithubPullRequestLocator {
                    host: "github.com".into(),
                    owner: "o".into(),
                    repository: "r".into(),
                    pull_number: 1,
                },
            )
            .unwrap();
        assert_eq!(imported.len(), 3);
        let thread = imported
            .iter()
            .find(|comment| comment.id == "github-inline-11")
            .unwrap();
        assert_eq!(thread.kind, ImportedCommentKind::ReviewThreadComment);
        assert_eq!(thread.upstream_resolved, Some(true));
        let summary = imported
            .iter()
            .find(|comment| comment.id == "github-review-31")
            .unwrap();
        assert_eq!(summary.kind, ImportedCommentKind::ReviewSummary);
        assert_eq!(summary.upstream_review_state.as_deref(), Some("approved"));
        assert!(summary.body.is_empty());
        server.join().unwrap();
    }

    #[test]
    fn complete_blob_diff_is_derived_locally_and_binary_safe() {
        let diff = full_unified_diff("a.txt", Some("old\n"), Some("new\n"));
        assert!(diff.contains("--- a/a.txt"));
        assert!(diff.contains("-old"));
        assert!(diff.contains("+new"));
        assert_eq!(
            full_unified_diff("image.bin", None, None),
            "Binary file changed"
        );
    }
}
