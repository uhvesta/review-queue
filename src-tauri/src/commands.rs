//! Typed, local-only frontend-to-core command boundary.
//!
//! The webview may request domain operations, but never obtains a database,
//! filesystem, Git, socket, credential, or provider capability.

use std::{
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};

use review_queue_core::{
    AgentRoute, Anchor, Collection, Decision, DomainError, FormalComment, ReviewBrief, Round,
    acp::{PreparedFeedbackPrompt, prepare_feedback_prompt},
    adapters::{AskConversation, AskTurn, DiscoveredSessionOption},
    capture::{self, CaptureRequest},
    diff::{self, MaterializedDiff, PinnedFileContent},
    reproduction::{self, ReproductionPreview, ReproductionResult},
    store::{DeliveryHistoryEntry, Store, SubmissionResult},
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

pub struct AppState(pub Arc<Mutex<Store>>);

#[tauri::command]
pub fn application_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

#[derive(Debug, Serialize)]
pub struct CommandError {
    pub code: String,
    pub message: String,
    pub data_safety: String,
    pub next_step: String,
}

/// Loads the exact Git objects captured for a round. This is read-only: it
/// does not inspect the current worktree state or mutate the Store.
#[tauri::command]
pub fn materialize_round_diff(
    id: String,
    state: State<'_, AppState>,
) -> Result<MaterializedDiff, CommandError> {
    let store = state.0.lock().map_err(|_| unavailable())?;
    let round = store.round(&id)?;
    if round.collection == Collection::Machine {
        return store
            .machine_snapshot(&id)
            .map(|snapshot| review_queue_core::machine::materialize_snapshot(&snapshot))
            .map_err(Into::into);
    }
    diff::materialize_round(&round).map_err(Into::into)
}

#[tauri::command]
pub fn materialize_round_file(
    round_id: String,
    repository_id: String,
    path: String,
    side: String,
    state: State<'_, AppState>,
) -> Result<PinnedFileContent, CommandError> {
    let store = state.0.lock().map_err(|_| unavailable())?;
    let round = store.round(&round_id)?;
    if round.collection == Collection::Machine {
        return review_queue_core::machine::materialize_snapshot_file(
            &store.machine_snapshot(&round_id)?,
            &repository_id,
            &path,
            &side,
        )
        .map_err(Into::into);
    }
    diff::materialize_file(&round, &repository_id, &path, &side).map_err(Into::into)
}

/// Builds an exact local reconstruction plan without invoking Git or creating
/// the requested destination directory.
#[tauri::command]
pub fn preview_round_reproduction(
    request: ReproductionRequest,
    state: State<'_, AppState>,
) -> Result<ReproductionPreview, CommandError> {
    let store = state.0.lock().map_err(|_| unavailable())?;
    let round = store.round(&request.round_id)?;
    if round.collection == Collection::Github {
        return Err(CommandError {
            code: "github_reproduction_uses_cached_source".into(),
            message: "GitHub reproduction uses the locally cached pinned source.".into(),
            data_safety: "No URL was treated as a filesystem path and nothing was created.".into(),
            next_step: "Use github_preview_reproduction for this round.".into(),
        });
    }
    if round.collection == Collection::Machine {
        let snapshot = store.machine_snapshot(&request.round_id)?;
        drop(store);
        return review_queue_core::machine::preview_cached_git_reproduction(
            &snapshot,
            request.destination,
        )
        .map_err(Into::into);
    }
    drop(store);
    reproduction::preview(&round.manifest, request.destination).map_err(Into::into)
}

/// Creates detached local clones of a round's saved commits. It never changes
/// the original workspace and never creates a delivery action.
#[tauri::command]
pub fn materialize_round_reproduction(
    request: MaterializeReproductionRequest,
    state: State<'_, AppState>,
) -> Result<ReproductionResult, CommandError> {
    require_confirmation(&request.round_id, &request.confirmation, "reproduce")?;
    let store = state.0.lock().map_err(|_| unavailable())?;
    let round = store.round(&request.round_id)?;
    if round.collection == Collection::Github {
        return Err(CommandError {
            code: "github_reproduction_uses_cached_source".into(),
            message: "GitHub reproduction uses the locally cached pinned source.".into(),
            data_safety: "No URL was treated as a filesystem path and nothing was created.".into(),
            next_step: "Use github_materialize_reproduction for this round.".into(),
        });
    }
    if round.collection == Collection::Machine {
        let snapshot = store.machine_snapshot(&request.round_id)?;
        drop(store);
        return review_queue_core::machine::reproduce_cached_git_snapshot(
            &snapshot,
            request.destination,
        )
        .map_err(Into::into);
    }
    drop(store);
    reproduction::materialize(&round.manifest, request.destination, true).map_err(Into::into)
}

impl From<DomainError> for CommandError {
    fn from(error: DomainError) -> Self {
        let error = error.error;
        Self {
            code: error.code,
            message: error.what_happened,
            data_safety: error.data_safety,
            next_step: error.next_step,
        }
    }
}

fn unavailable() -> CommandError {
    CommandError {
        code: "desktop_state_unavailable".into(),
        message: "The Review Queue desktop state is temporarily unavailable.".into(),
        data_safety: "No review data was changed.".into(),
        next_step: "Wait briefly and retry. If this persists, restart the desktop app.".into(),
    }
}

pub fn initialize_store(path: impl AsRef<Path>) -> anyhow::Result<Store> {
    Store::open(path)
}

#[tauri::command]
pub fn open_keychain_access() -> Result<(), CommandError> {
    let application = Path::new("/System/Library/CoreServices/Applications/Keychain Access.app");
    let status = Command::new("/usr/bin/open")
        .arg(application)
        .status()
        .map_err(|_| CommandError {
            code: "keychain_access_launch_failed".into(),
            message: "Review Queue could not open Keychain Access.".into(),
            data_safety: "No credential or review data was changed.".into(),
            next_step: "Open Keychain Access manually, select login, unlock it, then return and choose Retry connection.".into(),
        })?;
    if !status.success() {
        return Err(CommandError {
            code: "keychain_access_launch_failed".into(),
            message: "macOS did not open Keychain Access.".into(),
            data_safety: "No credential or review data was changed.".into(),
            next_step: "Open Keychain Access manually, select login, unlock it, then return and choose Retry connection.".into(),
        });
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalSubmitRequest {
    pub workspace_path: String,
    pub topic: String,
    pub brief: ReviewBrief,
    /// Optional explicit route selection. When omitted, the shared Store
    /// boundary attaches the sole registered route whose saved cwd belongs to
    /// this workspace; ambiguous route sets are never guessed.
    #[serde(default)]
    pub origin_route_id: Option<String>,
    #[serde(default)]
    pub participating_repository_ids: Vec<String>,
    #[serde(default)]
    pub preflight_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Confirmation {
    /// Must be true and be accompanied by the round-specific token below.
    pub confirmed: bool,
    pub token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReproductionRequest {
    pub round_id: String,
    pub destination: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterializeReproductionRequest {
    pub round_id: String,
    pub destination: String,
    pub confirmation: Confirmation,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFormalCommentRequest {
    pub round_id: String,
    pub thread_id: String,
    pub body: String,
    pub anchor: Option<Anchor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditFormalCommentRequest {
    pub comment_id: String,
    pub body: String,
    pub anchor: Option<Anchor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFileViewedRequest {
    pub round_id: String,
    pub repository_id: String,
    pub path: String,
    pub viewed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveConversationRequest {
    pub round_id: String,
    #[serde(default)]
    pub options: Vec<DiscoveredSessionOption>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareFeedbackPromptRequest {
    pub round_id: String,
    #[serde(default)]
    pub route_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmManualSubmissionRequest {
    pub delivery_id: String,
    pub confirmation: Confirmation,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewedFile {
    pub repository_id: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitLocalResult {
    pub outcome: String,
    pub round: Round,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_round_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPreflight {
    pub repositories: Vec<LocalPreflightRepository>,
    pub before_fingerprint: String,
    pub participating_repository_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_route_id: Option<String>,
    pub preflight_token: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPreflightRepository {
    pub root: String,
    pub repository_id: String,
    pub branch: String,
    pub head_sha: String,
    pub status: String,
    pub has_changes: bool,
    pub participating: bool,
}

impl From<capture::Preflight> for LocalPreflight {
    fn from(value: capture::Preflight) -> Self {
        Self {
            repositories: value
                .repositories
                .into_iter()
                .map(|repository| LocalPreflightRepository {
                    root: repository.root.to_string_lossy().into_owned(),
                    repository_id: repository.repository_id,
                    branch: repository.branch,
                    head_sha: repository.head_sha,
                    status: repository.status,
                    has_changes: repository.has_changes,
                    participating: repository.participating,
                })
                .collect(),
            before_fingerprint: value.before_fingerprint,
            participating_repository_ids: value.participating_repository_ids,
            origin_route_id: value.origin_route_id,
            preflight_token: value.preflight_token,
        }
    }
}

fn capture_request(input: LocalSubmitRequest) -> CaptureRequest {
    CaptureRequest {
        workspace_root: input.workspace_path.into(),
        topic: input.topic,
        brief: input.brief,
        origin_route_id: input.origin_route_id,
        participating_repository_ids: input.participating_repository_ids,
        preflight_token: input.preflight_token,
    }
}

fn require_confirmation(
    id: &str,
    confirmation: &Confirmation,
    operation: &str,
) -> Result<(), CommandError> {
    if confirmation.confirmed && confirmation.token == format!("{operation}:{id}") {
        return Ok(());
    }
    Err(CommandError {
        code: "confirmation_required".into(),
        message: format!("{operation} requires explicit confirmation for this review round."),
        data_safety: "No review data or source files were changed.".into(),
        next_step: format!(
            "Confirm the dialog and provide the displayed token '{operation}:{id}'."
        ),
    })
}

/// Discovery is read-only and intentionally has no Store argument.
#[tauri::command]
pub fn discover_local(workspace_path: String) -> Result<Vec<String>, CommandError> {
    capture::discover_repositories(workspace_path)
        .map(|paths| {
            paths
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect()
        })
        .map_err(Into::into)
}

/// Runs the complete no-write validation pass for the exact later submission.
#[tauri::command]
pub fn preflight_local(
    request: LocalSubmitRequest,
    state: State<'_, AppState>,
) -> Result<LocalPreflight, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .preflight_local_capture(&capture_request(request))
        .map(LocalPreflight::from)
        .map_err(Into::into)
}

#[tauri::command]
pub fn submit_local(
    request: LocalSubmitRequest,
    state: State<'_, AppState>,
) -> Result<SubmitLocalResult, CommandError> {
    let request = capture_request(request);
    let mut store = state.0.lock().map_err(|_| unavailable())?;
    match store.ingest_local_capture(&request)? {
        SubmissionResult::Existing(round) => Ok(SubmitLocalResult {
            outcome: "existing".into(),
            round,
            superseded_round_id: None,
        }),
        SubmissionResult::Created(round) => Ok(SubmitLocalResult {
            outcome: "created".into(),
            round,
            superseded_round_id: None,
        }),
        SubmissionResult::Superseded { old_id, round } => Ok(SubmitLocalResult {
            outcome: "superseded".into(),
            round,
            superseded_round_id: Some(old_id),
        }),
    }
}

#[tauri::command]
pub fn list_rounds(
    collection: Option<Collection>,
    include_old: Option<bool>,
    state: State<'_, AppState>,
) -> Result<Vec<Round>, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .list(collection, include_old.unwrap_or(false))
        .map_err(Into::into)
}

#[tauri::command]
pub fn get_round(id: String, state: State<'_, AppState>) -> Result<Round, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .round(&id)
        .map_err(Into::into)
}

#[tauri::command]
pub fn list_viewed_files(
    round_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<ViewedFile>, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .viewed_files(&round_id)
        .map(|files| {
            files
                .into_iter()
                .map(|(repository_id, path)| ViewedFile {
                    repository_id,
                    path,
                })
                .collect()
        })
        .map_err(Into::into)
}

#[tauri::command]
pub fn set_file_viewed(
    request: SetFileViewedRequest,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .set_file_viewed(
            &request.round_id,
            &request.repository_id,
            &request.path,
            request.viewed,
        )
        .map_err(Into::into)
}

#[tauri::command]
pub fn edit_round_brief(
    id: String,
    brief: ReviewBrief,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .edit_brief(&id, &brief)
        .map_err(Into::into)
}

#[tauri::command]
pub fn get_round_decision(
    id: String,
    state: State<'_, AppState>,
) -> Result<Option<Decision>, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .decision(&id)
        .map_err(Into::into)
}

#[tauri::command]
pub fn request_changes(id: String, state: State<'_, AppState>) -> Result<(), CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .request_changes(&id)
        .map_err(Into::into)
}

#[tauri::command]
pub fn complete_round(id: String, state: State<'_, AppState>) -> Result<(), CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .complete(&id)
        .map_err(Into::into)
}

#[tauri::command]
pub fn requeue_round(id: String, state: State<'_, AppState>) -> Result<(), CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .requeue(&id)
        .map_err(Into::into)
}

#[tauri::command]
pub fn move_round(
    id: String,
    target_rank: i64,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .move_rank(&id, target_rank)
        .map_err(Into::into)
}

/// Deletes only app-owned persisted state. It never runs a Git command. The
/// caller must send `{ confirmed: true, token: "purge:<round id>" }`.
#[tauri::command]
pub fn purge_round(
    id: String,
    confirmation: Confirmation,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    require_confirmation(&id, &confirmation, "purge")?;
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .purge(&id)
        .map_err(Into::into)
}

/// Local approve is destructive by product design: after confirmation it
/// purges the app-owned round. Remote approvals remain adapter-owned and are
/// intentionally not exposed by this local boundary.
#[tauri::command]
pub fn approve_local(
    id: String,
    confirmation: Confirmation,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    require_confirmation(&id, &confirmation, "approve-local")?;
    let store = state.0.lock().map_err(|_| unavailable())?;
    let round = store.round(&id)?;
    if round.source_adapter.approval != review_queue_core::adapters::ApprovalDisposition::PurgeRound
    {
        return Err(CommandError {
            code: "local_approval_only".into(),
            message: "This review source records a decision instead of purging on approval.".into(),
            data_safety: "No review state was changed.".into(),
            next_step: "Use the source's decision action for this round.".into(),
        });
    }
    store.approve_local(&id).map_err(Into::into)
}

/// Remote approval records a local decision only. Publishing or preparing a
/// manual originating-agent handoff remains a separate explicit operation.
#[tauri::command]
pub fn approve_remote(id: String, state: State<'_, AppState>) -> Result<(), CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .approve_remote(&id)
        .map_err(Into::into)
}

#[tauri::command]
pub fn list_formal_comments(
    round_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<FormalComment>, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .formal_comments(&round_id)
        .map_err(Into::into)
}

#[tauri::command]
pub fn create_formal_comment(
    request: CreateFormalCommentRequest,
    state: State<'_, AppState>,
) -> Result<FormalComment, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .create_formal_comment(
            &request.round_id,
            &request.thread_id,
            &request.body,
            request.anchor.as_ref(),
        )
        .map_err(Into::into)
}

#[tauri::command]
pub fn edit_formal_comment(
    request: EditFormalCommentRequest,
    state: State<'_, AppState>,
) -> Result<FormalComment, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .edit_formal_comment(&request.comment_id, &request.body, request.anchor.as_ref())
        .map_err(Into::into)
}

#[tauri::command]
pub fn delete_formal_comment(
    comment_id: String,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .delete_formal_comment(&comment_id)
        .map_err(Into::into)
}

/// Persists an immutable feedback payload and prepares a copyable prompt.
///
/// Route/session state selects truthful manual handoff guidance only. This
/// command has no provider, socket, terminal, or prompt-injection capability.
#[tauri::command]
pub fn prepare_feedback_handoff(
    request: PrepareFeedbackPromptRequest,
    state: State<'_, AppState>,
) -> Result<PreparedFeedbackPrompt, CommandError> {
    let mut store = state.0.lock().map_err(|_| unavailable())?;
    store.round(&request.round_id)?;
    let route = request
        .route_id
        .as_deref()
        .map(|route_id| store.route(route_id))
        .transpose()?;
    // `prepare_delivery` performs the mutability gate before reusing any
    // pending immutable payload, so historical rounds cannot prepare anew.
    let delivery = store.prepare_delivery(&request.round_id)?;
    prepare_feedback_prompt(&delivery, route.as_ref()).map_err(Into::into)
}

#[tauri::command]
pub fn list_agent_routes(state: State<'_, AppState>) -> Result<Vec<AgentRoute>, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .routes()
        .map_err(Into::into)
}

/// Reads immutable handoff attempts for active and historical rounds.
#[tauri::command]
pub fn list_feedback_delivery_history(
    round_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<DeliveryHistoryEntry>, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .delivery_history(&round_id)
        .map_err(Into::into)
}

/// Records only the user's explicit acknowledgement that they submitted the
/// prepared prompt manually. It never communicates with an agent.
#[tauri::command]
pub fn confirm_manual_feedback_submission(
    request: ConfirmManualSubmissionRequest,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    require_confirmation(&request.delivery_id, &request.confirmation, "manual-submit")?;
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .mark_delivery_manually_submitted(&request.delivery_id)?;
    Ok(())
}

/// Creates/retrieves the one active chat. This never invokes a provider.
#[tauri::command]
pub fn active_conversation(
    request: ActiveConversationRequest,
    state: State<'_, AppState>,
) -> Result<AskConversation, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .active_conversation(&request.round_id, request.options)
        .map_err(Into::into)
}

/// Reads the active transcript without creating one. Historical and
/// superseded rounds remain readable but never become provider sessions.
#[tauri::command]
pub fn current_conversation(
    round_id: String,
    state: State<'_, AppState>,
) -> Result<Option<AskConversation>, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .current_conversation(&round_id)
        .map_err(Into::into)
}

#[tauri::command]
pub fn list_conversation_history(
    round_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<AskConversation>, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .conversation_history(&round_id)
        .map_err(Into::into)
}

#[tauri::command]
pub fn list_previous_chats(
    round_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<AskConversation>, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .previous_conversations(&round_id)
        .map_err(Into::into)
}

/// Archives the active transcript and makes a clean active chat without any prompt call.
#[tauri::command]
pub fn clear_chat(
    round_id: String,
    state: State<'_, AppState>,
) -> Result<AskConversation, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .clear_conversation(&round_id)
        .map_err(Into::into)
}

#[tauri::command]
pub fn queue_ask_turn(turn: AskTurn, state: State<'_, AppState>) -> Result<AskTurn, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .queue_ask_turn(turn)
        .map_err(Into::into)
}
#[tauri::command]
pub fn list_ask_turns(
    conversation_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<AskTurn>, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .ask_turns(&conversation_id)
        .map_err(Into::into)
}
#[tauri::command]
pub fn begin_ask_turn(id: String, state: State<'_, AppState>) -> Result<AskTurn, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .begin_ask_turn(&id)
        .map_err(Into::into)
}
#[tauri::command]
pub fn append_ask_chunk(
    id: String,
    text: String,
    state: State<'_, AppState>,
) -> Result<AskTurn, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .append_ask_chunk(&id, &text)
        .map_err(Into::into)
}
#[tauri::command]
pub fn complete_ask_turn(id: String, state: State<'_, AppState>) -> Result<AskTurn, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .complete_ask_turn(&id)
        .map_err(Into::into)
}
#[tauri::command]
pub fn cancel_ask_turn(id: String, state: State<'_, AppState>) -> Result<AskTurn, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .cancel_ask_turn(&id)
        .map_err(Into::into)
}
#[tauri::command]
pub fn fail_ask_turn(
    id: String,
    reason: String,
    state: State<'_, AppState>,
) -> Result<AskTurn, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .fail_ask_turn(&id, &reason)
        .map_err(Into::into)
}
#[tauri::command]
pub fn interrupt_ask_turn(
    id: String,
    reason: String,
    state: State<'_, AppState>,
) -> Result<AskTurn, CommandError> {
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .interrupt_ask_turn(&id, &reason)
        .map_err(Into::into)
}
