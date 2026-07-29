//! Credential-owning GitHub Copilot SDK adapter for the desktop process.
//!
//! The core crate owns lifecycle invariants; this module owns only the local
//! official SDK client and session boundary.
//! It deliberately never asks the CLI to log in, log out, print credentials,
//! or refresh a credential.  Opening, reopening, and clearing a local chat do
//! not start a provider prompt.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use chrono::Utc;
use github_copilot_sdk::{
    CliProgram, Client, ClientOptions, EventSubscription, InfiniteSessionConfig, SessionConfig,
    handler::DenyAllHandler, session::Session,
};
use review_queue_core::{
    Anchor, Round,
    adapters::{AskTurn, AskTurnState, DiscoveredSessionOption, SessionOptionKind},
    copilot::{
        CopilotAdapter, CopilotAdapterError, CopilotAuthSource, CopilotAuthState,
        CopilotAuthValidation, CopilotCapabilities, CopilotPromptEnvelope, CopilotTransport,
        CopilotTransportError, ExplicitPrompt, OptionApplyPolicy, OptionChangeResult,
        PromptCancelled, PromptStarted, PromptStreamUpdate, ProviderSession,
        ProviderSessionRequest, SessionOptionChoice, SessionOptionGroup, TransportStreamEvent,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;
use uuid::Uuid;

use crate::commands::{AppState, CommandError};
use review_queue_desktop::{
    connection_health::{CopilotCliProbe, SystemCopilotCliProbe},
    keychain_vault::{Capability, CredentialVault, MacOsKeychainBackend},
};

/// Copilot documents these variables as credential or bring-your-own-provider
/// inputs. The signed desktop child must inherit neither form of auth: it is
/// allowed to use only the user's existing `copilot-cli` Keychain record.
const STRIPPED_COPILOT_AUTH_ENV: &[&str] = &[
    "COPILOT_GITHUB_TOKEN",
    "GITHUB_COPILOT_API_TOKEN",
    "COPILOT_API_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "COPILOT_OFFLINE",
    "COPILOT_PROVIDER_BASE_URL",
    "COPILOT_PROVIDER_TYPE",
    "COPILOT_PROVIDER_API_KEY",
    "COPILOT_PROVIDER_BEARER_TOKEN",
    "COPILOT_PROVIDER_WIRE_API",
    "COPILOT_PROVIDER_TRANSPORT",
    "COPILOT_PROVIDER_AZURE_API_VERSION",
    "COPILOT_PROVIDER_MODEL_ID",
    "COPILOT_PROVIDER_WIRE_MODEL",
    "COPILOT_PROVIDER_MAX_PROMPT_TOKENS",
    "COPILOT_PROVIDER_MAX_OUTPUT_TOKENS",
    "COPILOT_PROVIDER_HEADERS",
];

/// Mutable desktop-only session state. There is intentionally one active
/// provider process at a time: a session belongs to exactly one persisted
/// conversation, and switching conversations requires an explicit end/clear.
pub struct CopilotDesktopState(Mutex<Option<ActiveCopilot>>);

struct ActiveCopilot {
    conversation_id: String,
    materialization_directory: PathBuf,
    adapter: CopilotAdapter<AcpCliTransport>,
}

impl CopilotDesktopState {
    pub fn new() -> Self {
        Self(Mutex::new(None))
    }

    fn start(
        &self,
        conversation_id: String,
        materialization_directory: PathBuf,
        auth_source: CopilotAuthSource,
        options: BTreeMap<String, String>,
    ) -> Result<CopilotSessionInfo, CopilotCommandError> {
        let mut active = self.0.lock().map_err(|_| unavailable())?;
        if let Some(current) = active.as_ref() {
            if current.conversation_id == conversation_id {
                return Err(conflict(
                    "copilot_session_already_active",
                    "This conversation already has an active Copilot session.",
                    "Use the existing session, or explicitly end it before starting another.",
                ));
            }
            return Err(conflict(
                "copilot_other_session_active",
                "Another conversation has an active Copilot session.",
                "End or clear that conversation before opening a different Copilot session.",
            ));
        }

        let mut adapter =
            CopilotAdapter::new(AcpCliTransport::new(materialization_directory.clone()));
        let auth = adapter.validate_auth(auth_source).map_err(adapter_error)?;
        let capabilities = adapter.discover_capabilities().map_err(adapter_error)?;
        let selected = selected_options(&capabilities, options)?;
        let session = adapter
            .start_session(conversation_id.clone(), selected.clone())
            .map_err(adapter_error)?;
        *active = Some(ActiveCopilot {
            conversation_id,
            materialization_directory,
            adapter,
        });
        Ok(CopilotSessionInfo {
            session_id: session.session_id,
            auth_source: auth.source,
            account: auth.account_label,
            capabilities,
            active_options: selected,
        })
    }

    fn change_option(
        &self,
        key: &str,
        value: &str,
    ) -> Result<OptionChangeResult, CopilotCommandError> {
        self.0
            .lock()
            .map_err(|_| unavailable())?
            .as_mut()
            .ok_or_else(|| {
                conflict(
                    "copilot_session_required",
                    "There is no active Copilot session.",
                    "Start a session before changing its options.",
                )
            })?
            .adapter
            .change_option(key, value)
            .map_err(adapter_error)
    }

    fn start_prompt(&self, prompt: ExplicitPrompt) -> Result<PromptStarted, CopilotCommandError> {
        self.0
            .lock()
            .map_err(|_| unavailable())?
            .as_mut()
            .ok_or_else(|| {
                conflict(
                    "copilot_session_required",
                    "There is no active Copilot session.",
                    "Start a session before sending a prompt.",
                )
            })?
            .adapter
            .start_prompt(prompt)
            .map_err(adapter_error)
    }

    fn poll(&self) -> Result<PromptStreamUpdate, CopilotCommandError> {
        self.0
            .lock()
            .map_err(|_| unavailable())?
            .as_mut()
            .ok_or_else(|| {
                conflict(
                    "copilot_session_required",
                    "There is no active Copilot session.",
                    "Start a session before polling a response.",
                )
            })?
            .adapter
            .poll_prompt()
            .map_err(adapter_error)
    }

    fn cancel(&self) -> Result<PromptCancelled, CopilotCommandError> {
        self.0
            .lock()
            .map_err(|_| unavailable())?
            .as_mut()
            .ok_or_else(|| {
                conflict(
                    "copilot_session_required",
                    "There is no active Copilot session.",
                    "No Copilot session is active.",
                )
            })?
            .adapter
            .cancel_prompt()
            .map_err(adapter_error)
    }

    fn end_for(&self, conversation_id: Option<&str>) -> Result<bool, CopilotCommandError> {
        let mut active = self.0.lock().map_err(|_| unavailable())?;
        let matches = active
            .as_ref()
            .is_some_and(|current| conversation_id.is_none_or(|id| id == current.conversation_id));
        if !matches {
            return Ok(false);
        }
        let mut current = active.take().expect("checked above");
        current.adapter.end_session().map_err(adapter_error)?;
        // This is an app-owned directory created for this one session ID;
        // never point cleanup at captured worktrees or remote workspace paths.
        let _ = fs::remove_dir_all(current.materialization_directory);
        Ok(true)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotSessionInfo {
    pub session_id: String,
    pub auth_source: CopilotAuthSource,
    pub account: Option<String>,
    pub capabilities: CopilotCapabilities,
    pub active_options: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotPollResult {
    pub turn: AskTurn,
    pub update: PromptStreamUpdate,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartCopilotSessionRequest {
    pub round_id: String,
    pub conversation_id: String,
    #[serde(default)]
    pub auth_source: Option<CopilotAuthSource>,
    #[serde(default)]
    pub option_values: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeCopilotOptionRequest {
    pub round_id: String,
    pub conversation_id: String,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendCopilotPromptRequest {
    pub round_id: String,
    pub conversation_id: String,
    pub prompt: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub anchor: Option<Anchor>,
    #[serde(default)]
    pub option_values: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PollCopilotPromptRequest {
    pub turn_id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotCommandError {
    pub code: String,
    pub message: String,
    pub data_safety: String,
    pub next_step: String,
    pub retryable: bool,
}

/// The only content sent to an ACP model for an Ask turn. It intentionally
/// contains one immutable snapshot and one freshly authored question; prior
/// turns, live worktree state, credentials, and provider session history are
/// never copied into this request.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AskPromptEnvelope<'a> {
    protocol: &'static str,
    round: AskPromptRound<'a>,
    conversation_id: &'a str,
    anchor: Option<AskPromptAnchor<'a>>,
    user_question: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AskPromptRound<'a> {
    id: &'a str,
    collection: &'a str,
    topic_identity: &'a str,
    manifest_hash: &'a str,
    workspace_id: &'a str,
    workspace_topic: &'a str,
    before_fingerprint: &'a str,
    after_fingerprint: &'a str,
    repositories: Vec<AskPromptRepository<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AskPromptRepository<'a> {
    repository_id: &'a str,
    base_sha: &'a str,
    head_sha: &'a str,
    object_checksum: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AskPromptAnchor<'a> {
    repository_id: &'a str,
    workspace_relative_path: &'a str,
    side: &'a str,
    start_line: u32,
    end_line: u32,
    blob_sha: &'a str,
    selected_code: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopilotCapabilityRequest {
    #[serde(default)]
    pub auth_source: Option<CopilotAuthSource>,
}

/// Discover the signed-in user's real model catalog without creating a
/// session or sending a prompt. The SDK owns the process protocol.
#[tauri::command]
pub fn copilot_capabilities(
    request: Option<CopilotCapabilityRequest>,
) -> Result<CopilotCapabilities, CopilotCommandError> {
    let directory = std::env::current_dir().map_err(|_| unavailable())?;
    let source = request
        .and_then(|request| request.auth_source)
        .map(Ok)
        .unwrap_or_else(selected_auth_source)?;
    let mut adapter = CopilotAdapter::new(AcpCliTransport::new(directory));
    adapter.validate_auth(source).map_err(adapter_error)?;
    adapter.discover_capabilities().map_err(adapter_error)
}

/// Starts an ACP process only after an explicit user action. The conversation
/// opening path stays entirely local and is intentionally separate.
#[tauri::command]
pub fn copilot_start_session(
    request: StartCopilotSessionRequest,
    desktop: State<'_, CopilotDesktopState>,
    state: State<'_, AppState>,
) -> Result<CopilotSessionInfo, CopilotCommandError> {
    let auth_source = match request.auth_source {
        Some(source) => source,
        None => selected_auth_source()?,
    };
    let materialization_directory = {
        let store = state.0.lock().map_err(|_| unavailable())?;
        let round = store.round(&request.round_id).map_err(domain_error)?;
        ensure_conversation_matches_round(&store, &request.round_id, &request.conversation_id)?;
        materialize_copilot_session_directory(&store, &round, &request.conversation_id)?
    };
    let started = desktop.start(
        request.conversation_id.clone(),
        materialization_directory,
        auth_source,
        request.option_values,
    );
    let session = match started {
        Ok(session) => session,
        Err(error) => {
            // The directory was created only for this attempted session.
            let root = std::env::temp_dir()
                .join("review-queue-copilot")
                .join(&request.round_id)
                .join(&request.conversation_id);
            let _ = fs::remove_dir_all(root);
            return Err(error);
        }
    };

    let persisted_options = session
        .capabilities
        .option_groups
        .iter()
        .map(|group| DiscoveredSessionOption {
            key: group.key.clone(),
            label: group.label.clone(),
            kind: SessionOptionKind::Select,
            values: group
                .choices
                .iter()
                .map(|choice| choice.value.clone())
                .collect(),
            selected: session.active_options.get(&group.key).cloned(),
            supported: group.supported,
            unavailable_reason: group.unsupported_reason.clone(),
        })
        .collect::<Vec<_>>();
    let auth_label = match session.auth_source {
        CopilotAuthSource::ExistingCliSignInReadOnly => "existing CLI sign-in",
        CopilotAuthSource::AppOwnedOauth => "app-owned OAuth",
    };
    let provider_label = session.account.as_deref().map_or_else(
        || format!("Copilot via {auth_label}"),
        |account| format!("Copilot via {auth_label} ({account})"),
    );
    let persisted = state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .mark_conversation_provider_started(
            &request.conversation_id,
            &provider_label,
            &persisted_options,
        )
        .map_err(domain_error);
    if let Err(error) = persisted {
        let _ = desktop.end_for(Some(&request.conversation_id));
        return Err(error);
    }

    Ok(session)
}

fn selected_auth_source() -> Result<CopilotAuthSource, CopilotCommandError> {
    let vault = CredentialVault::new(MacOsKeychainBackend::new());
    let opted_out = vault
        .get(Capability::CopilotCliOptOut)
        .map_err(|_| unavailable())?
        .is_some();
    let cli = SystemCopilotCliProbe.validate_read_only();
    if !opted_out && cli.installed && cli.signed_in {
        return Ok(CopilotAuthSource::ExistingCliSignInReadOnly);
    }
    if vault
        .get_app_credential(Capability::CopilotApp)
        .map_err(|_| unavailable())?
        .is_some()
    {
        return Ok(CopilotAuthSource::AppOwnedOauth);
    }
    Err(CopilotCommandError {
        code: "copilot_connection_required".into(),
        message: "Copilot is not connected for Review Queue.".into(),
        data_safety: "No prompt was created or sent.".into(),
        next_step: "Open Connection settings and connect Copilot, then retry.".into(),
        retryable: false,
    })
}

/// Creates the only filesystem view available to a Copilot session. GitHub
/// and machine manifests contain remote URL/path metadata, never a usable
/// local cwd; their already-cached immutable blobs are reproduced here.
fn materialize_copilot_session_directory(
    store: &review_queue_core::store::Store,
    round: &Round,
    conversation_id: &str,
) -> Result<PathBuf, CopilotCommandError> {
    let root = std::env::temp_dir()
        .join("review-queue-copilot")
        .join(&round.id);
    fs::create_dir_all(&root).map_err(|_| unavailable())?;
    let destination = root.join(conversation_id);
    if destination.exists() {
        return Err(conflict(
            "copilot_materialization_already_exists",
            "This Copilot session directory already exists.",
            "Clear the chat and start a fresh session; Review Queue will not reuse provider context.",
        ));
    }
    match round.collection {
        review_queue_core::Collection::Github => {
            let github = store.github_round(&round.id).map_err(domain_error)?;
            review_queue_core::github::reproduce(&round.manifest, &github, &destination)
                .map_err(domain_error)?;
        }
        review_queue_core::Collection::Machine => {
            let snapshot = store.machine_snapshot(&round.id).map_err(domain_error)?;
            review_queue_core::machine::reproduce_snapshot(&snapshot, &destination)
                .map_err(domain_error)?;
        }
        review_queue_core::Collection::Local => {
            review_queue_core::reproduction::materialize(&round.manifest, &destination, true)
                .map_err(domain_error)?;
        }
    }
    Ok(destination)
}

fn ensure_conversation_matches_round(
    store: &review_queue_core::store::Store,
    round_id: &str,
    conversation_id: &str,
) -> Result<review_queue_core::adapters::AskConversation, CopilotCommandError> {
    let conversation = store
        .current_conversation(round_id)
        .map_err(domain_error)?
        .ok_or_else(|| {
            conflict(
                "copilot_conversation_required",
                "This round has no active Copilot conversation.",
                "Open the current chat before starting a session or sending a prompt.",
            )
        })?;
    if conversation.id != conversation_id {
        return Err(conflict(
            "copilot_conversation_round_mismatch",
            "The selected Copilot conversation belongs to a different review round.",
            "Reopen the intended round and use its current chat.",
        ));
    }
    Ok(conversation)
}

#[tauri::command]
pub fn copilot_change_option(
    request: ChangeCopilotOptionRequest,
    desktop: State<'_, CopilotDesktopState>,
    state: State<'_, AppState>,
) -> Result<OptionChangeResult, CopilotCommandError> {
    let previous_value = {
        let store = state.0.lock().map_err(|_| unavailable())?;
        let conversation =
            ensure_conversation_matches_round(&store, &request.round_id, &request.conversation_id)?;
        conversation
            .options
            .iter()
            .find(|option| option.key == request.key)
            .and_then(|option| option.selected.clone())
    };
    let changed = desktop.change_option(&request.key, &request.value)?;
    let persisted = state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .update_conversation_option_selection(
            &request.conversation_id,
            &request.key,
            &request.value,
        )
        .map_err(domain_error);
    if let Err(error) = persisted {
        if let Some(previous_value) = previous_value {
            let _ = desktop.change_option(&request.key, &previous_value);
        }
        return Err(error);
    }
    Ok(changed)
}

/// Queue first, then make exactly one provider request for this durable turn.
#[tauri::command]
pub fn copilot_send_prompt(
    request: SendCopilotPromptRequest,
    desktop: State<'_, CopilotDesktopState>,
    state: State<'_, AppState>,
) -> Result<AskTurn, CopilotCommandError> {
    let (queued, round) = {
        let mut store = state.0.lock().map_err(|_| unavailable())?;
        let round = store.round(&request.round_id).map_err(domain_error)?;
        ensure_conversation_matches_round(&store, &request.round_id, &request.conversation_id)?;
        let turn = AskTurn {
            id: Uuid::new_v4().to_string(),
            conversation_id: request.conversation_id.clone(),
            idempotency_key: request.idempotency_key.clone(),
            prompt: request.prompt.clone(),
            anchor: request.anchor.clone(),
            option_values: request.option_values,
            state: AskTurnState::Queued,
            created_at: Utc::now(),
            completed_at: None,
            failure_reason: None,
            response_text: String::new(),
        };
        (store.queue_ask_turn(turn).map_err(domain_error)?, round)
    };
    if queued.state != AskTurnState::Queued {
        return Err(conflict(
            "copilot_prompt_already_consumed",
            "That explicit prompt was already attempted.",
            "Create a new prompt with a new idempotency key; this prompt was not replayed.",
        ));
    }
    let structured_prompt = ask_prompt_envelope(
        &round,
        &queued.conversation_id,
        queued.anchor.as_ref(),
        &queued.prompt,
    )?;
    let prompt = ExplicitPrompt {
        prompt_id: queued.id.clone(),
        conversation_id: queued.conversation_id.clone(),
        idempotency_key: queued.idempotency_key.clone(),
        text: structured_prompt,
    };
    match desktop.start_prompt(prompt) {
        Ok(_) => state
            .0
            .lock()
            .map_err(|_| unavailable())?
            .begin_ask_turn(&queued.id)
            .map_err(domain_error),
        Err(error) => {
            let _ = state
                .0
                .lock()
                .map_err(|_| unavailable())?
                .fail_ask_turn(&queued.id, &error.message);
            Err(error)
        }
    }
}

fn ask_prompt_envelope(
    round: &Round,
    conversation_id: &str,
    anchor: Option<&Anchor>,
    user_question: &str,
) -> Result<String, CopilotCommandError> {
    if let Some(anchor) = anchor
        && !round
            .manifest
            .repositories
            .iter()
            .any(|repository| repository.repository_id == anchor.repository_id)
    {
        return Err(conflict(
            "ask_anchor_repository_not_in_round",
            "The selected code belongs to a repository outside this immutable review round.",
            "Select code from a file captured in this round, then ask again.",
        ));
    }
    let manifest = &round.manifest;
    let envelope = AskPromptEnvelope {
        protocol: "review_queue.ask.v1",
        round: AskPromptRound {
            id: &round.id,
            collection: round.collection.as_str(),
            topic_identity: &round.topic_identity,
            manifest_hash: &round.manifest_hash,
            workspace_id: &manifest.workspace_id,
            workspace_topic: &manifest.topic,
            before_fingerprint: &manifest.before_fingerprint,
            after_fingerprint: &manifest.after_fingerprint,
            repositories: manifest
                .repositories
                .iter()
                .map(|repository| AskPromptRepository {
                    repository_id: &repository.repository_id,
                    base_sha: &repository.base_sha,
                    head_sha: &repository.head_sha,
                    object_checksum: &repository.object_checksum,
                })
                .collect(),
        },
        conversation_id,
        anchor: anchor.map(|anchor| AskPromptAnchor {
            repository_id: &anchor.repository_id,
            workspace_relative_path: &anchor.workspace_relative_path,
            side: &anchor.side,
            start_line: anchor.start_line,
            end_line: anchor.end_line,
            blob_sha: &anchor.blob_sha,
            selected_code: &anchor.selected_code,
        }),
        user_question,
    };
    serde_json::to_string(&envelope).map_err(|_| CopilotCommandError {
        code: "ask_prompt_envelope_encode_failed".into(),
        message: "Review Queue could not prepare the immutable ask context.".into(),
        data_safety: "No provider request was made and the original question remains local.".into(),
        next_step: "Retry the explicit question. If this persists, restart Review Queue.".into(),
        retryable: true,
    })
}

/// Polls one newline-delimited ACP event and durably appends a chunk before it
/// becomes visible to the webview. The prompt is never replayed on restart.
#[tauri::command]
pub fn copilot_poll_prompt(
    request: PollCopilotPromptRequest,
    desktop: State<'_, CopilotDesktopState>,
    state: State<'_, AppState>,
) -> Result<CopilotPollResult, CopilotCommandError> {
    let update = desktop.poll()?;
    let store = state.0.lock().map_err(|_| unavailable())?;
    let turn = match &update {
        PromptStreamUpdate::Chunk {
            prompt_id, text, ..
        } => {
            ensure_turn(prompt_id, &request.turn_id)?;
            store
                .append_ask_chunk(prompt_id, text)
                .map_err(domain_error)?
        }
        PromptStreamUpdate::Completed { prompt_id } => {
            ensure_turn(prompt_id, &request.turn_id)?;
            store.complete_ask_turn(prompt_id).map_err(domain_error)?
        }
        PromptStreamUpdate::Failed { prompt_id, error } => {
            ensure_turn(prompt_id, &request.turn_id)?;
            store
                .fail_ask_turn(prompt_id, &error.what_happened)
                .map_err(domain_error)?
        }
    };
    Ok(CopilotPollResult { turn, update })
}

#[tauri::command]
pub fn copilot_cancel_prompt(
    turn_id: String,
    desktop: State<'_, CopilotDesktopState>,
    state: State<'_, AppState>,
) -> Result<AskTurn, CopilotCommandError> {
    let cancelled = desktop.cancel()?;
    ensure_turn(&cancelled.prompt_id, &turn_id)?;
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .cancel_ask_turn(&turn_id)
        .map_err(domain_error)
}

#[tauri::command]
pub fn copilot_end_session(
    conversation_id: Option<String>,
    desktop: State<'_, CopilotDesktopState>,
) -> Result<bool, CopilotCommandError> {
    desktop.end_for(conversation_id.as_deref())
}

/// Clears the durable transcript only after its matching provider session has
/// been ended. If no ACP session exists this remains a local SQLite-only
/// action. Neither branch creates a provider prompt or replays history.
#[tauri::command]
pub fn copilot_clear_chat(
    round_id: String,
    conversation_id: String,
    desktop: State<'_, CopilotDesktopState>,
    state: State<'_, AppState>,
) -> Result<review_queue_core::adapters::AskConversation, CopilotCommandError> {
    let _ = desktop.end_for(Some(&conversation_id))?;
    state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .clear_conversation(&round_id)
        .map_err(domain_error)
}

fn ensure_turn(expected: &str, actual: &str) -> Result<(), CopilotCommandError> {
    if expected == actual {
        Ok(())
    } else {
        Err(conflict(
            "copilot_prompt_mismatch",
            "The Copilot stream belongs to a different prompt.",
            "Keep the current transcript open and poll the matching prompt.",
        ))
    }
}

fn selected_options(
    capabilities: &CopilotCapabilities,
    requested: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, CopilotCommandError> {
    let mut selected = capabilities.selected_options();
    for (key, value) in requested {
        let group = capabilities
            .option_groups
            .iter()
            .find(|group| group.key == key)
            .ok_or_else(|| {
                conflict(
                    "unknown_option_group",
                    "Copilot did not advertise that option.",
                    "Refresh capabilities and choose an advertised option.",
                )
            })?;
        if !group.supported || !group.choices.iter().any(|choice| choice.value == value) {
            return Err(conflict(
                "copilot_option_value_unavailable",
                "The selected Copilot option is not available.",
                "Choose a value advertised by the current Copilot CLI.",
            ));
        }
        selected.insert(key, value);
    }
    Ok(selected)
}

fn adapter_error(error: CopilotAdapterError) -> CopilotCommandError {
    CopilotCommandError {
        code: error.code,
        message: error.what_happened,
        data_safety: error.data_safety,
        next_step: error.next_step,
        retryable: error.retryable,
    }
}

fn domain_error(error: review_queue_core::DomainError) -> CopilotCommandError {
    let error: CommandError = error.into();
    CopilotCommandError {
        code: error.code,
        message: error.message,
        data_safety: error.data_safety,
        next_step: error.next_step,
        retryable: false,
    }
}

fn unavailable() -> CopilotCommandError {
    CopilotCommandError {
        code: "copilot_desktop_state_unavailable".into(),
        message: "The local Copilot desktop state is temporarily unavailable.".into(),
        data_safety: "No prompt or credential was changed.".into(),
        next_step: "Wait briefly and retry. If this persists, restart Review Queue.".into(),
        retryable: true,
    }
}

fn conflict(code: &str, message: &str, next_step: &str) -> CopilotCommandError {
    CopilotCommandError {
        code: code.into(),
        message: message.into(),
        data_safety: "No prompt was replayed and the existing transcript is preserved.".into(),
        next_step: next_step.into(),
        retryable: false,
    }
}

/// Official GitHub Copilot SDK transport. The SDK owns the JSON-RPC process
/// lifecycle and sends an app-owned OAuth token only through its dedicated
/// child-only `COPILOT_SDK_AUTH_TOKEN` channel (`--auth-token-env`), never via
/// argv, browser IPC, SQLite, or an inherited environment variable.
struct AcpCliTransport {
    working_directory: PathBuf,
    runtime: tokio::runtime::Runtime,
    auth: Option<SdkAuthentication>,
    client: Option<Client>,
    session: Option<Session>,
    events: Option<EventSubscription>,
}

enum SdkAuthentication {
    ExistingCli,
    AppOwned { token: String },
}

impl AcpCliTransport {
    fn new(working_directory: PathBuf) -> Self {
        Self {
            working_directory,
            runtime: tokio::runtime::Runtime::new()
                .expect("Review Queue requires the bundled Tokio runtime"),
            auth: None,
            client: None,
            session: None,
            events: None,
        }
    }

    fn client_options(&self) -> Result<ClientOptions, CopilotTransportError> {
        let auth = self
            .auth
            .as_ref()
            .ok_or(CopilotTransportError::AuthenticationUnavailable)?;
        let options = ClientOptions::new()
            .with_program(CliProgram::Path(PathBuf::from("copilot")))
            .with_cwd(self.working_directory.clone())
            .with_env_remove(STRIPPED_COPILOT_AUTH_ENV.iter().copied());
        Ok(match auth {
            // The official SDK requires its logged-in-user path to read the
            // existing Copilot CLI Keychain session. Review Queue selects this
            // branch only after a read-only signed-in probe succeeds.
            SdkAuthentication::ExistingCli => options.with_use_logged_in_user(true),
            SdkAuthentication::AppOwned { token } => {
                // The official SDK places this only in the spawned child's
                // private environment and passes the variable *name* in argv.
                options
                    .with_use_logged_in_user(false)
                    .with_github_token(token.clone())
            }
        })
    }

    fn ensure_client(&mut self) -> Result<Client, CopilotTransportError> {
        if self.client.is_none() {
            let options = self.client_options()?;
            let client = self
                .runtime
                .block_on(Client::start(options))
                .map_err(|_| CopilotTransportError::AuthenticationUnavailable)?;
            self.client = Some(client);
        }
        self.client
            .as_ref()
            .cloned()
            .ok_or(CopilotTransportError::AuthenticationUnavailable)
    }

    fn discovered_capabilities(models: Vec<github_copilot_sdk::Model>) -> CopilotCapabilities {
        let model_choices = models
            .iter()
            .map(|model| SessionOptionChoice {
                value: model.id.clone(),
                label: model.name.clone(),
            })
            .collect::<Vec<_>>();
        let reasoning_choices = models
            .iter()
            .flat_map(|model| model.supported_reasoning_efforts.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|value| SessionOptionChoice {
                label: value.clone(),
                value,
            })
            .collect::<Vec<_>>();
        let mut option_groups = Vec::new();
        if !model_choices.is_empty() {
            option_groups.push(SessionOptionGroup {
                key: "model".into(),
                label: "Model".into(),
                supported: true,
                unsupported_reason: None,
                apply_policy: OptionApplyPolicy::AppliesNow,
                selected: model_choices.first().map(|choice| choice.value.clone()),
                choices: model_choices,
            });
        }
        if !reasoning_choices.is_empty() {
            option_groups.push(SessionOptionGroup {
                key: "reasoning_effort".into(),
                label: "Reasoning effort".into(),
                supported: true,
                unsupported_reason: None,
                apply_policy: OptionApplyPolicy::RequiresFreshSession,
                selected: models
                    .iter()
                    .find_map(|model| model.default_reasoning_effort.clone()),
                choices: reasoning_choices,
            });
        }
        option_groups.push(SessionOptionGroup {
            key: "context_window".into(),
            label: "Context window".into(),
            supported: true,
            unsupported_reason: None,
            apply_policy: OptionApplyPolicy::RequiresFreshSession,
            selected: Some("managed_80".into()),
            choices: vec![
                SessionOptionChoice {
                    value: "managed_80".into(),
                    label: "Managed · compact at 80%".into(),
                },
                SessionOptionChoice {
                    value: "managed_65".into(),
                    label: "Managed · compact early at 65%".into(),
                },
                SessionOptionChoice {
                    value: "native".into(),
                    label: "Native model window · no compaction".into(),
                },
            ],
        });
        CopilotCapabilities {
            supported: true,
            unsupported_reason: None,
            option_groups,
        }
    }
}

impl CopilotTransport for AcpCliTransport {
    fn validate_existing_cli_sign_in_read_only(
        &mut self,
    ) -> Result<CopilotAuthValidation, CopilotTransportError> {
        let status = SystemCopilotCliProbe.validate_read_only();
        if !status.installed || !status.signed_in {
            return Err(CopilotTransportError::AuthenticationUnavailable);
        }
        self.auth = Some(SdkAuthentication::ExistingCli);
        Ok(CopilotAuthValidation {
            source: CopilotAuthSource::ExistingCliSignInReadOnly,
            state: CopilotAuthState::Connected,
            account_label: status.account,
        })
    }

    fn validate_app_owned_oauth(&mut self) -> Result<CopilotAuthValidation, CopilotTransportError> {
        let vault = CredentialVault::new(MacOsKeychainBackend::new());
        let record = vault
            .get_app_credential(Capability::CopilotApp)
            .map_err(|_| CopilotTransportError::AuthenticationUnavailable)?
            .ok_or(CopilotTransportError::AuthenticationUnavailable)?;
        let account = record.account_label.clone();
        self.auth = Some(SdkAuthentication::AppOwned {
            token: record.access_token,
        });
        Ok(CopilotAuthValidation {
            source: CopilotAuthSource::AppOwnedOauth,
            state: CopilotAuthState::Connected,
            account_label: account,
        })
    }

    fn discover_capabilities(&mut self) -> Result<CopilotCapabilities, CopilotTransportError> {
        let client = self.ensure_client()?;
        let models = self
            .runtime
            .block_on(client.list_models())
            .map_err(|_| CopilotTransportError::ProviderRejected)?;
        Ok(Self::discovered_capabilities(models))
    }

    fn start_session(
        &mut self,
        request: ProviderSessionRequest,
    ) -> Result<ProviderSession, CopilotTransportError> {
        if self.session.is_some() {
            return Err(CopilotTransportError::ProviderRejected);
        }
        let client = self.ensure_client()?;
        let mut config = SessionConfig::default()
            .with_client_name("Review Queue")
            .with_streaming(true)
            .with_working_directory(self.working_directory.clone())
            .with_available_tools(Vec::<String>::new())
            .with_enable_config_discovery(false)
            .with_mcp_servers(HashMap::new())
            .with_permission_handler(Arc::new(DenyAllHandler));
        if let Some(model) = request.options.get("model") {
            config = config.with_model(model.clone());
        }
        if let Some(effort) = request.options.get("reasoning_effort") {
            config = config.with_reasoning_effort(effort.clone());
        }
        config = match request.options.get("context_window").map(String::as_str) {
            Some("managed_65") => config.with_infinite_sessions(
                InfiniteSessionConfig::new()
                    .with_enabled(true)
                    .with_background_compaction_threshold(0.65)
                    .with_buffer_exhaustion_threshold(0.9),
            ),
            Some("native") => {
                config.with_infinite_sessions(InfiniteSessionConfig::new().with_enabled(false))
            }
            _ => config.with_infinite_sessions(
                InfiniteSessionConfig::new()
                    .with_enabled(true)
                    .with_background_compaction_threshold(0.8)
                    .with_buffer_exhaustion_threshold(0.95),
            ),
        };
        let session = self
            .runtime
            .block_on(client.create_session(config))
            .map_err(|_| CopilotTransportError::ProviderRejected)?;
        let session_id = session.id().to_string();
        self.events = Some(session.subscribe());
        self.session = Some(session);
        Ok(ProviderSession { session_id })
    }

    fn apply_options(
        &mut self,
        _session_id: &str,
        options: BTreeMap<String, String>,
    ) -> Result<(), CopilotTransportError> {
        let session = self
            .session
            .as_ref()
            .ok_or(CopilotTransportError::ProviderRejected)?;
        if let Some(model) = options.get("model") {
            self.runtime
                .block_on(session.set_model(model, None))
                .map_err(|_| CopilotTransportError::ModelUnavailable)?;
        }
        Ok(())
    }

    fn start_prompt(
        &mut self,
        envelope: CopilotPromptEnvelope,
    ) -> Result<String, CopilotTransportError> {
        let session = self
            .session
            .as_ref()
            .ok_or(CopilotTransportError::ProviderRejected)?;
        self.runtime
            .block_on(session.send(envelope.prompt))
            .map_err(|_| CopilotTransportError::ProviderRejected)
    }

    fn poll_stream(
        &mut self,
        _stream_id: &str,
    ) -> Result<TransportStreamEvent, CopilotTransportError> {
        let events = self
            .events
            .as_mut()
            .ok_or(CopilotTransportError::ProviderRejected)?;
        loop {
            let event = self
                .runtime
                .block_on(events.recv())
                .map_err(|_| CopilotTransportError::NetworkUnavailable)?;
            if event.event_type == "assistant.message_delta" {
                if let Some(text) = event.data.get("deltaContent").and_then(Value::as_str) {
                    return Ok(TransportStreamEvent::Chunk { text: text.into() });
                }
            } else if event.event_type == "session.idle" {
                return Ok(TransportStreamEvent::Completed);
            } else if event.event_type == "session.error" {
                return Err(CopilotTransportError::ProviderRejected);
            }
        }
    }

    fn cancel_prompt(&mut self, _stream_id: &str) -> Result<(), CopilotTransportError> {
        let session = self
            .session
            .as_ref()
            .ok_or(CopilotTransportError::ProviderRejected)?;
        self.runtime
            .block_on(session.abort())
            .map_err(|_| CopilotTransportError::NetworkUnavailable)
    }

    fn end_session(&mut self, _session_id: &str) -> Result<(), CopilotTransportError> {
        self.events = None;
        let mut completed_session_id = None;
        if let Some(session) = self.session.take() {
            completed_session_id = Some(session.id().clone());
            self.runtime
                .block_on(session.disconnect())
                .map_err(|_| CopilotTransportError::NetworkUnavailable)?;
        }
        if let Some(client) = self.client.take() {
            if let Some(session_id) = completed_session_id.as_ref() {
                // The SDK's disconnect intentionally preserves provider-side
                // session files for resume. Review Queue never resumes a
                // provider session, so remove those files before stopping.
                self.runtime
                    .block_on(client.delete_session(session_id))
                    .map_err(|_| CopilotTransportError::NetworkUnavailable)?;
            }
            self.runtime
                .block_on(client.stop())
                .map_err(|_| CopilotTransportError::NetworkUnavailable)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use review_queue_core::copilot::LocalConversationAction;
    use review_queue_core::{
        Collection, Lifecycle, RepositorySnapshot, ReviewBrief, WorkspaceManifest,
    };
    use serde_json::json;

    fn immutable_round() -> Round {
        Round {
            id: "round-immutable-1".into(),
            collection: Collection::Local,
            topic_identity: "workspace-1:topic-1".into(),
            manifest_hash: "manifest-hash-1".into(),
            brief: ReviewBrief {
                title: "Review topic".into(),
                what: String::new(),
                why: String::new(),
                approach_alternatives: String::new(),
                testing: String::new(),
            },
            manifest: WorkspaceManifest {
                workspace_id: "workspace-1".into(),
                workspace_root: "/private/local/workspace".into(),
                topic: "topic-1".into(),
                repositories: vec![RepositorySnapshot {
                    repository_id: "repo-main".into(),
                    root: "/private/local/workspace".into(),
                    branch: "topic-1".into(),
                    base_sha: "base-sha-1".into(),
                    head_sha: "head-sha-1".into(),
                    remote_fingerprint: None,
                    object_checksum: "tree-sha-1".into(),
                }],
                before_fingerprint: "before-1".into(),
                after_fingerprint: "after-1".into(),
                created_at: Utc::now(),
            },
            rank: 0,
            lifecycle: Lifecycle::Queued,
            superseded_by: None,
            created_at: Utc::now(),
            origin_route_id: None,
            origin_route: None,
            source_metadata: None,
        }
    }

    #[derive(Default)]
    struct FakeAcpServerTranscript {
        frames: Vec<Value>,
        chunks: VecDeque<String>,
    }

    impl CopilotTransport for FakeAcpServerTranscript {
        fn validate_existing_cli_sign_in_read_only(
            &mut self,
        ) -> Result<CopilotAuthValidation, CopilotTransportError> {
            self.frames.push(json!({ "method": "auth/metadata-only" }));
            Ok(CopilotAuthValidation {
                source: CopilotAuthSource::ExistingCliSignInReadOnly,
                state: CopilotAuthState::Connected,
                account_label: Some("signed-in-user".into()),
            })
        }

        fn validate_app_owned_oauth(
            &mut self,
        ) -> Result<CopilotAuthValidation, CopilotTransportError> {
            Err(CopilotTransportError::AuthenticationUnavailable)
        }

        fn discover_capabilities(&mut self) -> Result<CopilotCapabilities, CopilotTransportError> {
            self.frames.push(json!({ "method": "initialize" }));
            Ok(AcpCliTransport::discovered_capabilities(vec![
                github_copilot_sdk::Model {
                    id: "official-model".into(),
                    name: "Official model".into(),
                    supported_reasoning_efforts: vec!["low".into(), "high".into()],
                    default_reasoning_effort: Some("high".into()),
                    ..Default::default()
                },
            ]))
        }

        fn start_session(
            &mut self,
            request: ProviderSessionRequest,
        ) -> Result<ProviderSession, CopilotTransportError> {
            self.frames.push(json!({
                "method": "session/new",
                "params": { "conversationId": request.conversation_id, "options": request.options },
            }));
            Ok(ProviderSession {
                session_id: "acp-session-1".into(),
            })
        }

        fn apply_options(
            &mut self,
            _session_id: &str,
            _options: BTreeMap<String, String>,
        ) -> Result<(), CopilotTransportError> {
            unreachable!("all current ACP options require a fresh session")
        }

        fn start_prompt(
            &mut self,
            envelope: CopilotPromptEnvelope,
        ) -> Result<String, CopilotTransportError> {
            self.frames.push(json!({
                "method": "session/prompt",
                "params": {
                    "sessionId": envelope.session_id,
                    "prompt": [{ "type": "text", "text": envelope.prompt }],
                },
            }));
            Ok("stream-1".into())
        }

        fn poll_stream(
            &mut self,
            _stream_id: &str,
        ) -> Result<TransportStreamEvent, CopilotTransportError> {
            Ok(self
                .chunks
                .pop_front()
                .map(|text| TransportStreamEvent::Chunk { text })
                .unwrap_or(TransportStreamEvent::Completed))
        }

        fn cancel_prompt(&mut self, _stream_id: &str) -> Result<(), CopilotTransportError> {
            self.frames.push(json!({ "method": "session/cancel" }));
            Ok(())
        }

        fn end_session(&mut self, session_id: &str) -> Result<(), CopilotTransportError> {
            self.frames
                .push(json!({ "method": "session/close", "params": { "sessionId": session_id } }));
            Ok(())
        }
    }

    /// A fake ACP transcript at the provider boundary. It demonstrates the
    /// important UX contract: navigation only records a local action, while
    /// one explicit send produces exactly one `session/prompt` equivalent.
    #[test]
    fn fake_acp_transcript_has_zero_prompts_until_one_explicit_send() {
        let transport = FakeAcpServerTranscript {
            frames: Vec::new(),
            chunks: ["first", " second"].into_iter().map(Into::into).collect(),
        };
        let mut adapter = CopilotAdapter::new(transport);
        assert_eq!(
            adapter
                .local_conversation_action(LocalConversationAction::Open)
                .provider_requests,
            0
        );
        assert_eq!(
            adapter
                .local_conversation_action(LocalConversationAction::Reopen)
                .provider_requests,
            0
        );
        assert_eq!(
            adapter
                .local_conversation_action(LocalConversationAction::Clear)
                .provider_requests,
            0
        );
        let capabilities = adapter.discover_capabilities().unwrap();
        adapter
            .start_session("conversation-1", capabilities.selected_options())
            .unwrap();
        adapter
            .start_prompt(ExplicitPrompt {
                prompt_id: "turn-1".into(),
                conversation_id: "conversation-1".into(),
                idempotency_key: "once-1".into(),
                text: "Explain this hunk".into(),
            })
            .unwrap();
        assert_eq!(
            adapter
                .transport()
                .frames
                .iter()
                .filter(|frame| frame["method"] == "session/prompt")
                .count(),
            1
        );
        assert!(
            matches!(adapter.poll_prompt().unwrap(), PromptStreamUpdate::Chunk { text, .. } if text == "first")
        );
        assert!(
            matches!(adapter.poll_prompt().unwrap(), PromptStreamUpdate::Chunk { text, .. } if text == " second")
        );
        assert!(matches!(
            adapter.poll_prompt().unwrap(),
            PromptStreamUpdate::Completed { .. }
        ));
        assert_eq!(
            adapter
                .transport()
                .frames
                .iter()
                .filter(|frame| frame["method"] == "session/prompt")
                .count(),
            1
        );
    }

    #[test]
    fn fake_acp_prompt_frame_contains_only_immutable_round_anchor_and_new_question() {
        let anchor = Anchor {
            repository_id: "repo-main".into(),
            workspace_relative_path: "src/lib.rs".into(),
            side: "head".into(),
            start_line: 12,
            end_line: 16,
            blob_sha: "blob-head-1".into(),
            selected_code: "fn parse() {}".into(),
        };
        let envelope = ask_prompt_envelope(
            &immutable_round(),
            "conversation-immutable-1",
            Some(&anchor),
            "Why is this parser branch needed?",
        )
        .unwrap();
        let mut adapter = CopilotAdapter::new(FakeAcpServerTranscript::default());
        let capabilities = adapter.discover_capabilities().unwrap();
        adapter
            .start_session("conversation-immutable-1", capabilities.selected_options())
            .unwrap();
        adapter
            .start_prompt(ExplicitPrompt {
                prompt_id: "turn-immutable-1".into(),
                conversation_id: "conversation-immutable-1".into(),
                idempotency_key: "ask-immutable-1".into(),
                text: envelope,
            })
            .unwrap();
        let frame = adapter
            .transport()
            .frames
            .iter()
            .find(|frame| frame["method"] == "session/prompt")
            .expect("one explicit ACP prompt");
        let sent: Value = serde_json::from_str(
            frame
                .pointer("/params/prompt/0/text")
                .and_then(Value::as_str)
                .expect("ACP text content"),
        )
        .unwrap();
        assert_eq!(sent["protocol"], "review_queue.ask.v1");
        assert_eq!(sent["round"]["id"], "round-immutable-1");
        assert_eq!(sent["round"]["manifestHash"], "manifest-hash-1");
        assert_eq!(sent["round"]["repositories"][0]["headSha"], "head-sha-1");
        assert_eq!(sent["anchor"]["workspaceRelativePath"], "src/lib.rs");
        assert_eq!(sent["anchor"]["startLine"], 12);
        assert_eq!(sent["anchor"]["blobSha"], "blob-head-1");
        assert_eq!(sent["anchor"]["selectedCode"], "fn parse() {}");
        assert_eq!(sent["userQuestion"], "Why is this parser branch needed?");
        assert!(sent.get("priorTurns").is_none());
        assert!(sent.get("transcript").is_none());
        assert!(sent.pointer("/round/workspaceRoot").is_none());
    }

    #[test]
    fn capabilities_are_derived_from_official_sdk_model_discovery() {
        let capabilities = AcpCliTransport::discovered_capabilities(vec![
            github_copilot_sdk::Model {
                id: "sdk-model-a".into(),
                name: "SDK model A".into(),
                supported_reasoning_efforts: vec!["low".into(), "high".into()],
                default_reasoning_effort: Some("high".into()),
                ..Default::default()
            },
            github_copilot_sdk::Model {
                id: "sdk-model-b".into(),
                name: "SDK model B".into(),
                supported_reasoning_efforts: vec!["medium".into()],
                ..Default::default()
            },
        ]);
        assert_eq!(
            capabilities
                .option_groups
                .iter()
                .map(|group| group.key.as_str())
                .collect::<Vec<_>>(),
            vec!["model", "reasoning_effort", "context_window"]
        );
        assert_eq!(
            capabilities.option_groups[0].choices[0].value,
            "sdk-model-a"
        );
        assert_eq!(
            capabilities.option_groups[1].selected.as_deref(),
            Some("high")
        );
        assert_eq!(
            capabilities.option_groups[2].selected.as_deref(),
            Some("managed_80")
        );
    }

    #[test]
    fn sdk_child_explicitly_strips_token_and_byok_environment_inputs() {
        let mut transport = AcpCliTransport::new(PathBuf::from("."));
        transport.auth = Some(SdkAuthentication::ExistingCli);
        let options = transport.client_options().unwrap();
        assert_eq!(options.use_logged_in_user, Some(true));
        assert!(options.github_token.is_none());
        let stripped = options
            .env_remove
            .iter()
            .map(|key| key.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        for key in STRIPPED_COPILOT_AUTH_ENV {
            assert!(
                stripped.iter().any(|removed| removed == key),
                "{key} must not be inherited by the ACP child"
            );
        }
        assert!(stripped.iter().any(|removed| removed == "GH_TOKEN"));
        assert!(
            stripped
                .iter()
                .any(|removed| removed == "COPILOT_PROVIDER_API_KEY")
        );
        assert!(
            stripped
                .iter()
                .any(|removed| removed == "GITHUB_COPILOT_API_TOKEN")
        );
    }

    #[test]
    fn app_owned_oauth_uses_only_the_sdk_child_token_channel() {
        let mut transport = AcpCliTransport::new(PathBuf::from("."));
        transport.auth = Some(SdkAuthentication::AppOwned {
            token: "test-token-not-a-credential".into(),
        });
        let options = transport.client_options().unwrap();
        assert_eq!(options.use_logged_in_user, Some(false));
        assert!(options.github_token.is_some());
        assert!(
            options
                .env_remove
                .iter()
                .all(|key| key != "COPILOT_SDK_AUTH_TOKEN")
        );
    }
}
