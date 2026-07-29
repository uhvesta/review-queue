//! Production-shaped, credential-free Copilot adapter boundary.
//!
//! The signed desktop process owns credentials and the concrete SDK/CLI
//! transport. This module deliberately models only public authentication
//! state, capability metadata, explicit prompts, and stream lifecycle.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fmt,
};

use serde::{Deserialize, Serialize};

/// Selects where the desktop-owned transport obtains authentication.
///
/// Neither variant carries a credential. In particular, CLI authentication is
/// validation-only: an implementation must not invoke a login flow or modify
/// the CLI credential store.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CopilotAuthSource {
    ExistingCliSignInReadOnly,
    AppOwnedOauth,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CopilotAuthState {
    Connected,
    NotConnected,
    Expired,
}

/// Public validation result. Account labels are display metadata, never
/// usernames/passwords, access tokens, device codes, or refresh tokens.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CopilotAuthValidation {
    pub source: CopilotAuthSource,
    pub state: CopilotAuthState,
    #[serde(default)]
    pub account_label: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OptionApplyPolicy {
    AppliesNow,
    RequiresFreshSession,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionOptionChoice {
    pub value: String,
    pub label: String,
}

/// A provider-discovered option group. Keys are intentionally open-ended so a
/// newer SDK can add groups without a desktop release.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionOptionGroup {
    pub key: String,
    pub label: String,
    pub supported: bool,
    #[serde(default)]
    pub unsupported_reason: Option<String>,
    pub apply_policy: OptionApplyPolicy,
    #[serde(default)]
    pub choices: Vec<SessionOptionChoice>,
    #[serde(default)]
    pub selected: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CopilotCapabilities {
    pub supported: bool,
    #[serde(default)]
    pub unsupported_reason: Option<String>,
    #[serde(default)]
    pub option_groups: Vec<SessionOptionGroup>,
}

impl CopilotCapabilities {
    fn validate(&self) -> Result<(), CopilotAdapterError> {
        if !self.supported && empty(self.unsupported_reason.as_deref().unwrap_or("")) {
            return Err(CopilotAdapterError::validation(
                "capabilities_unsupported_reason_required",
                "Copilot is unavailable without an explanation.",
                "No provider session was created.",
                "Refresh capabilities or select another provider.",
            ));
        }
        let mut keys = HashSet::new();
        for group in &self.option_groups {
            if empty(&group.key) || empty(&group.label) || !keys.insert(group.key.clone()) {
                return Err(CopilotAdapterError::validation(
                    "invalid_option_group",
                    "Provider option groups have a missing or duplicate key.",
                    "No provider session was created.",
                    "Refresh provider capabilities.",
                ));
            }
            if !group.supported && empty(group.unsupported_reason.as_deref().unwrap_or("")) {
                return Err(CopilotAdapterError::validation(
                    "option_unsupported_reason_required",
                    "An unsupported provider option has no explanation.",
                    "No provider session was created.",
                    "Refresh provider capabilities.",
                ));
            }
            if group.supported && group.choices.is_empty() {
                return Err(CopilotAdapterError::validation(
                    "option_choices_required",
                    "A supported provider option has no choices.",
                    "No provider session was created.",
                    "Refresh provider capabilities.",
                ));
            }
            let mut choices = HashSet::new();
            for choice in &group.choices {
                if empty(&choice.value)
                    || empty(&choice.label)
                    || !choices.insert(choice.value.clone())
                {
                    return Err(CopilotAdapterError::validation(
                        "invalid_option_choice",
                        "A provider option has a missing or duplicate choice.",
                        "No provider session was created.",
                        "Refresh provider capabilities.",
                    ));
                }
            }
            if let Some(selected) = &group.selected
                && !choices.contains(selected)
            {
                return Err(CopilotAdapterError::validation(
                    "unknown_selected_option",
                    "A provider selected an option it did not advertise.",
                    "No provider session was created.",
                    "Refresh provider capabilities.",
                ));
            }
        }
        Ok(())
    }

    pub fn selected_options(&self) -> BTreeMap<String, String> {
        self.option_groups
            .iter()
            .filter(|group| group.supported)
            .filter_map(|group| {
                group
                    .selected
                    .as_ref()
                    .map(|value| (group.key.clone(), value.clone()))
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CopilotFailureKind {
    Authentication,
    UnsupportedModel,
    Network,
    Validation,
    Conflict,
    Provider,
}

/// Actionable and intentionally credential-free provider failure.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CopilotAdapterError {
    pub code: String,
    pub kind: CopilotFailureKind,
    pub what_happened: String,
    pub data_safety: String,
    pub next_step: String,
    pub retryable: bool,
}

impl CopilotAdapterError {
    fn validation(code: &str, what: &str, safety: &str, next: &str) -> CopilotAdapterError {
        Self {
            code: code.into(),
            kind: CopilotFailureKind::Validation,
            what_happened: what.into(),
            data_safety: safety.into(),
            next_step: next.into(),
            retryable: false,
        }
    }

    fn conflict(code: &str, what: &str, next: &str) -> CopilotAdapterError {
        Self {
            code: code.into(),
            kind: CopilotFailureKind::Conflict,
            what_happened: what.into(),
            data_safety: "The existing session and transcript are preserved.".into(),
            next_step: next.into(),
            retryable: false,
        }
    }
}

impl fmt::Display for CopilotAdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} Next: {}",
            self.what_happened, self.data_safety, self.next_step
        )
    }
}

impl std::error::Error for CopilotAdapterError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopilotTransportError {
    AuthenticationUnavailable,
    ModelUnavailable,
    NetworkUnavailable,
    ProviderRejected,
}

impl From<CopilotTransportError> for CopilotAdapterError {
    fn from(value: CopilotTransportError) -> Self {
        match value {
            CopilotTransportError::AuthenticationUnavailable => Self {
                code: "copilot_authentication_unavailable".into(),
                kind: CopilotFailureKind::Authentication,
                what_happened: "Copilot authentication could not be validated.".into(),
                data_safety: "No prompt was sent and no credential was changed.".into(),
                next_step: "Connect the selected authentication source and retry.".into(),
                retryable: false,
            },
            CopilotTransportError::ModelUnavailable => Self {
                code: "copilot_model_unavailable".into(),
                kind: CopilotFailureKind::UnsupportedModel,
                what_happened: "The selected Copilot model is unavailable.".into(),
                data_safety: "No prompt was sent and the conversation is preserved.".into(),
                next_step: "Refresh capabilities and select an available model.".into(),
                retryable: false,
            },
            CopilotTransportError::NetworkUnavailable => Self {
                code: "copilot_network_unavailable".into(),
                kind: CopilotFailureKind::Network,
                what_happened: "Copilot could not be reached.".into(),
                data_safety: "The prompt remains local and was not replayed.".into(),
                next_step: "Check the network, then retry as a new explicit prompt.".into(),
                retryable: true,
            },
            CopilotTransportError::ProviderRejected => Self {
                code: "copilot_provider_rejected".into(),
                kind: CopilotFailureKind::Provider,
                what_happened: "Copilot rejected the request.".into(),
                data_safety: "The conversation is preserved and the prompt was not replayed."
                    .into(),
                next_step: "Review provider status and retry as a new explicit prompt.".into(),
                retryable: false,
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderSessionRequest {
    pub conversation_id: String,
    pub options: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderSession {
    pub session_id: String,
}

/// One user-authored prompt and its stable identities.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExplicitPrompt {
    pub prompt_id: String,
    pub conversation_id: String,
    pub idempotency_key: String,
    pub text: String,
}

impl fmt::Debug for ExplicitPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExplicitPrompt")
            .field("prompt_id", &self.prompt_id)
            .field("conversation_id", &self.conversation_id)
            .field("idempotency_key", &self.idempotency_key)
            .field(
                "text",
                &format_args!("<redacted:{} bytes>", self.text.len()),
            )
            .finish()
    }
}

/// Exact provider request. There is deliberately no transcript, history, or
/// messages collection: only the newly submitted explicit prompt is sent.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CopilotPromptEnvelope {
    pub session_id: String,
    pub prompt_id: String,
    pub conversation_id: String,
    pub idempotency_key: String,
    pub prompt: String,
    pub option_stamp: BTreeMap<String, String>,
}

impl fmt::Debug for CopilotPromptEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CopilotPromptEnvelope")
            .field("session_id", &self.session_id)
            .field("prompt_id", &self.prompt_id)
            .field("conversation_id", &self.conversation_id)
            .field("idempotency_key", &self.idempotency_key)
            .field(
                "prompt",
                &format_args!("<redacted:{} bytes>", self.prompt.len()),
            )
            .field("option_stamp", &self.option_stamp)
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PromptStarted {
    pub prompt_id: String,
    pub stream_id: String,
    pub option_stamp: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransportStreamEvent {
    Chunk { text: String },
    Completed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PromptStreamUpdate {
    Chunk {
        prompt_id: String,
        sequence: u64,
        text: String,
    },
    Completed {
        prompt_id: String,
    },
    Failed {
        prompt_id: String,
        error: CopilotAdapterError,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PromptCancelled {
    pub prompt_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OptionChangeEffect {
    AppliedToCurrentSession,
    RequiresFreshSession,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct OptionChangeResult {
    pub key: String,
    pub requested_value: String,
    pub effect: OptionChangeEffect,
    /// Exact options that remain active for the next prompt in this session.
    pub active_option_stamp: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalConversationAction {
    Open,
    Reopen,
    Clear,
    LoadHistory,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LocalConversationAcknowledgement {
    pub action: LocalConversationAction,
    pub provider_requests: u64,
}

/// SDK/CLI boundary implemented inside the desktop process.
pub trait CopilotTransport {
    /// Validate the existing CLI sign-in without creating, refreshing, or
    /// mutating credentials.
    fn validate_existing_cli_sign_in_read_only(
        &mut self,
    ) -> Result<CopilotAuthValidation, CopilotTransportError>;

    /// Validate credentials owned by the app's Keychain-backed OAuth flow.
    fn validate_app_owned_oauth(&mut self) -> Result<CopilotAuthValidation, CopilotTransportError>;

    fn discover_capabilities(&mut self) -> Result<CopilotCapabilities, CopilotTransportError>;
    fn start_session(
        &mut self,
        request: ProviderSessionRequest,
    ) -> Result<ProviderSession, CopilotTransportError>;
    fn apply_options(
        &mut self,
        session_id: &str,
        options: BTreeMap<String, String>,
    ) -> Result<(), CopilotTransportError>;
    fn start_prompt(
        &mut self,
        envelope: CopilotPromptEnvelope,
    ) -> Result<String, CopilotTransportError>;
    fn poll_stream(
        &mut self,
        stream_id: &str,
    ) -> Result<TransportStreamEvent, CopilotTransportError>;
    fn cancel_prompt(&mut self, stream_id: &str) -> Result<(), CopilotTransportError>;
    fn end_session(&mut self, session_id: &str) -> Result<(), CopilotTransportError>;
}

struct ActiveSession {
    session_id: String,
    conversation_id: String,
    options: BTreeMap<String, String>,
}

struct ActivePrompt {
    prompt_id: String,
    stream_id: String,
    next_sequence: u64,
}

/// Stateful coordinator that enforces the no-replay and one-active-stream
/// invariants before crossing the provider boundary.
pub struct CopilotAdapter<T: CopilotTransport> {
    transport: T,
    capabilities: Option<CopilotCapabilities>,
    session: Option<ActiveSession>,
    prompt: Option<ActivePrompt>,
    consumed_prompt_ids: HashSet<String>,
    consumed_idempotency_keys: HashSet<String>,
}

impl<T: CopilotTransport> CopilotAdapter<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            capabilities: None,
            session: None,
            prompt: None,
            consumed_prompt_ids: HashSet::new(),
            consumed_idempotency_keys: HashSet::new(),
        }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn validate_auth(
        &mut self,
        source: CopilotAuthSource,
    ) -> Result<CopilotAuthValidation, CopilotAdapterError> {
        let validation = match source {
            CopilotAuthSource::ExistingCliSignInReadOnly => {
                self.transport.validate_existing_cli_sign_in_read_only()
            }
            CopilotAuthSource::AppOwnedOauth => self.transport.validate_app_owned_oauth(),
        }
        .map_err(CopilotAdapterError::from)?;
        if validation.source != source {
            return Err(CopilotAdapterError::validation(
                "copilot_auth_source_mismatch",
                "The provider validated a different authentication source.",
                "No prompt was sent and no credential was changed.",
                "Reconnect the selected authentication source.",
            ));
        }
        if validation.state != CopilotAuthState::Connected {
            return Err(CopilotTransportError::AuthenticationUnavailable.into());
        }
        Ok(validation)
    }

    pub fn discover_capabilities(&mut self) -> Result<CopilotCapabilities, CopilotAdapterError> {
        let capabilities = self
            .transport
            .discover_capabilities()
            .map_err(CopilotAdapterError::from)?;
        capabilities.validate()?;
        self.capabilities = Some(capabilities.clone());
        Ok(capabilities)
    }

    pub fn start_session(
        &mut self,
        conversation_id: impl Into<String>,
        options: BTreeMap<String, String>,
    ) -> Result<ProviderSession, CopilotAdapterError> {
        if self.session.is_some() {
            return Err(CopilotAdapterError::conflict(
                "copilot_session_already_active",
                "A Copilot session is already active.",
                "End the active session before starting another.",
            ));
        }
        let conversation_id = conversation_id.into();
        require_identifier(&conversation_id, "conversation_id_required")?;
        self.validate_options(&options)?;
        let session = self
            .transport
            .start_session(ProviderSessionRequest {
                conversation_id: conversation_id.clone(),
                options: options.clone(),
            })
            .map_err(CopilotAdapterError::from)?;
        require_identifier(&session.session_id, "provider_session_id_required")?;
        self.session = Some(ActiveSession {
            session_id: session.session_id.clone(),
            conversation_id,
            options,
        });
        Ok(session)
    }

    pub fn change_option(
        &mut self,
        key: &str,
        value: &str,
    ) -> Result<OptionChangeResult, CopilotAdapterError> {
        let capabilities = self.capabilities.as_ref().ok_or_else(|| {
            CopilotAdapterError::conflict(
                "capabilities_not_discovered",
                "Copilot capabilities have not been discovered.",
                "Discover capabilities before changing options.",
            )
        })?;
        let group = capabilities
            .option_groups
            .iter()
            .find(|group| group.key == key)
            .ok_or_else(|| {
                CopilotAdapterError::validation(
                    "unknown_option_group",
                    "Copilot did not advertise that option.",
                    "The active session was not changed.",
                    "Refresh capabilities and choose an advertised option.",
                )
            })?;
        validate_group_choice(group, value)?;
        let session = self.session.as_mut().ok_or_else(|| {
            CopilotAdapterError::conflict(
                "copilot_session_required",
                "There is no active Copilot session.",
                "Start a session before changing its options.",
            )
        })?;
        let effect = match group.apply_policy {
            OptionApplyPolicy::AppliesNow => {
                let mut next = session.options.clone();
                next.insert(key.into(), value.into());
                self.transport
                    .apply_options(&session.session_id, next.clone())
                    .map_err(CopilotAdapterError::from)?;
                session.options = next;
                OptionChangeEffect::AppliedToCurrentSession
            }
            OptionApplyPolicy::RequiresFreshSession => OptionChangeEffect::RequiresFreshSession,
        };
        Ok(OptionChangeResult {
            key: key.into(),
            requested_value: value.into(),
            effect,
            active_option_stamp: session.options.clone(),
        })
    }

    pub fn start_prompt(
        &mut self,
        request: ExplicitPrompt,
    ) -> Result<PromptStarted, CopilotAdapterError> {
        if self.prompt.is_some() {
            return Err(CopilotAdapterError::conflict(
                "copilot_prompt_already_streaming",
                "A Copilot prompt is already streaming.",
                "Wait for it to finish or cancel it before sending another.",
            ));
        }
        for (value, code) in [
            (&request.prompt_id, "prompt_id_required"),
            (&request.conversation_id, "conversation_id_required"),
            (&request.idempotency_key, "idempotency_key_required"),
        ] {
            require_identifier(value, code)?;
        }
        if empty(&request.text) {
            return Err(CopilotAdapterError::validation(
                "prompt_text_required",
                "The Copilot prompt is empty.",
                "No provider request was made.",
                "Enter a prompt and send it explicitly.",
            ));
        }
        let session = self.session.as_ref().ok_or_else(|| {
            CopilotAdapterError::conflict(
                "copilot_session_required",
                "There is no active Copilot session.",
                "Start a session before sending a prompt.",
            )
        })?;
        if session.conversation_id != request.conversation_id {
            return Err(CopilotAdapterError::validation(
                "prompt_conversation_mismatch",
                "The prompt belongs to a different conversation.",
                "No provider request was made.",
                "Send from the active conversation.",
            ));
        }
        if self.consumed_prompt_ids.contains(&request.prompt_id)
            || self
                .consumed_idempotency_keys
                .contains(&request.idempotency_key)
        {
            return Err(CopilotAdapterError {
                code: "copilot_prompt_already_consumed".into(),
                kind: CopilotFailureKind::Conflict,
                what_happened: "That explicit prompt was already attempted.".into(),
                data_safety: "The prompt was not replayed.".into(),
                next_step: "Create a new explicit prompt with new identifiers.".into(),
                retryable: false,
            });
        }

        // Reserve both identities before crossing the transport boundary.
        // Even an ambiguous network failure can therefore never auto-replay.
        self.consumed_prompt_ids.insert(request.prompt_id.clone());
        self.consumed_idempotency_keys
            .insert(request.idempotency_key.clone());
        let option_stamp = session.options.clone();
        let envelope = CopilotPromptEnvelope {
            session_id: session.session_id.clone(),
            prompt_id: request.prompt_id.clone(),
            conversation_id: request.conversation_id,
            idempotency_key: request.idempotency_key,
            prompt: request.text,
            option_stamp: option_stamp.clone(),
        };
        let stream_id = self
            .transport
            .start_prompt(envelope)
            .map_err(CopilotAdapterError::from)?;
        require_identifier(&stream_id, "provider_stream_id_required")?;
        self.prompt = Some(ActivePrompt {
            prompt_id: request.prompt_id.clone(),
            stream_id: stream_id.clone(),
            next_sequence: 0,
        });
        Ok(PromptStarted {
            prompt_id: request.prompt_id,
            stream_id,
            option_stamp,
        })
    }

    pub fn poll_prompt(&mut self) -> Result<PromptStreamUpdate, CopilotAdapterError> {
        let active = self.prompt.as_mut().ok_or_else(|| {
            CopilotAdapterError::conflict(
                "copilot_prompt_not_streaming",
                "There is no active Copilot stream.",
                "Send an explicit prompt first.",
            )
        })?;
        match self.transport.poll_stream(&active.stream_id) {
            Ok(TransportStreamEvent::Chunk { text }) => {
                let sequence = active.next_sequence;
                active.next_sequence += 1;
                Ok(PromptStreamUpdate::Chunk {
                    prompt_id: active.prompt_id.clone(),
                    sequence,
                    text,
                })
            }
            Ok(TransportStreamEvent::Completed) => {
                let prompt_id = active.prompt_id.clone();
                self.prompt = None;
                Ok(PromptStreamUpdate::Completed { prompt_id })
            }
            Err(error) => {
                let prompt_id = active.prompt_id.clone();
                self.prompt = None;
                Ok(PromptStreamUpdate::Failed {
                    prompt_id,
                    error: error.into(),
                })
            }
        }
    }

    pub fn cancel_prompt(&mut self) -> Result<PromptCancelled, CopilotAdapterError> {
        let active = self.prompt.as_ref().ok_or_else(|| {
            CopilotAdapterError::conflict(
                "copilot_prompt_not_streaming",
                "There is no active Copilot stream.",
                "No cancellation is needed.",
            )
        })?;
        self.transport
            .cancel_prompt(&active.stream_id)
            .map_err(CopilotAdapterError::from)?;
        let prompt_id = active.prompt_id.clone();
        self.prompt = None;
        Ok(PromptCancelled { prompt_id })
    }

    pub fn end_session(&mut self) -> Result<(), CopilotAdapterError> {
        if self.prompt.is_some() {
            return Err(CopilotAdapterError::conflict(
                "copilot_prompt_still_streaming",
                "The Copilot prompt is still streaming.",
                "Cancel or finish the prompt before ending the session.",
            ));
        }
        let session = self.session.as_ref().ok_or_else(|| {
            CopilotAdapterError::conflict(
                "copilot_session_required",
                "There is no active Copilot session.",
                "No session shutdown is needed.",
            )
        })?;
        self.transport
            .end_session(&session.session_id)
            .map_err(CopilotAdapterError::from)?;
        self.session = None;
        Ok(())
    }

    /// Records desktop-only navigation or transcript lifecycle. These actions
    /// must never infer a provider prompt or touch provider session state.
    pub fn local_conversation_action(
        &mut self,
        action: LocalConversationAction,
    ) -> LocalConversationAcknowledgement {
        LocalConversationAcknowledgement {
            action,
            provider_requests: 0,
        }
    }

    fn validate_options(
        &self,
        options: &BTreeMap<String, String>,
    ) -> Result<(), CopilotAdapterError> {
        let capabilities = self.capabilities.as_ref().ok_or_else(|| {
            CopilotAdapterError::conflict(
                "capabilities_not_discovered",
                "Copilot capabilities have not been discovered.",
                "Discover capabilities before starting a session.",
            )
        })?;
        if !capabilities.supported {
            return Err(CopilotAdapterError {
                code: "copilot_capabilities_unsupported".into(),
                kind: CopilotFailureKind::Provider,
                what_happened: capabilities
                    .unsupported_reason
                    .clone()
                    .unwrap_or_else(|| "Copilot is unavailable.".into()),
                data_safety: "No provider session was created.".into(),
                next_step: "Select another provider or refresh capabilities.".into(),
                retryable: false,
            });
        }
        for (key, value) in options {
            let group = capabilities
                .option_groups
                .iter()
                .find(|group| &group.key == key)
                .ok_or_else(|| {
                    CopilotAdapterError::validation(
                        "unknown_option_group",
                        "The requested session option was not advertised.",
                        "No provider session was created.",
                        "Refresh capabilities and use advertised options.",
                    )
                })?;
            validate_group_choice(group, value)?;
        }
        Ok(())
    }
}

fn validate_group_choice(
    group: &SessionOptionGroup,
    value: &str,
) -> Result<(), CopilotAdapterError> {
    if !group.supported {
        return Err(CopilotAdapterError {
            code: "copilot_option_unsupported".into(),
            kind: CopilotFailureKind::Provider,
            what_happened: group
                .unsupported_reason
                .clone()
                .unwrap_or_else(|| "The selected option is unsupported.".into()),
            data_safety: "The active provider session was not changed.".into(),
            next_step: "Choose a supported option.".into(),
            retryable: false,
        });
    }
    if !group.choices.iter().any(|choice| choice.value == value) {
        return Err(CopilotAdapterError {
            code: if group.key == "model" {
                "copilot_model_unavailable"
            } else {
                "copilot_option_value_unavailable"
            }
            .into(),
            kind: if group.key == "model" {
                CopilotFailureKind::UnsupportedModel
            } else {
                CopilotFailureKind::Validation
            },
            what_happened: format!("The selected {} value is unavailable.", group.label),
            data_safety: "The active provider session was not changed.".into(),
            next_step: "Refresh capabilities and choose an advertised value.".into(),
            retryable: false,
        });
    }
    Ok(())
}

fn require_identifier(value: &str, code: &str) -> Result<(), CopilotAdapterError> {
    if empty(value) {
        Err(CopilotAdapterError::validation(
            code,
            "A required Copilot request identifier is missing.",
            "No provider request was made.",
            "Create a new explicit request with stable identifiers.",
        ))
    } else {
        Ok(())
    }
}

fn empty(value: &str) -> bool {
    value.trim().is_empty()
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct FakeRequestCounters {
    pub cli_auth_validations: u64,
    pub app_oauth_validations: u64,
    pub capability_discoveries: u64,
    pub session_starts: u64,
    pub option_updates: u64,
    pub prompt_starts: u64,
    pub stream_polls: u64,
    pub prompt_cancellations: u64,
    pub session_ends: u64,
    /// Must remain zero: the CLI auth path is read-only by contract.
    pub cli_credential_writes: u64,
}

impl FakeRequestCounters {
    pub fn provider_requests(self) -> u64 {
        self.cli_auth_validations
            + self.app_oauth_validations
            + self.capability_discoveries
            + self.session_starts
            + self.option_updates
            + self.prompt_starts
            + self.stream_polls
            + self.prompt_cancellations
            + self.session_ends
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FakeOperation {
    CliAuthValidation,
    AppOauthValidation,
    CapabilityDiscovery,
    SessionStart,
    OptionUpdate,
    PromptStart,
    StreamPoll,
    PromptCancellation,
    SessionEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeFailure {
    Authentication,
    Model,
    Network,
    Provider,
}

impl From<FakeFailure> for CopilotTransportError {
    fn from(value: FakeFailure) -> Self {
        match value {
            FakeFailure::Authentication => Self::AuthenticationUnavailable,
            FakeFailure::Model => Self::ModelUnavailable,
            FakeFailure::Network => Self::NetworkUnavailable,
            FakeFailure::Provider => Self::ProviderRejected,
        }
    }
}

struct FakeStream {
    chunks: VecDeque<String>,
}

/// Deterministic transport for contract tests. It contains no credential field
/// and its Debug output exposes only counters and public capability metadata.
pub struct FakeCopilotTransport {
    counters: FakeRequestCounters,
    capabilities: CopilotCapabilities,
    failures: HashMap<FakeOperation, VecDeque<FakeFailure>>,
    planned_streams: VecDeque<Vec<String>>,
    streams: HashMap<String, FakeStream>,
    last_prompt_envelope: Option<CopilotPromptEnvelope>,
    next_session: u64,
    next_stream: u64,
}

impl fmt::Debug for FakeCopilotTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeCopilotTransport")
            .field("counters", &self.counters)
            .field("capabilities", &self.capabilities)
            .field("planned_stream_count", &self.planned_streams.len())
            .field("active_stream_count", &self.streams.len())
            .finish()
    }
}

impl Default for FakeCopilotTransport {
    fn default() -> Self {
        Self::healthy()
    }
}

impl FakeCopilotTransport {
    pub fn healthy() -> Self {
        Self {
            counters: FakeRequestCounters::default(),
            capabilities: CopilotCapabilities {
                supported: true,
                unsupported_reason: None,
                option_groups: vec![
                    group(
                        "model",
                        "Model",
                        &["gpt-5.6", "gpt-5.6-fast"],
                        "gpt-5.6",
                        OptionApplyPolicy::RequiresFreshSession,
                    ),
                    group(
                        "thinking",
                        "Thinking",
                        &["low", "high"],
                        "high",
                        OptionApplyPolicy::AppliesNow,
                    ),
                    group(
                        "context",
                        "Context",
                        &["review", "workspace"],
                        "review",
                        OptionApplyPolicy::AppliesNow,
                    ),
                    group(
                        "future.vendor.option",
                        "Future option",
                        &["alpha", "beta"],
                        "alpha",
                        OptionApplyPolicy::RequiresFreshSession,
                    ),
                ],
            },
            failures: HashMap::new(),
            planned_streams: VecDeque::new(),
            streams: HashMap::new(),
            last_prompt_envelope: None,
            next_session: 1,
            next_stream: 1,
        }
    }

    pub fn counters(&self) -> FakeRequestCounters {
        self.counters
    }

    pub fn capabilities_mut(&mut self) -> &mut CopilotCapabilities {
        &mut self.capabilities
    }

    pub fn fail_next(&mut self, operation: FakeOperation, failure: FakeFailure) {
        self.failures
            .entry(operation)
            .or_default()
            .push_back(failure);
    }

    pub fn enqueue_stream<I, S>(&mut self, chunks: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.planned_streams
            .push_back(chunks.into_iter().map(Into::into).collect());
    }

    pub fn last_prompt_envelope(&self) -> Option<&CopilotPromptEnvelope> {
        self.last_prompt_envelope.as_ref()
    }

    fn failure(&mut self, operation: FakeOperation) -> Result<(), CopilotTransportError> {
        if let Some(failure) = self
            .failures
            .get_mut(&operation)
            .and_then(VecDeque::pop_front)
        {
            Err(failure.into())
        } else {
            Ok(())
        }
    }
}

impl CopilotTransport for FakeCopilotTransport {
    fn validate_existing_cli_sign_in_read_only(
        &mut self,
    ) -> Result<CopilotAuthValidation, CopilotTransportError> {
        self.counters.cli_auth_validations += 1;
        self.failure(FakeOperation::CliAuthValidation)?;
        Ok(CopilotAuthValidation {
            source: CopilotAuthSource::ExistingCliSignInReadOnly,
            state: CopilotAuthState::Connected,
            account_label: Some("existing CLI account".into()),
        })
    }

    fn validate_app_owned_oauth(&mut self) -> Result<CopilotAuthValidation, CopilotTransportError> {
        self.counters.app_oauth_validations += 1;
        self.failure(FakeOperation::AppOauthValidation)?;
        Ok(CopilotAuthValidation {
            source: CopilotAuthSource::AppOwnedOauth,
            state: CopilotAuthState::Connected,
            account_label: Some("app account".into()),
        })
    }

    fn discover_capabilities(&mut self) -> Result<CopilotCapabilities, CopilotTransportError> {
        self.counters.capability_discoveries += 1;
        self.failure(FakeOperation::CapabilityDiscovery)?;
        Ok(self.capabilities.clone())
    }

    fn start_session(
        &mut self,
        _request: ProviderSessionRequest,
    ) -> Result<ProviderSession, CopilotTransportError> {
        self.counters.session_starts += 1;
        self.failure(FakeOperation::SessionStart)?;
        let session_id = format!("fake-session-{}", self.next_session);
        self.next_session += 1;
        Ok(ProviderSession { session_id })
    }

    fn apply_options(
        &mut self,
        _session_id: &str,
        _options: BTreeMap<String, String>,
    ) -> Result<(), CopilotTransportError> {
        self.counters.option_updates += 1;
        self.failure(FakeOperation::OptionUpdate)
    }

    fn start_prompt(
        &mut self,
        envelope: CopilotPromptEnvelope,
    ) -> Result<String, CopilotTransportError> {
        self.counters.prompt_starts += 1;
        self.last_prompt_envelope = Some(envelope);
        self.failure(FakeOperation::PromptStart)?;
        let stream_id = format!("fake-stream-{}", self.next_stream);
        self.next_stream += 1;
        self.streams.insert(
            stream_id.clone(),
            FakeStream {
                chunks: self
                    .planned_streams
                    .pop_front()
                    .unwrap_or_else(|| vec!["deterministic response".into()])
                    .into(),
            },
        );
        Ok(stream_id)
    }

    fn poll_stream(
        &mut self,
        stream_id: &str,
    ) -> Result<TransportStreamEvent, CopilotTransportError> {
        self.counters.stream_polls += 1;
        self.failure(FakeOperation::StreamPoll)?;
        let stream = self
            .streams
            .get_mut(stream_id)
            .ok_or(CopilotTransportError::ProviderRejected)?;
        if let Some(text) = stream.chunks.pop_front() {
            Ok(TransportStreamEvent::Chunk { text })
        } else {
            self.streams.remove(stream_id);
            Ok(TransportStreamEvent::Completed)
        }
    }

    fn cancel_prompt(&mut self, stream_id: &str) -> Result<(), CopilotTransportError> {
        self.counters.prompt_cancellations += 1;
        self.failure(FakeOperation::PromptCancellation)?;
        self.streams.remove(stream_id);
        Ok(())
    }

    fn end_session(&mut self, _session_id: &str) -> Result<(), CopilotTransportError> {
        self.counters.session_ends += 1;
        self.failure(FakeOperation::SessionEnd)
    }
}

fn group(
    key: &str,
    label: &str,
    values: &[&str],
    selected: &str,
    apply_policy: OptionApplyPolicy,
) -> SessionOptionGroup {
    SessionOptionGroup {
        key: key.into(),
        label: label.into(),
        supported: true,
        unsupported_reason: None,
        apply_policy,
        choices: values
            .iter()
            .map(|value| SessionOptionChoice {
                value: (*value).into(),
                label: (*value).into(),
            })
            .collect(),
        selected: Some(selected.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected_adapter() -> CopilotAdapter<FakeCopilotTransport> {
        let mut adapter = CopilotAdapter::new(FakeCopilotTransport::healthy());
        adapter
            .validate_auth(CopilotAuthSource::ExistingCliSignInReadOnly)
            .unwrap();
        let capabilities = adapter.discover_capabilities().unwrap();
        adapter
            .start_session("conversation-1", capabilities.selected_options())
            .unwrap();
        adapter
    }

    fn prompt(id: &str, key: &str, text: &str) -> ExplicitPrompt {
        ExplicitPrompt {
            prompt_id: id.into(),
            conversation_id: "conversation-1".into(),
            idempotency_key: key.into(),
            text: text.into(),
        }
    }

    #[test]
    fn cli_auth_is_read_only_and_app_oauth_is_a_distinct_source() {
        let mut adapter = CopilotAdapter::new(FakeCopilotTransport::healthy());
        let cli = adapter
            .validate_auth(CopilotAuthSource::ExistingCliSignInReadOnly)
            .unwrap();
        let app = adapter
            .validate_auth(CopilotAuthSource::AppOwnedOauth)
            .unwrap();
        assert_eq!(cli.source, CopilotAuthSource::ExistingCliSignInReadOnly);
        assert_eq!(app.source, CopilotAuthSource::AppOwnedOauth);
        let counters = adapter.transport().counters();
        assert_eq!(counters.cli_auth_validations, 1);
        assert_eq!(counters.app_oauth_validations, 1);
        assert_eq!(counters.cli_credential_writes, 0);
    }

    #[test]
    fn discovery_accepts_known_and_future_option_groups_and_reasons() {
        let mut transport = FakeCopilotTransport::healthy();
        transport
            .capabilities_mut()
            .option_groups
            .push(SessionOptionGroup {
                key: "future.experimental.mode".into(),
                label: "Experimental mode".into(),
                supported: false,
                unsupported_reason: Some("Requires a newer provider account.".into()),
                apply_policy: OptionApplyPolicy::RequiresFreshSession,
                choices: vec![],
                selected: None,
            });
        let mut adapter = CopilotAdapter::new(transport);
        let discovered = adapter.discover_capabilities().unwrap();
        assert!(
            discovered
                .option_groups
                .iter()
                .any(|group| group.key == "model")
        );
        assert!(
            discovered
                .option_groups
                .iter()
                .any(|group| group.key == "thinking")
        );
        assert!(
            discovered
                .option_groups
                .iter()
                .any(|group| group.key == "context")
        );
        assert_eq!(
            discovered.option_groups.last().unwrap().unsupported_reason,
            Some("Requires a newer provider account.".into())
        );
    }

    #[test]
    fn explicit_prompt_is_exactly_once_and_envelope_has_no_history() {
        let mut adapter = connected_adapter();
        adapter.transport_mut().enqueue_stream(["first ", "second"]);
        let started = adapter
            .start_prompt(prompt("prompt-1", "idem-1", "Review this hunk"))
            .unwrap();
        assert_eq!(
            started.option_stamp.get("model").map(String::as_str),
            Some("gpt-5.6")
        );
        assert_eq!(
            adapter.poll_prompt().unwrap(),
            PromptStreamUpdate::Chunk {
                prompt_id: "prompt-1".into(),
                sequence: 0,
                text: "first ".into()
            }
        );
        assert!(matches!(
            adapter.poll_prompt().unwrap(),
            PromptStreamUpdate::Chunk { sequence: 1, .. }
        ));
        assert!(matches!(
            adapter.poll_prompt().unwrap(),
            PromptStreamUpdate::Completed { .. }
        ));

        let envelope = adapter.transport().last_prompt_envelope().unwrap();
        assert_eq!(envelope.prompt, "Review this hunk");
        let json = serde_json::to_value(envelope).unwrap();
        assert!(json.get("transcript").is_none());
        assert!(json.get("history").is_none());
        assert!(json.get("messages").is_none());
        assert_eq!(adapter.transport().counters().prompt_starts, 1);

        let error = adapter
            .start_prompt(prompt("prompt-1", "idem-new", "Replay"))
            .unwrap_err();
        assert_eq!(error.code, "copilot_prompt_already_consumed");
        assert_eq!(adapter.transport().counters().prompt_starts, 1);

        let error = adapter
            .start_prompt(prompt("prompt-new", "idem-1", "Replay"))
            .unwrap_err();
        assert_eq!(error.code, "copilot_prompt_already_consumed");
        assert_eq!(adapter.transport().counters().prompt_starts, 1);
    }

    #[test]
    fn ambiguous_start_failure_reserves_idempotency_key() {
        let mut adapter = connected_adapter();
        adapter
            .transport_mut()
            .fail_next(FakeOperation::PromptStart, FakeFailure::Network);
        let error = adapter
            .start_prompt(prompt("prompt-1", "idem-1", "Review"))
            .unwrap_err();
        assert_eq!(error.kind, CopilotFailureKind::Network);
        let again = adapter
            .start_prompt(prompt("prompt-2", "idem-1", "Review"))
            .unwrap_err();
        assert_eq!(again.code, "copilot_prompt_already_consumed");
        assert_eq!(adapter.transport().counters().prompt_starts, 1);
    }

    #[test]
    fn streams_can_cancel_and_fail_without_replay() {
        let mut adapter = connected_adapter();
        adapter
            .start_prompt(prompt("prompt-1", "idem-1", "Review"))
            .unwrap();
        let cancelled = adapter.cancel_prompt().unwrap();
        assert_eq!(cancelled.prompt_id, "prompt-1");
        assert_eq!(adapter.transport().counters().prompt_cancellations, 1);

        adapter
            .start_prompt(prompt("prompt-2", "idem-2", "Review again"))
            .unwrap();
        adapter
            .transport_mut()
            .fail_next(FakeOperation::StreamPoll, FakeFailure::Network);
        let failed = adapter.poll_prompt().unwrap();
        match failed {
            PromptStreamUpdate::Failed { error, .. } => {
                assert_eq!(error.kind, CopilotFailureKind::Network);
                assert!(error.retryable);
            }
            other => panic!("unexpected stream update: {other:?}"),
        }
    }

    #[test]
    fn per_turn_stamp_changes_only_when_policy_applies_now() {
        let mut adapter = connected_adapter();
        let immediate = adapter.change_option("thinking", "low").unwrap();
        assert_eq!(
            immediate.effect,
            OptionChangeEffect::AppliedToCurrentSession
        );
        assert_eq!(
            immediate
                .active_option_stamp
                .get("thinking")
                .map(String::as_str),
            Some("low")
        );
        let fresh = adapter.change_option("model", "gpt-5.6-fast").unwrap();
        assert_eq!(fresh.effect, OptionChangeEffect::RequiresFreshSession);
        assert_eq!(
            fresh.active_option_stamp.get("model").map(String::as_str),
            Some("gpt-5.6")
        );
        let started = adapter
            .start_prompt(prompt("prompt-1", "idem-1", "Review"))
            .unwrap();
        assert_eq!(
            started.option_stamp.get("thinking").map(String::as_str),
            Some("low")
        );
        assert_eq!(
            started.option_stamp.get("model").map(String::as_str),
            Some("gpt-5.6")
        );
        assert_eq!(adapter.transport().counters().option_updates, 1);
    }

    #[test]
    fn local_open_reopen_clear_and_history_never_request_provider() {
        let mut adapter = CopilotAdapter::new(FakeCopilotTransport::healthy());
        let before = adapter.transport().counters().provider_requests();
        for action in [
            LocalConversationAction::Open,
            LocalConversationAction::Reopen,
            LocalConversationAction::Clear,
            LocalConversationAction::LoadHistory,
        ] {
            let acknowledgement = adapter.local_conversation_action(action);
            assert_eq!(acknowledgement.provider_requests, 0);
        }
        assert_eq!(adapter.transport().counters().provider_requests(), before);
    }

    #[test]
    fn ending_session_is_explicit_and_refuses_active_stream() {
        let mut adapter = connected_adapter();
        adapter
            .start_prompt(prompt("prompt-1", "idem-1", "Review"))
            .unwrap();
        let error = adapter.end_session().unwrap_err();
        assert_eq!(error.code, "copilot_prompt_still_streaming");
        adapter.cancel_prompt().unwrap();
        adapter.end_session().unwrap();
        assert_eq!(adapter.transport().counters().session_ends, 1);
    }

    #[test]
    fn fake_auth_model_and_network_failures_are_actionable() {
        let cases = [
            (
                FakeOperation::CliAuthValidation,
                FakeFailure::Authentication,
                CopilotFailureKind::Authentication,
                "copilot_authentication_unavailable",
            ),
            (
                FakeOperation::SessionStart,
                FakeFailure::Model,
                CopilotFailureKind::UnsupportedModel,
                "copilot_model_unavailable",
            ),
            (
                FakeOperation::CapabilityDiscovery,
                FakeFailure::Network,
                CopilotFailureKind::Network,
                "copilot_network_unavailable",
            ),
        ];
        for (operation, failure, expected_kind, expected_code) in cases {
            let mut transport = FakeCopilotTransport::healthy();
            transport.fail_next(operation, failure);
            let mut adapter = CopilotAdapter::new(transport);
            let error = match operation {
                FakeOperation::CliAuthValidation => adapter
                    .validate_auth(CopilotAuthSource::ExistingCliSignInReadOnly)
                    .unwrap_err(),
                FakeOperation::CapabilityDiscovery => adapter.discover_capabilities().unwrap_err(),
                FakeOperation::SessionStart => {
                    let capabilities = adapter.discover_capabilities().unwrap();
                    adapter
                        .start_session("conversation-1", capabilities.selected_options())
                        .unwrap_err()
                }
                _ => unreachable!(),
            };
            assert_eq!(error.kind, expected_kind);
            assert_eq!(error.code, expected_code);
            assert!(!error.what_happened.is_empty());
            assert!(!error.data_safety.is_empty());
            assert!(!error.next_step.is_empty());
        }
    }

    #[test]
    fn prompt_debug_redacts_text_and_transport_debug_has_no_prompt() {
        let request = prompt("prompt-1", "idem-1", "super secret user text");
        let debug = format!("{request:?}");
        assert!(!debug.contains("super secret user text"));

        let mut adapter = connected_adapter();
        adapter.start_prompt(request).unwrap();
        let debug = format!("{:?}", adapter.transport());
        assert!(!debug.contains("super secret user text"));
        assert!(!debug.to_ascii_lowercase().contains("token"));
    }
}
