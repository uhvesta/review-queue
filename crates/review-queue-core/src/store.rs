use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AgentRoute, Anchor, Collection, Decision, DeliveryPayload, DomainError, DurableDelivery,
    FormalComment, Lifecycle, MachineRecord, ReviewBrief, Round, Submission, WorkspaceManifest,
    adapters::{
        ApprovalDisposition, AskConversation, AskTurn, AskTurnState, ConversationSessionState,
        DiscoveredSessionOption, SourceAdapterContract, SourceCapability,
    },
    capture::{CaptureRequest, Preflight, prepare_capture},
    github::{
        GithubMaterializedFile, GithubPublishAttempt, GithubPublishStatus, GithubQueuePayload,
        GithubReplyAttempt, GithubReplyRequest, GithubRoundState,
    },
    local_topic_identity,
    machine::MachineSnapshot,
    machine::{
        DEFAULT_REMOTE_SOCKET, MachineConfig, MachineEndpoint,
        MachineRecord as ConnectedMachineRecord, MachineSourceType, SshAdapter,
    },
    manifest_hash,
};

pub struct Store {
    conn: Connection,
}

#[derive(Clone, Debug)]
pub enum SubmissionResult {
    Existing(Round),
    Created(Round),
    Superseded { old_id: String, round: Round },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleEventKind {
    RequestChanges,
    ApproveLocal,
    ApproveRemote,
    Complete,
    Requeue,
    Purge,
}

impl LifecycleEventKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::RequestChanges => "request_changes",
            Self::ApproveLocal => "approve_local",
            Self::ApproveRemote => "approve_remote",
            Self::Complete => "complete",
            Self::Requeue => "requeue",
            Self::Purge => "purge",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LifecycleEvent {
    pub id: String,
    pub round_id: String,
    pub kind: LifecycleEventKind,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeliveryHistoryEntry {
    pub delivery: DurableDelivery,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    /// Stable, token-free acknowledgement vocabulary. Manual submission and
    /// legacy-invalid cleanup never change the immutable delivery payload.
    pub outcome: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RedactedArtifactExport {
    pub schema_version: u32,
    pub verified_at: DateTime<Utc>,
    pub table_row_counts: BTreeMap<String, u64>,
    /// Raw database/config/protocol values are deliberately excluded.
    pub contains_raw_values: bool,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> anyhow::Result<()> {
        self.conn.execute_batch("\
            CREATE TABLE IF NOT EXISTS rounds (
              id TEXT PRIMARY KEY, collection TEXT NOT NULL, topic_identity TEXT NOT NULL,
              manifest_hash TEXT NOT NULL, brief_json TEXT NOT NULL, manifest_json TEXT NOT NULL,
              rank INTEGER NOT NULL, lifecycle TEXT NOT NULL, superseded_by TEXT,
              created_at TEXT NOT NULL, origin_route_id TEXT
            );
            CREATE INDEX IF NOT EXISTS rounds_topic ON rounds(collection, topic_identity, created_at);
            CREATE TABLE IF NOT EXISTS routes (
              id TEXT PRIMARY KEY, adapter_kind TEXT NOT NULL, agent_id TEXT NOT NULL,
              endpoint TEXT, session_id TEXT, status TEXT NOT NULL, last_heartbeat TEXT NOT NULL,
              provenance_json TEXT
            );
            CREATE TABLE IF NOT EXISTS comments (
              id TEXT PRIMARY KEY, round_id TEXT NOT NULL REFERENCES rounds(id) ON DELETE CASCADE,
              thread_id TEXT NOT NULL, body TEXT NOT NULL, anchor_json TEXT, revision INTEGER NOT NULL,
              delivered_revision INTEGER, created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS decisions (
              round_id TEXT PRIMARY KEY REFERENCES rounds(id) ON DELETE CASCADE,
              decision TEXT NOT NULL, created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS deliveries (
              id TEXT PRIMARY KEY, round_id TEXT NOT NULL REFERENCES rounds(id) ON DELETE CASCADE,
              idempotency_key TEXT NOT NULL UNIQUE, payload_json TEXT NOT NULL, created_at TEXT NOT NULL,
              delivered_at TEXT, outcome TEXT
            );
            CREATE TABLE IF NOT EXISTS lifecycle_events (
              id TEXT PRIMARY KEY, round_id TEXT NOT NULL, kind TEXT NOT NULL,
              created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS lifecycle_events_round
              ON lifecycle_events(round_id, created_at, id);
            CREATE TABLE IF NOT EXISTS conversations (
              id TEXT PRIMARY KEY, round_id TEXT NOT NULL REFERENCES rounds(id) ON DELETE CASCADE,
              active INTEGER NOT NULL, history_only INTEGER NOT NULL, options_json TEXT NOT NULL,
              created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS ask_turns (
              id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
              idempotency_key TEXT NOT NULL UNIQUE, prompt TEXT NOT NULL, anchor_json TEXT,
              option_values_json TEXT NOT NULL, state TEXT NOT NULL, created_at TEXT NOT NULL,
              completed_at TEXT, failure_reason TEXT, response_text TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS ask_turn_chunks (
              turn_id TEXT NOT NULL REFERENCES ask_turns(id) ON DELETE CASCADE,
              sequence INTEGER NOT NULL, text TEXT NOT NULL, created_at TEXT NOT NULL,
              PRIMARY KEY(turn_id, sequence)
            );
            CREATE TABLE IF NOT EXISTS machines (
              id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, endpoint TEXT NOT NULL UNIQUE,
              created_at TEXT NOT NULL, config_json TEXT
            );
            CREATE TABLE IF NOT EXISTS file_views (
              round_id TEXT NOT NULL REFERENCES rounds(id) ON DELETE CASCADE,
              repository_id TEXT NOT NULL, path TEXT NOT NULL, viewed INTEGER NOT NULL,
              updated_at TEXT NOT NULL,
              PRIMARY KEY(round_id, repository_id, path)
            );
            CREATE TABLE IF NOT EXISTS github_round_state (
              round_id TEXT PRIMARY KEY REFERENCES rounds(id) ON DELETE CASCADE,
              payload_json TEXT NOT NULL, files_json TEXT NOT NULL DEFAULT '[]',
              imported_comments_json TEXT NOT NULL DEFAULT '[]',
              staleness_json TEXT, updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS github_publish_attempts (
              id TEXT PRIMARY KEY, round_id TEXT NOT NULL UNIQUE
                REFERENCES rounds(id) ON DELETE CASCADE,
              idempotency_key TEXT NOT NULL UNIQUE, preview_json TEXT NOT NULL,
              request_json TEXT NOT NULL, status TEXT NOT NULL, review_id TEXT,
              created_at TEXT NOT NULL, completed_at TEXT
            );
            CREATE TABLE IF NOT EXISTS github_reply_attempts (
              id TEXT PRIMARY KEY, round_id TEXT NOT NULL REFERENCES rounds(id) ON DELETE CASCADE,
              formal_comment_id TEXT NOT NULL, formal_revision INTEGER NOT NULL,
              idempotency_key TEXT NOT NULL UNIQUE, request_json TEXT NOT NULL,
              status TEXT NOT NULL, comment_id TEXT, created_at TEXT NOT NULL, completed_at TEXT,
              UNIQUE(round_id,formal_comment_id,formal_revision)
            );
            CREATE TABLE IF NOT EXISTS machine_round_snapshots (
              round_id TEXT PRIMARY KEY REFERENCES rounds(id) ON DELETE CASCADE,
              snapshot_json TEXT NOT NULL, created_at TEXT NOT NULL
            );
        ")?;
        // Databases created before deliveries tracked completion remain valid.
        if !table_has_column(&self.conn, "deliveries", "delivered_at")? {
            self.conn
                .execute("ALTER TABLE deliveries ADD COLUMN delivered_at TEXT", [])?;
        }
        if !table_has_column(&self.conn, "deliveries", "outcome")? {
            self.conn
                .execute("ALTER TABLE deliveries ADD COLUMN outcome TEXT", [])?;
        }
        if !table_has_column(&self.conn, "rounds", "origin_route_id")? {
            self.conn
                .execute("ALTER TABLE rounds ADD COLUMN origin_route_id TEXT", [])?;
        }
        if !table_has_column(&self.conn, "routes", "provenance_json")? {
            self.conn
                .execute("ALTER TABLE routes ADD COLUMN provenance_json TEXT", [])?;
        }
        if !table_has_column(&self.conn, "rounds", "source_metadata_json")? {
            self.conn.execute(
                "ALTER TABLE rounds ADD COLUMN source_metadata_json TEXT",
                [],
            )?;
        }
        if !table_has_column(&self.conn, "rounds", "origin_route_json")? {
            self.conn
                .execute("ALTER TABLE rounds ADD COLUMN origin_route_json TEXT", [])?;
        }
        if !table_has_column(&self.conn, "rounds", "source_adapter_json")? {
            self.conn
                .execute("ALTER TABLE rounds ADD COLUMN source_adapter_json TEXT", [])?;
        }
        if !table_has_column(&self.conn, "machines", "config_json")? {
            self.conn
                .execute("ALTER TABLE machines ADD COLUMN config_json TEXT", [])?;
        }
        for (column, definition) in [
            ("session_state", "TEXT NOT NULL DEFAULT 'can_continue'"),
            ("history_only_reason", "TEXT"),
            ("provider_session_label", "TEXT"),
            ("archived_at", "TEXT"),
        ] {
            if !table_has_column(&self.conn, "conversations", column)? {
                self.conn.execute(
                    &format!("ALTER TABLE conversations ADD COLUMN {column} {definition}"),
                    [],
                )?;
            }
        }
        self.conn.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS one_active_conversation_per_round ON conversations(round_id) WHERE active = 1;")?;
        // A provider stream cannot safely resume after process death. Keep the
        // transcript and make the recovery state explicit; never replay it.
        self.conn.execute(
            "UPDATE ask_turns SET state='interrupted', failure_reason=COALESCE(failure_reason, 'The app restarted before this response finished.'), completed_at=?1 WHERE state IN ('queued','streaming')",
            params![Utc::now().to_rfc3339()],
        )?;
        // A started SDK child is process-local and is deliberately never
        // resumed. Untouched empty conversations have no provider process to
        // lose and remain safe to start after restart.
        self.conn.execute(
            "UPDATE conversations SET session_state='history_only', history_only_reason=COALESCE(history_only_reason, 'The app restarted; the Copilot provider session was not resumed.'), provider_session_label=NULL WHERE session_state='can_continue' AND provider_session_label IS NOT NULL",
            [],
        )?;
        Ok(())
    }

    /// Returns the sole active chat, creating it only once for a mutable round.
    pub fn active_conversation(
        &mut self,
        round_id: &str,
        options: Vec<DiscoveredSessionOption>,
    ) -> Result<AskConversation, DomainError> {
        validate_ingress(&options)?;
        let round = self.round(round_id)?;
        ensure_mutable(&round)?;
        if let Some(chat) = self.active_conversation_for_round(round_id)? {
            return Ok(chat);
        }
        for option in &options {
            option.validate()?;
        }
        let chat = AskConversation {
            id: Uuid::new_v4().to_string(),
            round_id: round_id.to_owned(),
            session_state: ConversationSessionState::CanContinue,
            history_only_reason: None,
            provider_session_label: None,
            options,
            created_at: Utc::now(),
            archived_at: None,
        };
        self.insert_conversation(&chat, true)?;
        Ok(chat)
    }

    pub fn conversation_history(
        &self,
        round_id: &str,
    ) -> Result<Vec<AskConversation>, DomainError> {
        self.round(round_id)?;
        let mut statement = self.conn.prepare("SELECT id,round_id,session_state,history_only_reason,provider_session_label,options_json,created_at,archived_at FROM conversations WHERE round_id=?1 ORDER BY created_at,id").map_err(db_error)?;
        statement
            .query_map(params![round_id], conversation_from_row)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)
    }

    /// Reads the active transcript without creating a conversation or
    /// checking whether the round is mutable. This is safe for historical
    /// and superseded rounds and never contacts a provider.
    pub fn current_conversation(
        &self,
        round_id: &str,
    ) -> Result<Option<AskConversation>, DomainError> {
        self.round(round_id)?;
        self.active_conversation_for_round(round_id)
    }

    /// Marks the exact point at which a process-local provider session exists.
    /// Startup recovery uses this marker to distinguish a lost SDK session
    /// from an untouched conversation that is still safe to start.
    pub fn mark_conversation_provider_started(
        &self,
        conversation_id: &str,
        provider_session_label: &str,
        selected_options: &[DiscoveredSessionOption],
    ) -> Result<AskConversation, DomainError> {
        validate_ingress(&(conversation_id, provider_session_label, selected_options))?;
        if provider_session_label.trim().is_empty() {
            return Err(DomainError::actionable(
                "A started provider session needs a non-empty label.",
                "The conversation remains available and no prompt was sent.",
                "Start the provider session again and persist its public label.",
                "provider_session_label_required",
            ));
        }
        for option in selected_options {
            option.validate()?;
        }
        let conversation = self.conversation(conversation_id)?;
        ensure_mutable(&self.round(&conversation.round_id)?)?;
        conversation.validate_prompt_allowed()?;
        self.conn
            .execute(
                "UPDATE conversations
                 SET provider_session_label=?1, options_json=?2
                 WHERE id=?3 AND active=1",
                params![
                    provider_session_label,
                    json(&selected_options)?,
                    conversation_id
                ],
            )
            .map_err(db_error)?;
        self.conversation(conversation_id)
    }

    /// Persists the user's selected provider option on the active
    /// conversation. This is used both for options applied immediately and
    /// for selections that will take effect after an explicit Clear chat.
    pub fn update_conversation_option_selection(
        &self,
        conversation_id: &str,
        key: &str,
        value: &str,
    ) -> Result<AskConversation, DomainError> {
        validate_ingress(&(conversation_id, key, value))?;
        let conversation = self.conversation(conversation_id)?;
        ensure_mutable(&self.round(&conversation.round_id)?)?;
        conversation.validate_prompt_allowed()?;
        let mut options = conversation.options.clone();
        let option = options
            .iter_mut()
            .find(|option| option.key == key)
            .ok_or_else(|| {
                DomainError::actionable(
                    "The provider did not advertise that conversation option.",
                    "The saved conversation options were not changed.",
                    "Refresh Copilot capabilities and choose an advertised option.",
                    "unknown_conversation_option",
                )
            })?;
        if !option.supported {
            return Err(DomainError::actionable(
                "That provider option is currently unavailable.",
                "The saved conversation options were not changed.",
                "Choose a supported option or refresh Copilot capabilities.",
                "conversation_option_unavailable",
            ));
        }
        option.selected = Some(value.to_owned());
        option.validate()?;
        let changed = self
            .conn
            .execute(
                "UPDATE conversations SET options_json=?1 WHERE id=?2 AND active=1",
                params![json(&options)?, conversation_id],
            )
            .map_err(db_error)?;
        if changed != 1 {
            return Err(DomainError::actionable(
                "The active conversation changed before its option could be saved.",
                "The provider option was not recorded in review history.",
                "Reopen the current chat and choose the option again.",
                "conversation_option_save_conflict",
            ));
        }
        self.conversation(conversation_id)
    }

    /// Archived chats are deliberately separate from the current active chat.
    pub fn previous_conversations(
        &self,
        round_id: &str,
    ) -> Result<Vec<AskConversation>, DomainError> {
        self.round(round_id)?;
        let mut statement = self.conn.prepare("SELECT id,round_id,session_state,history_only_reason,provider_session_label,options_json,created_at,archived_at FROM conversations WHERE round_id=?1 AND active=0 ORDER BY created_at,id").map_err(db_error)?;
        statement
            .query_map(params![round_id], conversation_from_row)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)
    }

    pub fn clear_conversation(&mut self, round_id: &str) -> Result<AskConversation, DomainError> {
        let round = self.round(round_id)?;
        ensure_mutable(&round)?;
        let tx = self.conn.transaction().map_err(db_error)?;
        let old: Option<(String, String)> = tx
            .query_row(
                "SELECT id,options_json FROM conversations WHERE round_id=?1 AND active=1",
                params![round_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_error)?;
        let options = old
            .as_ref()
            .map(|(_, json)| parse::<Vec<DiscoveredSessionOption>>(json.clone()))
            .transpose()?
            .unwrap_or_default();
        let now = Utc::now();
        tx.execute("UPDATE conversations SET active=0,session_state='history_only',history_only_reason='Cleared for a fresh chat.',archived_at=?1 WHERE round_id=?2 AND active=1", params![now.to_rfc3339(),round_id]).map_err(db_error)?;
        let chat = AskConversation {
            id: Uuid::new_v4().to_string(),
            round_id: round_id.to_owned(),
            session_state: ConversationSessionState::CanContinue,
            history_only_reason: None,
            provider_session_label: None,
            options,
            created_at: now,
            archived_at: None,
        };
        insert_conversation_tx(&tx, &chat, true)?;
        tx.commit().map_err(db_error)?;
        Ok(chat)
    }

    /// Durable queue boundary: call this before starting any provider I/O.
    pub fn queue_ask_turn(&mut self, turn: AskTurn) -> Result<AskTurn, DomainError> {
        validate_ingress(&turn)?;
        let round_id: String = self
            .conn
            .query_row(
                "SELECT round_id FROM conversations WHERE id=?1",
                params![turn.conversation_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| ask_not_found("conversation_not_found"))?;
        ensure_mutable(&self.round(&round_id)?)?;
        let conversation = self.conversation(&turn.conversation_id)?;
        turn.validate_for_conversation(&conversation)?;
        if turn.state != AskTurnState::Queued {
            return Err(DomainError::actionable(
                "A new prompt must be queued before provider work begins.",
                "No prompt was sent.",
                "Queue the prompt, then begin streaming.",
                "ask_turn_not_queued",
            ));
        }
        if let Some(existing) = self.turn_by_idempotency(&turn.idempotency_key)? {
            return Ok(existing);
        }
        self.conn.execute("INSERT INTO ask_turns(id,conversation_id,idempotency_key,prompt,anchor_json,option_values_json,state,created_at,completed_at,failure_reason,response_text) VALUES(?1,?2,?3,?4,?5,?6,'queued',?7,NULL,NULL,'')", params![turn.id,turn.conversation_id,turn.idempotency_key,turn.prompt,turn.anchor.as_ref().map(json).transpose()?,json(&turn.option_values)?,turn.created_at.to_rfc3339()]).map_err(db_error)?;
        Ok(turn)
    }

    pub fn ask_turns(&self, conversation_id: &str) -> Result<Vec<AskTurn>, DomainError> {
        self.conversation(conversation_id)?;
        let mut st=self.conn.prepare("SELECT id,conversation_id,idempotency_key,prompt,anchor_json,option_values_json,state,created_at,completed_at,failure_reason,response_text FROM ask_turns WHERE conversation_id=?1 ORDER BY created_at,id").map_err(db_error)?;
        st.query_map(params![conversation_id], ask_turn_from_row)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)
    }

    pub fn begin_ask_turn(&self, id: &str) -> Result<AskTurn, DomainError> {
        self.transition_ask_turn(id, AskTurnState::Streaming, None)
    }
    pub fn complete_ask_turn(&self, id: &str) -> Result<AskTurn, DomainError> {
        self.transition_ask_turn(id, AskTurnState::Completed, None)
    }
    pub fn cancel_ask_turn(&self, id: &str) -> Result<AskTurn, DomainError> {
        self.transition_ask_turn(id, AskTurnState::Cancelled, None)
    }
    pub fn fail_ask_turn(&self, id: &str, reason: &str) -> Result<AskTurn, DomainError> {
        validate_ingress(&reason)?;
        self.transition_ask_turn(id, AskTurnState::Failed, Some(reason))
    }
    pub fn interrupt_ask_turn(&self, id: &str, reason: &str) -> Result<AskTurn, DomainError> {
        validate_ingress(&reason)?;
        self.transition_ask_turn(id, AskTurnState::Interrupted, Some(reason))
    }
    pub fn append_ask_chunk(&self, id: &str, text: &str) -> Result<AskTurn, DomainError> {
        validate_ingress(&text)?;
        if text.is_empty() {
            return self.ask_turn(id);
        }
        let turn = self.ask_turn(id)?;
        if turn.state != AskTurnState::Streaming {
            return Err(DomainError::actionable(
                "This response is not currently streaming.",
                "The saved transcript is unchanged.",
                "Start the queued prompt before writing response chunks.",
                "ask_turn_not_streaming",
            ));
        }
        let seq: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(sequence),-1)+1 FROM ask_turn_chunks WHERE turn_id=?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        self.conn
            .execute(
                "INSERT INTO ask_turn_chunks(turn_id,sequence,text,created_at) VALUES(?1,?2,?3,?4)",
                params![id, seq, text, Utc::now().to_rfc3339()],
            )
            .map_err(db_error)?;
        self.conn
            .execute(
                "UPDATE ask_turns SET response_text=response_text || ?1 WHERE id=?2",
                params![text, id],
            )
            .map_err(db_error)?;
        self.ask_turn(id)
    }

    pub fn submit(&mut self, submission: Submission) -> Result<SubmissionResult, DomainError> {
        let tx = self.conn.transaction().map_err(db_error)?;
        let result = submit_in_transaction(&tx, submission)?;
        tx.commit().map_err(db_error)?;
        Ok(result)
    }

    /// Runs local preflight after binding an explicitly selected route, or
    /// the sole registered route whose saved working directory is inside the
    /// submitted workspace. The route ID is part of capture's fingerprint, so
    /// the later mutation cannot silently attach a different route.
    pub fn preflight_local_capture(
        &self,
        request: &CaptureRequest,
    ) -> Result<Preflight, DomainError> {
        let (request, _) = self.bind_local_capture_route(request)?;
        crate::capture::preflight(&request)
    }

    /// The single local-ingestion path used by the desktop UI and local
    /// socket. Git refs stay guarded until SQLite persistence and real-index
    /// finalization have both succeeded.
    pub fn ingest_local_capture(
        &mut self,
        request: &CaptureRequest,
    ) -> Result<SubmissionResult, DomainError> {
        self.ingest_local_capture_with_ack(request, |_| Ok(()))
    }

    /// Socket ingestion uses `acknowledge` as the transport half of a small
    /// two-phase handshake. A disconnect before acknowledgement rolls back
    /// SQLite, refs, and exact index bytes. The final success response is sent
    /// only after this method commits.
    pub fn ingest_local_capture_with_ack<F>(
        &mut self,
        request: &CaptureRequest,
        acknowledge: F,
    ) -> Result<SubmissionResult, DomainError>
    where
        F: FnOnce(&SubmissionResult) -> Result<(), DomainError>,
    {
        let (request, origin_route) = self.bind_local_capture_route(request)?;
        if let Some(route) = origin_route.as_ref() {
            // Route rows may originate in an older database. Re-validate the
            // complete snapshot at the capture boundary before Git is touched.
            validate_ingress(route)?;
        }
        if request.participating_repository_ids.is_empty() {
            return Err(DomainError::actionable(
                "At least one repository must participate in capture.",
                "No source files, Git refs, staging state, or review data were changed.",
                "Run preflight, select one or more repositories, and submit again.",
                "repository_selection_required",
            ));
        }
        if request
            .preflight_token
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
        {
            return Err(DomainError::actionable(
                "Local capture requires a fresh repository preflight.",
                "No source files, Git refs, staging state, or review data were changed.",
                "Run preflight for the exact form and repository selection, then submit again.",
                "preflight_required",
            ));
        }

        let mut pending = prepare_capture(&request)?;
        let submission = Submission {
            collection: Collection::Local,
            topic_identity: local_topic_identity(pending.manifest()),
            brief: request.brief.clone(),
            manifest: pending.manifest().clone(),
            origin_route,
            source_metadata: None,
            source_adapter: None,
        };
        let tx = match self.conn.transaction() {
            Ok(tx) => tx,
            Err(problem) => return Err(pending.abort(db_error(problem))),
        };
        let result = match submit_in_transaction(&tx, submission) {
            Ok(result) => result,
            Err(problem) => return Err(pending.abort(problem)),
        };
        // If this fails, PendingCapture restores every ref and exact original
        // index while dropping `tx` rolls SQLite back.
        pending.finalize_indexes()?;
        if let Err(problem) = acknowledge(&result) {
            return Err(pending.abort(problem));
        }
        if let Err(problem) = tx.commit() {
            return Err(pending.abort(db_error(problem)));
        }
        pending.seal();
        Ok(result)
    }

    fn bind_local_capture_route(
        &self,
        request: &CaptureRequest,
    ) -> Result<(CaptureRequest, Option<AgentRoute>), DomainError> {
        let mut request = request.clone();
        if let Some(id) = request
            .origin_route_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
        {
            let route = self.route(&id)?;
            request.origin_route_id = Some(route.id.clone());
            return Ok((request, Some(route)));
        }
        request.origin_route_id = None;

        let candidates = self
            .routes()?
            .into_iter()
            .filter(|route| route_matches_workspace(route, &request.workspace_root))
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            let route = candidates.into_iter().next().expect("one candidate");
            request.origin_route_id = Some(route.id.clone());
            return Ok((request, Some(route)));
        }
        Ok((request, None))
    }
}

fn route_matches_workspace(route: &AgentRoute, workspace: &Path) -> bool {
    let Some(original_cwd) = route
        .provenance
        .as_deref()
        .and_then(|provenance| provenance.original_cwd.as_deref())
        .filter(|cwd| !cwd.trim().is_empty())
    else {
        return false;
    };
    let Ok(workspace) = std::fs::canonicalize(workspace) else {
        return false;
    };
    let Ok(original_cwd) = std::fs::canonicalize(original_cwd) else {
        return false;
    };
    original_cwd.starts_with(workspace)
}

fn submit_in_transaction(
    tx: &Transaction<'_>,
    submission: Submission,
) -> Result<SubmissionResult, DomainError> {
    validate_ingress(&submission)?;
    submission.brief.validate()?;
    if submission.manifest.repositories.is_empty() {
        return Err(DomainError::actionable(
            "No repositories were captured.",
            "No queue item was created.",
            "Select at least one Git repository and retry.",
            "repositories_required",
        ));
    }
    let source_adapter = submission
        .source_adapter
        .clone()
        .unwrap_or_else(|| SourceAdapterContract::legacy_for_collection(submission.collection));
    source_adapter.validate()?;
    validate_source_adapter_binding(&submission, &source_adapter)?;
    let hash = manifest_hash(&submission.manifest);
    let now = Utc::now();
    let origin_route_id = submission
        .origin_route
        .as_ref()
        .map(|route| route.id.clone());
    let origin_route = submission.origin_route.clone();
    let active = find_active_by_topic(tx, submission.collection, &submission.topic_identity)?;
    if let Some(old) = active.as_ref()
        && (old.manifest_hash == hash || same_pinned_source(&old.manifest, &submission.manifest))
    {
        return Ok(SubmissionResult::Existing(old.clone()));
    }
    let rank = active
        .as_ref()
        .map_or_else(|| next_rank(tx, submission.collection), |r| Ok(r.rank))?;
    let round = Round {
        id: Uuid::new_v4().to_string(),
        collection: submission.collection,
        topic_identity: submission.topic_identity.clone(),
        manifest_hash: hash,
        brief: submission.brief,
        manifest: submission.manifest,
        rank,
        lifecycle: Lifecycle::Queued,
        superseded_by: None,
        created_at: now,
        origin_route_id,
        origin_route,
        source_metadata: submission.source_metadata,
        source_adapter,
    };
    if let Some(route) = submission.origin_route {
        upsert_route_tx(tx, &route)?;
    }
    insert_round(tx, &round)?;
    let outcome = if let Some(old) = active {
        tx.execute(
            "UPDATE rounds SET superseded_by = ?1, lifecycle = 'completed' WHERE id = ?2",
            params![round.id, old.id],
        )
        .map_err(db_error)?;
        SubmissionResult::Superseded {
            old_id: old.id,
            round: round.clone(),
        }
    } else {
        SubmissionResult::Created(round.clone())
    };
    Ok(outcome)
}

impl Store {
    pub fn list(
        &self,
        collection: Option<Collection>,
        include_old: bool,
    ) -> Result<Vec<Round>, DomainError> {
        let sql = if collection.is_some() {
            if include_old {
                "SELECT * FROM rounds WHERE collection = ?1 ORDER BY rank, created_at"
            } else {
                "SELECT * FROM rounds WHERE collection = ?1 AND lifecycle != 'completed' AND superseded_by IS NULL ORDER BY rank, created_at"
            }
        } else if include_old {
            "SELECT * FROM rounds ORDER BY collection, rank, created_at"
        } else {
            "SELECT * FROM rounds WHERE lifecycle != 'completed' AND superseded_by IS NULL ORDER BY collection, rank, created_at"
        };
        let mut statement = self.conn.prepare(sql).map_err(db_error)?;
        let mut rows = if let Some(c) = collection {
            statement.query(params![c.as_str()]).map_err(db_error)?
        } else {
            statement.query([]).map_err(db_error)?
        };
        let mut result = Vec::new();
        while let Some(row) = rows.next().map_err(db_error)? {
            result.push(round_from_row(row).map_err(db_error)?);
        }
        Ok(result)
    }

    pub fn round(&self, id: &str) -> Result<Round, DomainError> {
        self.conn
            .query_row(
                "SELECT * FROM rounds WHERE id = ?1",
                params![id],
                round_from_row,
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| {
                DomainError::actionable(
                    "That review round no longer exists.",
                    "No source files were changed.",
                    "Return to Queue Home and choose an available round.",
                    "round_not_found",
                )
            })
    }

    pub fn decision(&self, id: &str) -> Result<Option<Decision>, DomainError> {
        self.round(id)?;
        recorded_decision(&self.conn, id)
    }

    pub fn save_machine_snapshot(
        &self,
        round_id: &str,
        snapshot: &MachineSnapshot,
    ) -> Result<(), DomainError> {
        validate_ingress(snapshot)?;
        let round = self.round(round_id)?;
        require_adapter(
            &round,
            "connected_daemon_workspace",
            SourceCapability::RemoteRefresh,
            "a connected-machine snapshot",
        )?;
        self.conn
            .execute(
                "INSERT INTO machine_round_snapshots(round_id,snapshot_json,created_at)
                 VALUES(?1,?2,?3)
                 ON CONFLICT(round_id) DO UPDATE SET snapshot_json=excluded.snapshot_json",
                params![round_id, json(snapshot)?, Utc::now().to_rfc3339()],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn machine_snapshot(&self, round_id: &str) -> Result<MachineSnapshot, DomainError> {
        let round = self.round(round_id)?;
        require_adapter(
            &round,
            "connected_daemon_workspace",
            SourceCapability::RemoteRefresh,
            "a connected-machine snapshot",
        )?;
        self.conn
            .query_row(
                "SELECT snapshot_json FROM machine_round_snapshots WHERE round_id=?1",
                params![round_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?
            .map(parse)
            .transpose()?
            .ok_or_else(|| {
                DomainError::actionable(
                    "This machine round does not have a locally cached source snapshot.",
                    "No remote request was made and the review remains saved.",
                    "Reconnect and materialize the machine round again.",
                    "machine_snapshot_not_cached",
                )
            })
    }

    pub fn save_github_round(
        &self,
        round_id: &str,
        payload: &GithubQueuePayload,
    ) -> Result<GithubRoundState, DomainError> {
        validate_ingress(payload)?;
        let round = self.round(round_id)?;
        require_adapter(
            &round,
            "github_pull_request_mirror",
            SourceCapability::UpstreamDiscussion,
            "GitHub source metadata",
        )?;
        self.conn
            .execute(
                "INSERT INTO github_round_state(
                   round_id,payload_json,files_json,imported_comments_json,staleness_json,updated_at
                 ) VALUES(?1,?2,'[]','[]',NULL,?3)
                 ON CONFLICT(round_id) DO UPDATE SET
                   payload_json=excluded.payload_json,updated_at=excluded.updated_at",
                params![round_id, json(payload)?, Utc::now().to_rfc3339()],
            )
            .map_err(db_error)?;
        let source_metadata = crate::SourceMetadata::Github {
            host: payload.metadata.host.clone(),
            owner: payload.metadata.owner.clone(),
            repository: payload.metadata.repository.clone(),
            pull_number: payload.metadata.pull_number,
            base_sha: payload.metadata.base_sha.clone(),
            head_sha: payload.metadata.head_sha.clone(),
            state: payload.metadata.state,
            is_draft: payload.metadata.is_draft,
            staleness: self
                .github_round(round_id)
                .ok()
                .and_then(|state| state.last_staleness),
        };
        self.conn
            .execute(
                "UPDATE rounds SET source_metadata_json=?1 WHERE id=?2",
                params![json(&source_metadata)?, round_id],
            )
            .map_err(db_error)?;
        self.github_round(round_id)
    }

    pub fn github_round(&self, round_id: &str) -> Result<GithubRoundState, DomainError> {
        let round = self.round(round_id)?;
        require_adapter(
            &round,
            "github_pull_request_mirror",
            SourceCapability::UpstreamDiscussion,
            "a GitHub pull request",
        )?;
        self.conn
            .query_row(
                "SELECT payload_json,files_json,imported_comments_json,staleness_json
                 FROM github_round_state WHERE round_id=?1",
                params![round_id],
                |row| {
                    let read = || -> Result<GithubRoundState, DomainError> {
                        Ok(GithubRoundState {
                            round_id: round_id.to_owned(),
                            payload: parse(row.get(0).map_err(db_error)?)?,
                            files: parse(row.get(1).map_err(db_error)?)?,
                            imported_comments: parse(row.get(2).map_err(db_error)?)?,
                            last_staleness: row
                                .get::<_, Option<String>>(3)
                                .map_err(db_error)?
                                .map(parse)
                                .transpose()?,
                        })
                    };
                    read().map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
                },
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| {
                DomainError::actionable(
                    "This GitHub round is missing its normalized source metadata.",
                    "No remote request was made.",
                    "Refresh or re-add the pull request.",
                    "github_round_state_not_found",
                )
            })
    }

    pub fn save_github_files(
        &self,
        round_id: &str,
        files: &[GithubMaterializedFile],
    ) -> Result<GithubRoundState, DomainError> {
        validate_ingress(&files)?;
        self.github_round(round_id)?;
        self.conn
            .execute(
                "UPDATE github_round_state SET files_json=?1,updated_at=?2 WHERE round_id=?3",
                params![json(&files)?, Utc::now().to_rfc3339(), round_id],
            )
            .map_err(db_error)?;
        self.github_round(round_id)
    }

    pub fn save_github_comments(
        &self,
        round_id: &str,
        comments: &[crate::adapters::ImportedComment],
    ) -> Result<GithubRoundState, DomainError> {
        validate_ingress(&comments)?;
        self.github_round(round_id)?;
        self.conn
            .execute(
                "UPDATE github_round_state
                 SET imported_comments_json=?1,updated_at=?2 WHERE round_id=?3",
                params![json(&comments)?, Utc::now().to_rfc3339(), round_id],
            )
            .map_err(db_error)?;
        self.github_round(round_id)
    }

    pub fn save_github_staleness(
        &self,
        round_id: &str,
        status: &crate::adapters::StalenessStatus,
    ) -> Result<GithubRoundState, DomainError> {
        validate_ingress(status)?;
        self.github_round(round_id)?;
        self.conn
            .execute(
                "UPDATE github_round_state SET staleness_json=?1,updated_at=?2 WHERE round_id=?3",
                params![json(status)?, Utc::now().to_rfc3339(), round_id],
            )
            .map_err(db_error)?;
        let mut round = self.round(round_id)?;
        if let Some(crate::SourceMetadata::Github { staleness, .. }) =
            round.source_metadata.as_mut()
        {
            *staleness = Some(status.clone());
            self.conn
                .execute(
                    "UPDATE rounds SET source_metadata_json=?1 WHERE id=?2",
                    params![json(&round.source_metadata)?, round_id],
                )
                .map_err(db_error)?;
        }
        self.github_round(round_id)
    }

    pub fn github_publish_attempt(
        &self,
        round_id: &str,
    ) -> Result<Option<GithubPublishAttempt>, DomainError> {
        self.round(round_id)?;
        let mut attempt = self
            .conn
            .query_row(
                "SELECT id,preview_json,request_json,status,review_id,created_at,completed_at
                 FROM github_publish_attempts WHERE round_id=?1",
                params![round_id],
                |row| github_publish_attempt_from_row(round_id, row),
            )
            .optional()
            .map_err(db_error)?;
        if let Some(attempt) = attempt.as_mut() {
            attempt.replies = self.github_reply_attempts(round_id)?;
        }
        Ok(attempt)
    }

    pub fn github_publish_attempt_by_public_id(
        &self,
        id: &str,
    ) -> Result<Option<GithubPublishAttempt>, DomainError> {
        let mut attempt = self.conn
            .query_row(
                "SELECT round_id,id,preview_json,request_json,status,review_id,created_at,completed_at
                 FROM github_publish_attempts WHERE id=?1",
                params![id],
                |row| {
                    let round_id: String = row.get(0)?;
                    github_publish_attempt_from_row_offset(&round_id, row, 1)
                },
            )
            .optional()
            .map_err(db_error)?;
        if let Some(attempt) = attempt.as_mut() {
            attempt.replies = self.github_reply_attempts(&attempt.round_id)?;
        }
        Ok(attempt)
    }

    pub fn prepare_github_publish(
        &self,
        round_id: &str,
        preview: &crate::adapters::PublishPreview,
        request: &crate::github::GithubPublishRequest,
        replies: &[GithubReplyRequest],
    ) -> Result<GithubPublishAttempt, DomainError> {
        validate_ingress(&(preview, request, replies))?;
        self.github_round(round_id)?;
        if let Some(existing) = self.github_publish_attempt(round_id)? {
            return Ok(existing);
        }
        let attempt = GithubPublishAttempt {
            id: Uuid::new_v4().to_string(),
            round_id: round_id.to_owned(),
            preview: preview.clone(),
            request: request.clone(),
            status: GithubPublishStatus::Prepared,
            review_id: None,
            created_at: Utc::now(),
            completed_at: None,
            replies: replies
                .iter()
                .map(|request| GithubReplyAttempt {
                    id: Uuid::new_v4().to_string(),
                    round_id: round_id.to_owned(),
                    request: request.clone(),
                    status: GithubPublishStatus::Prepared,
                    comment_id: None,
                    created_at: Utc::now(),
                    completed_at: None,
                })
                .collect(),
        };
        self.conn
            .execute(
                "INSERT INTO github_publish_attempts(
                   id,round_id,idempotency_key,preview_json,request_json,status,review_id,
                   created_at,completed_at
                 ) VALUES(?1,?2,?3,?4,?5,'prepared',NULL,?6,NULL)",
                params![
                    attempt.id,
                    attempt.round_id,
                    attempt.request.idempotency_key,
                    json(&attempt.preview)?,
                    json(&attempt.request)?,
                    attempt.created_at.to_rfc3339()
                ],
            )
            .map_err(db_error)?;
        for reply in &attempt.replies {
            self.conn
                .execute(
                    "INSERT INTO github_reply_attempts(
                       id,round_id,formal_comment_id,formal_revision,idempotency_key,request_json,
                       status,comment_id,created_at,completed_at
                     ) VALUES(?1,?2,?3,?4,?5,?6,'prepared',NULL,?7,NULL)",
                    params![
                        reply.id,
                        reply.round_id,
                        reply.request.formal_comment_id,
                        reply.request.formal_revision,
                        reply.request.idempotency_key,
                        json(&reply.request)?,
                        reply.created_at.to_rfc3339()
                    ],
                )
                .map_err(db_error)?;
        }
        Ok(attempt)
    }

    pub fn github_reply_attempts(
        &self,
        round_id: &str,
    ) -> Result<Vec<GithubReplyAttempt>, DomainError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id,request_json,status,comment_id,created_at,completed_at
                 FROM github_reply_attempts WHERE round_id=?1 ORDER BY created_at,id",
            )
            .map_err(db_error)?;
        statement
            .query_map(params![round_id], |row| {
                github_reply_attempt_from_row(round_id, row)
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)
    }

    pub fn mark_github_reply_posting(&self, id: &str) -> Result<GithubReplyAttempt, DomainError> {
        let changed = self
            .conn
            .execute(
                "UPDATE github_reply_attempts SET status='posting'
                 WHERE id=?1 AND status='prepared'",
                params![id],
            )
            .map_err(db_error)?;
        if changed == 0 {
            return Err(github_publish_state_error());
        }
        self.github_reply_attempt_by_id(id)
    }

    pub fn complete_github_reply(
        &self,
        id: &str,
        comment_id: &str,
    ) -> Result<GithubReplyAttempt, DomainError> {
        self.conn
            .execute(
                "UPDATE github_reply_attempts
                 SET status='completed',comment_id=?1,completed_at=?2
                 WHERE id=?3 AND status='posting'",
                params![comment_id, Utc::now().to_rfc3339(), id],
            )
            .map_err(db_error)?;
        self.github_reply_attempt_by_id(id)
    }

    pub fn mark_github_reply_unknown(&self, id: &str) -> Result<GithubReplyAttempt, DomainError> {
        self.conn
            .execute(
                "UPDATE github_reply_attempts SET status='unknown'
                 WHERE id=?1 AND status='posting'",
                params![id],
            )
            .map_err(db_error)?;
        self.github_reply_attempt_by_id(id)
    }

    fn github_reply_attempt_by_id(&self, id: &str) -> Result<GithubReplyAttempt, DomainError> {
        self.conn
            .query_row(
                "SELECT round_id,id,request_json,status,comment_id,created_at,completed_at
                 FROM github_reply_attempts WHERE id=?1",
                params![id],
                |row| {
                    let round_id: String = row.get(0)?;
                    github_reply_attempt_from_row_offset(&round_id, row, 1)
                },
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(github_publish_state_error)
    }

    pub fn mark_github_publish_posting(
        &self,
        id: &str,
    ) -> Result<GithubPublishAttempt, DomainError> {
        let changed = self
            .conn
            .execute(
                "UPDATE github_publish_attempts SET status='posting'
                 WHERE id=?1 AND status='prepared'",
                params![id],
            )
            .map_err(db_error)?;
        if changed == 0 {
            return Err(github_publish_state_error());
        }
        self.github_publish_attempt_by_id(id)
    }

    pub fn complete_github_publish(
        &self,
        id: &str,
        review_id: &str,
    ) -> Result<GithubPublishAttempt, DomainError> {
        validate_ingress(&review_id)?;
        self.conn
            .execute(
                "UPDATE github_publish_attempts
                 SET status='completed',review_id=?1,completed_at=?2
                 WHERE id=?3 AND status='posting'",
                params![review_id, Utc::now().to_rfc3339(), id],
            )
            .map_err(db_error)?;
        self.github_publish_attempt_by_id(id)
    }

    pub fn mark_github_publish_unknown(
        &self,
        id: &str,
    ) -> Result<GithubPublishAttempt, DomainError> {
        self.conn
            .execute(
                "UPDATE github_publish_attempts SET status='unknown'
                 WHERE id=?1 AND status='posting'",
                params![id],
            )
            .map_err(db_error)?;
        self.github_publish_attempt_by_id(id)
    }

    fn github_publish_attempt_by_id(&self, id: &str) -> Result<GithubPublishAttempt, DomainError> {
        self.conn
            .query_row(
                "SELECT round_id,id,preview_json,request_json,status,review_id,created_at,completed_at
                 FROM github_publish_attempts WHERE id=?1",
                params![id],
                |row| {
                    let round_id: String = row.get(0)?;
                    github_publish_attempt_from_row_offset(&round_id, row, 1)
                },
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(github_publish_state_error)
    }

    pub fn edit_brief(&self, id: &str, brief: &ReviewBrief) -> Result<(), DomainError> {
        validate_ingress(brief)?;
        brief.validate()?;
        let round = self.round(id)?;
        ensure_mutable(&round)?;
        self.conn
            .execute(
                "UPDATE rounds SET brief_json = ?1 WHERE id = ?2",
                params![json(brief)?, id],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn request_changes(&self, id: &str) -> Result<(), DomainError> {
        self.set_decision(id, Decision::RequestChanges)
    }
    pub fn approve_remote(&self, id: &str) -> Result<(), DomainError> {
        let round = self.round(id)?;
        if round.source_adapter.approval == ApprovalDisposition::PurgeRound {
            return Err(DomainError::actionable(
                "This review source requires desktop confirmation and then purges the round.",
                "No review state or source file was changed.",
                "Confirm Approve in the reviewer, or cancel to keep the round.",
                "local_approval_requires_confirmation",
            ));
        }
        self.set_decision(id, Decision::Approve)
    }

    /// Local approval is intentionally terminal: after the caller confirms,
    /// record the explicit approval and purge only app-owned state.
    pub fn approve_local(&self, id: &str) -> Result<(), DomainError> {
        let round = self.round(id)?;
        ensure_mutable(&round)?;
        if round.source_adapter.approval != ApprovalDisposition::PurgeRound {
            return Err(DomainError::actionable(
                "This review source records a decision instead of purging on approval.",
                "No review state changed.",
                "Use the source's decision action for this round.",
                "local_approval_collection_mismatch",
            ));
        }
        let tx = self.conn.unchecked_transaction().map_err(db_error)?;
        insert_lifecycle_event(&tx, id, LifecycleEventKind::ApproveLocal)?;
        tx.execute("DELETE FROM rounds WHERE id=?1", params![id])
            .map_err(db_error)?;
        tx.commit().map_err(db_error)
    }

    fn set_decision(&self, id: &str, decision: Decision) -> Result<(), DomainError> {
        let round = self.round(id)?;
        ensure_mutable(&round)?;
        let lifecycle = match decision {
            Decision::Approve => round.lifecycle,
            Decision::RequestChanges => Lifecycle::ChangesRequested,
        };
        let event_kind = match decision {
            Decision::Approve => LifecycleEventKind::ApproveRemote,
            Decision::RequestChanges => LifecycleEventKind::RequestChanges,
        };
        let tx = self.conn.unchecked_transaction().map_err(db_error)?;
        tx.execute("INSERT INTO decisions(round_id, decision, created_at) VALUES(?1, ?2, ?3) ON CONFLICT(round_id) DO UPDATE SET decision = excluded.decision, created_at = excluded.created_at", params![id, decision_string(decision), Utc::now().to_rfc3339()]).map_err(db_error)?;
        tx.execute(
            "UPDATE rounds SET lifecycle = ?1 WHERE id = ?2",
            params![lifecycle.as_str(), id],
        )
        .map_err(db_error)?;
        insert_lifecycle_event(&tx, id, event_kind)?;
        tx.commit().map_err(db_error)
    }

    pub fn complete(&self, id: &str) -> Result<(), DomainError> {
        let round = self.round(id)?;
        ensure_mutable(&round)?;
        let tx = self.conn.unchecked_transaction().map_err(db_error)?;
        tx.execute(
            "UPDATE rounds SET lifecycle = 'completed' WHERE id = ?1",
            params![id],
        )
        .map_err(db_error)?;
        insert_lifecycle_event(&tx, id, LifecycleEventKind::Complete)?;
        tx.commit().map_err(db_error)
    }

    pub fn requeue(&mut self, id: &str) -> Result<(), DomainError> {
        let round = self.round(id)?;
        if round.superseded_by.is_some() {
            return Err(DomainError::actionable(
                "A superseded round cannot be requeued because a newer source snapshot exists.",
                "Its history remains readable and unchanged.",
                "Open the active successor or submit a new source snapshot.",
                "superseded_round_read_only",
            ));
        }
        let tx = self.conn.transaction().map_err(db_error)?;
        tx.execute("UPDATE rounds SET rank = rank + 1 WHERE collection = ?1 AND lifecycle != 'completed' AND superseded_by IS NULL", params![round.collection.as_str()]).map_err(db_error)?;
        tx.execute(
            "UPDATE rounds SET rank = 0, lifecycle = 'queued' WHERE id = ?1",
            params![id],
        )
        .map_err(db_error)?;
        insert_lifecycle_event(&tx, id, LifecycleEventKind::Requeue)?;
        tx.commit().map_err(db_error)
    }

    pub fn move_rank(&mut self, id: &str, target_rank: i64) -> Result<(), DomainError> {
        let round = self.round(id)?;
        if !round.lifecycle.active() || round.superseded_by.is_some() {
            return Err(DomainError::actionable(
                "This round cannot be reordered while read-only.",
                "Its review history is safe.",
                "Requeue it first or reorder an active round.",
                "round_read_only",
            ));
        }
        let tx = self.conn.transaction().map_err(db_error)?;
        let max_rank: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(rank),0) FROM rounds
                 WHERE collection=?1 AND lifecycle!='completed' AND superseded_by IS NULL",
                params![round.collection.as_str()],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        let target_rank = target_rank.clamp(0, max_rank);
        if target_rank < round.rank {
            tx.execute("UPDATE rounds SET rank = rank + 1 WHERE collection = ?1 AND rank >= ?2 AND rank < ?3 AND lifecycle != 'completed' AND superseded_by IS NULL", params![round.collection.as_str(), target_rank, round.rank]).map_err(db_error)?;
        } else if target_rank > round.rank {
            tx.execute("UPDATE rounds SET rank = rank - 1 WHERE collection = ?1 AND rank <= ?2 AND rank > ?3 AND lifecycle != 'completed' AND superseded_by IS NULL", params![round.collection.as_str(), target_rank, round.rank]).map_err(db_error)?;
        }
        tx.execute(
            "UPDATE rounds SET rank = ?1 WHERE id = ?2",
            params![target_rank, id],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)
    }

    pub fn set_file_viewed(
        &self,
        round_id: &str,
        repository_id: &str,
        path: &str,
        viewed: bool,
    ) -> Result<(), DomainError> {
        validate_ingress(&(repository_id, path))?;
        let round = self.round(round_id)?;
        ensure_mutable(&round)?;
        if repository_id.trim().is_empty() || path.trim().is_empty() {
            return Err(DomainError::actionable(
                "Viewed state needs a repository and file path.",
                "No review state changed.",
                "Choose a changed file and retry.",
                "file_identity_required",
            ));
        }
        self.conn
            .execute(
                "INSERT INTO file_views(round_id,repository_id,path,viewed,updated_at)
                 VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(round_id,repository_id,path)
                 DO UPDATE SET viewed=excluded.viewed,updated_at=excluded.updated_at",
                params![
                    round_id,
                    repository_id,
                    path,
                    i64::from(viewed),
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn viewed_files(&self, round_id: &str) -> Result<Vec<(String, String)>, DomainError> {
        self.round(round_id)?;
        let mut statement = self
            .conn
            .prepare(
                "SELECT repository_id,path FROM file_views
                 WHERE round_id=?1 AND viewed=1 ORDER BY repository_id,path",
            )
            .map_err(db_error)?;
        statement
            .query_map(params![round_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)
    }

    /// Confirmation belongs in the caller/UI. Once confirmed this removes all
    /// app-owned dependent state through foreign-key cascades and never runs Git.
    pub fn purge(&self, id: &str) -> Result<(), DomainError> {
        self.round(id)?;
        let tx = self.conn.unchecked_transaction().map_err(db_error)?;
        insert_lifecycle_event(&tx, id, LifecycleEventKind::Purge)?;
        tx.execute("DELETE FROM rounds WHERE id = ?1", params![id])
            .map_err(db_error)?;
        tx.commit().map_err(db_error)
    }

    pub fn register_route(&self, route: &AgentRoute) -> Result<(), DomainError> {
        validate_ingress(route)?;
        upsert_route(&self.conn, route)
    }

    pub fn route(&self, id: &str) -> Result<AgentRoute, DomainError> {
        self.conn
            .query_row(
                "SELECT id,adapter_kind,agent_id,endpoint,session_id,status,last_heartbeat,provenance_json
                 FROM routes WHERE id=?1",
                params![id],
                route_from_row,
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| {
                DomainError::actionable(
                    "The agent route is not registered.",
                    "No review state changed.",
                    "Register the route before selecting or heartbeating it.",
                    "route_not_found",
                )
            })
    }

    pub fn routes(&self) -> Result<Vec<AgentRoute>, DomainError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id,adapter_kind,agent_id,endpoint,session_id,status,last_heartbeat,provenance_json
                 FROM routes ORDER BY last_heartbeat DESC,id",
            )
            .map_err(db_error)?;
        statement
            .query_map([], route_from_row)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)
    }

    pub fn lifecycle_events(&self, round_id: &str) -> Result<Vec<LifecycleEvent>, DomainError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id,round_id,kind,created_at FROM lifecycle_events
                 WHERE round_id=?1 ORDER BY created_at,id",
            )
            .map_err(db_error)?;
        statement
            .query_map(params![round_id], lifecycle_event_from_row)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)
    }

    /// Creates a locally authored formal draft. Ask turns deliberately have no
    /// representation in this store API and therefore cannot enter delivery.
    pub fn create_formal_comment(
        &self,
        round_id: &str,
        thread_id: &str,
        body: &str,
        anchor: Option<&Anchor>,
    ) -> Result<FormalComment, DomainError> {
        validate_ingress(&(thread_id, body, anchor))?;
        validate_formal_comment(thread_id, body)?;
        let round = self.round(round_id)?;
        ensure_mutable(&round)?;
        let comment = FormalComment {
            id: Uuid::new_v4().to_string(),
            thread_id: thread_id.to_owned(),
            body: body.to_owned(),
            anchor: anchor.cloned(),
            revision: 1,
            delivered_revision: None,
        };
        self.conn
            .execute(
                "INSERT INTO comments(id,round_id,thread_id,body,anchor_json,revision,delivered_revision,created_at) VALUES(?1,?2,?3,?4,?5,?6,NULL,?7)",
                params![comment.id, round_id, comment.thread_id, comment.body, comment.anchor.as_ref().map(json).transpose()?, comment.revision, Utc::now().to_rfc3339()],
            )
            .map_err(db_error)?;
        Ok(comment)
    }

    pub fn formal_comments(&self, round_id: &str) -> Result<Vec<FormalComment>, DomainError> {
        self.round(round_id)?;
        let mut statement = self.conn.prepare("SELECT id,thread_id,body,anchor_json,revision,delivered_revision FROM comments WHERE round_id = ?1 ORDER BY created_at, id").map_err(db_error)?;
        let comments = statement
            .query_map(params![round_id], formal_comment_from_row)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(comments)
    }

    fn formal_comment(&self, comment_id: &str) -> Result<(String, FormalComment), DomainError> {
        self.conn
            .query_row(
                "SELECT round_id,id,thread_id,body,anchor_json,revision,delivered_revision FROM comments WHERE id = ?1",
                params![comment_id],
                |row| {
                    let round_id = row.get(0)?;
                    let comment = formal_comment_from_row_offset(row, 1)?;
                    Ok((round_id, comment))
                },
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| {
                DomainError::actionable(
                    "That formal comment no longer exists.",
                    "No feedback was changed.",
                    "Return to the formal feedback drawer and choose an available comment.",
                    "formal_comment_not_found",
                )
            })
    }

    /// Editing is revisioned even when the comment has not yet been sent. A
    /// later delivery can therefore mark exactly the revision it contained.
    pub fn edit_formal_comment(
        &self,
        comment_id: &str,
        body: &str,
        anchor: Option<&Anchor>,
    ) -> Result<FormalComment, DomainError> {
        validate_ingress(&(body, anchor))?;
        validate_formal_comment("comment", body)?;
        let (round_id, mut comment) = self.formal_comment(comment_id)?;
        ensure_mutable(&self.round(&round_id)?)?;
        comment.body = body.to_owned();
        comment.anchor = anchor.cloned();
        comment.revision += 1;
        self.conn
            .execute(
                "UPDATE comments SET body = ?1, anchor_json = ?2, revision = ?3 WHERE id = ?4",
                params![
                    comment.body,
                    comment.anchor.as_ref().map(json).transpose()?,
                    comment.revision,
                    comment_id
                ],
            )
            .map_err(db_error)?;
        Ok(comment)
    }

    /// Resolving a locally authored draft is deletion. Imported upstream
    /// comments are adapter-owned and never enter this table.
    pub fn delete_formal_comment(&self, comment_id: &str) -> Result<(), DomainError> {
        let (round_id, _) = self.formal_comment(comment_id)?;
        ensure_mutable(&self.round(&round_id)?)?;
        self.conn
            .execute("DELETE FROM comments WHERE id=?1", params![comment_id])
            .map_err(db_error)?;
        Ok(())
    }

    /// Persists an immutable, idempotent manual-handoff payload. It contains
    /// the current decision and only revisions that have not already been
    /// confirmed as manually submitted.
    pub fn prepare_delivery(&mut self, round_id: &str) -> Result<DurableDelivery, DomainError> {
        let round = self.round(round_id)?;
        ensure_mutable(&round)?;
        if let Some(pending) = self.pending_delivery(round_id)? {
            return Ok(pending);
        }
        let tx = self.conn.transaction().map_err(db_error)?;
        let decision = recorded_decision(&tx, round_id)?.ok_or_else(|| DomainError::actionable(
            "Formal feedback needs an approval or request-changes decision before it can be sent.",
            "Your drafts are saved locally and were not delivered.",
            "Record Approve or Request changes, then send again.",
            "decision_required",
        ))?;
        let comments = formal_comments_tx(&tx, round_id)?
            .into_iter()
            .filter(|comment| comment.delivered_revision != Some(comment.revision))
            .collect::<Vec<_>>();
        if comments.is_empty() {
            return Err(DomainError::actionable(
                "Formal feedback has no undelivered comments.",
                "No delivery record was created and nothing was sent.",
                "Add or edit at least one formal comment, then prepare the prompt again.",
                "formal_comments_required",
            ));
        }
        let delivery = DurableDelivery {
            id: Uuid::new_v4().to_string(),
            idempotency_key: Uuid::new_v4().to_string(),
            payload: DeliveryPayload {
                round_id: round_id.to_owned(),
                decision,
                comments,
            },
        };
        validate_ingress(&delivery)?;
        tx.execute("INSERT INTO deliveries(id,round_id,idempotency_key,payload_json,created_at,delivered_at,outcome) VALUES(?1,?2,?3,?4,?5,NULL,NULL)", params![delivery.id, round_id, delivery.idempotency_key, json(&delivery.payload)?, Utc::now().to_rfc3339()]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(delivery)
    }

    /// Returns the latest immutable delivery awaiting manual confirmation.
    ///
    /// Early preview builds could persist a decision-only delivery. Such an
    /// object can never form a valid feedback prompt, so it is retired
    /// defensively and never blocks creation of a corrected delivery.
    pub fn pending_delivery(&self, round_id: &str) -> Result<Option<DurableDelivery>, DomainError> {
        self.round(round_id)?;
        loop {
            let pending = self
                .conn
                .query_row(
                    "SELECT id,idempotency_key,payload_json FROM deliveries
                 WHERE round_id=?1 AND delivered_at IS NULL
                 ORDER BY created_at DESC,id DESC LIMIT 1",
                    params![round_id],
                    delivery_from_row,
                )
                .optional()
                .map_err(db_error)?;
            let Some(delivery) = pending else {
                return Ok(None);
            };
            if !delivery.payload.comments.is_empty() {
                return Ok(Some(delivery));
            }
            self.conn
                .execute(
                    "UPDATE deliveries
                     SET delivered_at=?1,outcome='invalid_empty_payload'
                     WHERE id=?2 AND delivered_at IS NULL",
                    params![Utc::now().to_rfc3339(), delivery.id],
                )
                .map_err(db_error)?;
        }
    }

    pub fn delivery_history(
        &self,
        round_id: &str,
    ) -> Result<Vec<DeliveryHistoryEntry>, DomainError> {
        self.round(round_id)?;
        let mut statement = self
            .conn
            .prepare(
                "SELECT id,idempotency_key,payload_json,created_at,delivered_at,outcome
                 FROM deliveries WHERE round_id=?1 ORDER BY created_at,id",
            )
            .map_err(db_error)?;
        statement
            .query_map(params![round_id], delivery_history_from_row)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)
    }

    /// Records the user's explicit confirmation that they manually submitted
    /// the prepared prompt. This never communicates with an agent.
    pub fn mark_delivery_manually_submitted(
        &mut self,
        delivery_id: &str,
    ) -> Result<(), DomainError> {
        let round_id = self
            .conn
            .query_row(
                "SELECT round_id FROM deliveries WHERE id = ?1",
                params![delivery_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| {
                DomainError::actionable(
                    "That feedback delivery no longer exists.",
                    "No comment state changed.",
                    "Choose a delivery from the saved handoff history.",
                    "delivery_not_found",
                )
            })?;
        ensure_mutable(&self.round(&round_id)?)?;
        let tx = self.conn.transaction().map_err(db_error)?;
        let (payload_json, delivered_at): (String, Option<String>) = tx
            .query_row(
                "SELECT payload_json, delivered_at FROM deliveries WHERE id = ?1",
                params![delivery_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| {
                DomainError::actionable(
                    "That feedback delivery no longer exists.",
                    "No comment state changed.",
                    "Create a new delivery from the formal feedback drawer.",
                    "delivery_not_found",
                )
            })?;
        if delivered_at.is_some() {
            tx.commit().map_err(db_error)?;
            return Ok(());
        }
        let payload: DeliveryPayload = parse(payload_json)?;
        for comment in payload.comments {
            tx.execute("UPDATE comments SET delivered_revision = CASE WHEN delivered_revision IS NULL OR delivered_revision < ?1 THEN ?1 ELSE delivered_revision END WHERE id = ?2", params![comment.revision, comment.id]).map_err(db_error)?;
        }
        tx.execute(
            "UPDATE deliveries
             SET delivered_at = ?1, outcome='manual_submission_confirmed'
             WHERE id = ?2",
            params![Utc::now().to_rfc3339(), delivery_id],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)
    }

    /// Compatibility alias for callers that already model explicit manual
    /// confirmation. There is no transport operation behind this method.
    pub fn mark_delivery(&mut self, delivery_id: &str) -> Result<(), DomainError> {
        self.mark_delivery_manually_submitted(delivery_id)
    }
    pub fn heartbeat(&self, id: &str, status: &str) -> Result<(), DomainError> {
        validate_ingress(&status)?;
        let changed = self
            .conn
            .execute(
                "UPDATE routes SET status = ?1, last_heartbeat = ?2 WHERE id = ?3",
                params![status, Utc::now().to_rfc3339(), id],
            )
            .map_err(db_error)?;
        if changed == 0 {
            return Err(DomainError::actionable(
                "The agent route is not registered.",
                "No review state changed.",
                "Run review-queue agent register first.",
                "route_not_found",
            ));
        }
        Ok(())
    }

    pub fn add_machine(&self, name: &str, endpoint: &str) -> Result<(String, bool), DomainError> {
        let config = MachineConfig {
            name: name.to_owned(),
            endpoint: MachineEndpoint::Ssh {
                target: endpoint.to_owned(),
                remote_socket: DEFAULT_REMOTE_SOCKET.to_owned(),
                adapter: SshAdapter::SystemOpenSsh,
            },
            source_type: MachineSourceType::ReviewQueueDaemon,
        };
        self.add_machine_config(&config)
            .map(|(record, created)| (record.id, created))
    }

    pub fn add_machine_config(
        &self,
        config: &MachineConfig,
    ) -> Result<(ConnectedMachineRecord, bool), DomainError> {
        validate_ingress(config)?;
        config.validate()?;
        let config = config.clone().normalized();
        let endpoint_key = json(&config.endpoint)?;
        if let Some((id, found_config_json)) = self
            .conn
            .query_row(
                "SELECT id,config_json FROM machines WHERE name = ?1 COLLATE NOCASE OR endpoint = ?2",
                params![config.name, endpoint_key],
                |r| -> rusqlite::Result<(String, Option<String>)> {
                    Ok((r.get(0)?, r.get(1)?))
                },
            )
            .optional()
            .map_err(db_error)?
        {
            if let Some(found_config_json) = found_config_json {
                let found: MachineConfig = parse(found_config_json)?;
                if found == config {
                    return Ok((ConnectedMachineRecord { id, config: found }, false));
                }
            }
            return Err(DomainError::actionable(
                "That machine name or endpoint is already used by a different machine configuration.",
                "Nothing was changed.",
                "Choose a unique machine name and endpoint, or remove the old configuration first.",
                "machine_config_conflict",
            ));
        }
        let id = config.machine_id()?;
        self.conn
            .execute(
                "INSERT INTO machines(id,name,endpoint,created_at,config_json) VALUES(?1,?2,?3,?4,?5)",
                params![
                    id,
                    config.name,
                    endpoint_key,
                    Utc::now().to_rfc3339(),
                    json(&config)?
                ],
            )
            .map_err(db_error)?;
        Ok((ConnectedMachineRecord { id, config }, true))
    }

    pub fn machines(&self) -> Result<Vec<MachineRecord>, DomainError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id,name,endpoint,created_at,config_json FROM machines ORDER BY created_at,id",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        rows.into_iter()
            .map(|(id, name, endpoint, created_at, config_json)| {
                let endpoint = config_json
                    .map(parse::<MachineConfig>)
                    .transpose()?
                    .map(|config| match config.endpoint {
                        MachineEndpoint::Loopback { socket_path } => socket_path,
                        MachineEndpoint::Ssh { target, .. } => target,
                    })
                    .unwrap_or(endpoint);
                Ok(MachineRecord {
                    id,
                    name,
                    endpoint,
                    created_at: parse_time(created_at)?,
                })
            })
            .collect()
    }

    pub fn machine_configs(&self) -> Result<Vec<ConnectedMachineRecord>, DomainError> {
        let mut statement = self
            .conn
            .prepare("SELECT id,name,endpoint,config_json FROM machines ORDER BY created_at,id")
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        rows.into_iter()
            .map(|(id, name, endpoint, config_json)| {
                let config = match config_json {
                    Some(value) => parse(value)?,
                    None => MachineConfig {
                        name,
                        endpoint: MachineEndpoint::Ssh {
                            target: endpoint,
                            remote_socket: DEFAULT_REMOTE_SOCKET.to_owned(),
                            adapter: SshAdapter::SystemOpenSsh,
                        },
                        source_type: MachineSourceType::ReviewQueueDaemon,
                    },
                };
                config.validate()?;
                Ok(ConnectedMachineRecord { id, config })
            })
            .collect()
    }

    pub fn machine_config(&self, id_or_name: &str) -> Result<ConnectedMachineRecord, DomainError> {
        self.machine_configs()?
            .into_iter()
            .find(|record| record.id == id_or_name || record.config.name == id_or_name)
            .ok_or_else(|| {
                DomainError::actionable(
                    "That connected machine is not configured.",
                    "No connection was opened and no review data changed.",
                    "Refresh the machine list or add the machine before connecting.",
                    "machine_not_found",
                )
            })
    }

    /// Verifies every logical database value together with a caller-supplied
    /// config/protocol projection, then returns a metadata-only export. Raw
    /// values never cross this diagnostic boundary.
    pub fn export_redacted_artifact<T: Serialize>(
        &self,
        config_protocol: &T,
    ) -> Result<RedactedArtifactExport, DomainError> {
        validate_ingress(config_protocol)?;
        let mut table_row_counts = BTreeMap::new();
        for table in [
            "rounds",
            "routes",
            "comments",
            "decisions",
            "deliveries",
            "lifecycle_events",
            "conversations",
            "ask_turns",
            "ask_turn_chunks",
            "machines",
            "file_views",
        ] {
            let sql = format!("SELECT * FROM {table}");
            let mut statement = self.conn.prepare(&sql).map_err(db_error)?;
            let column_count = statement.column_count();
            let mut rows = statement.query([]).map_err(db_error)?;
            let mut count = 0_u64;
            while let Some(row) = rows.next().map_err(db_error)? {
                count += 1;
                for index in 0..column_count {
                    use rusqlite::types::ValueRef;
                    match row.get_ref(index).map_err(db_error)? {
                        ValueRef::Text(value) | ValueRef::Blob(value) => {
                            validate_persisted_text(&String::from_utf8_lossy(value))?
                        }
                        ValueRef::Null | ValueRef::Integer(_) | ValueRef::Real(_) => {}
                    }
                }
            }
            table_row_counts.insert(table.to_owned(), count);
        }
        Ok(RedactedArtifactExport {
            schema_version: crate::SCHEMA_VERSION,
            verified_at: Utc::now(),
            table_row_counts,
            contains_raw_values: false,
        })
    }

    pub fn remove_machine(&self, id_or_name: &str) -> Result<(String, bool), DomainError> {
        if id_or_name.trim().is_empty() {
            return Err(DomainError::actionable(
                "A machine ID or name is required.",
                "Nothing was removed.",
                "Pass the machine ID or exact display name and retry.",
                "machine_identifier_required",
            ));
        }
        let existing = self
            .conn
            .query_row(
                "SELECT id FROM machines WHERE id=?1 OR name=?1",
                params![id_or_name],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?;
        let Some(id) = existing else {
            return Ok((id_or_name.to_owned(), false));
        };
        self.conn
            .execute("DELETE FROM machines WHERE id=?1", params![id])
            .map_err(db_error)?;
        Ok((id, true))
    }

    fn active_conversation_for_round(
        &self,
        round_id: &str,
    ) -> Result<Option<AskConversation>, DomainError> {
        self.conn.query_row("SELECT id,round_id,session_state,history_only_reason,provider_session_label,options_json,created_at,archived_at FROM conversations WHERE round_id=?1 AND active=1", params![round_id], conversation_from_row).optional().map_err(db_error)
    }
    fn conversation(&self, id: &str) -> Result<AskConversation, DomainError> {
        self.conn.query_row("SELECT id,round_id,session_state,history_only_reason,provider_session_label,options_json,created_at,archived_at FROM conversations WHERE id=?1", params![id], conversation_from_row).optional().map_err(db_error)?.ok_or_else(|| ask_not_found("conversation_not_found"))
    }
    fn insert_conversation(&self, chat: &AskConversation, active: bool) -> Result<(), DomainError> {
        insert_conversation_conn(&self.conn, chat, active)
    }
    fn turn_by_idempotency(&self, key: &str) -> Result<Option<AskTurn>, DomainError> {
        self.conn.query_row("SELECT id,conversation_id,idempotency_key,prompt,anchor_json,option_values_json,state,created_at,completed_at,failure_reason,response_text FROM ask_turns WHERE idempotency_key=?1",params![key],ask_turn_from_row).optional().map_err(db_error)
    }
    fn ask_turn(&self, id: &str) -> Result<AskTurn, DomainError> {
        self.conn.query_row("SELECT id,conversation_id,idempotency_key,prompt,anchor_json,option_values_json,state,created_at,completed_at,failure_reason,response_text FROM ask_turns WHERE id=?1",params![id],ask_turn_from_row).optional().map_err(db_error)?.ok_or_else(|| ask_not_found("ask_turn_not_found"))
    }
    fn transition_ask_turn(
        &self,
        id: &str,
        next: AskTurnState,
        reason: Option<&str>,
    ) -> Result<AskTurn, DomainError> {
        let turn = self.ask_turn(id)?;
        let valid = matches!(
            (turn.state, next),
            (AskTurnState::Queued, AskTurnState::Streaming)
                | (AskTurnState::Queued, AskTurnState::Cancelled)
                | (AskTurnState::Streaming, AskTurnState::Completed)
                | (AskTurnState::Streaming, AskTurnState::Cancelled)
                | (
                    AskTurnState::Queued | AskTurnState::Streaming,
                    AskTurnState::Failed | AskTurnState::Interrupted
                )
        );
        if !valid {
            return Err(DomainError::actionable(
                "This prompt cannot make that state transition.",
                "Its saved transcript and state are unchanged.",
                "Create a new prompt to retry after a terminal result.",
                "invalid_ask_turn_transition",
            ));
        }
        if matches!(next, AskTurnState::Failed | AskTurnState::Interrupted)
            && reason.unwrap_or("").trim().is_empty()
        {
            return Err(DomainError::actionable(
                "A failed prompt needs a recovery reason.",
                "The prompt remains saved.",
                "Record the provider failure before marking it failed.",
                "ask_turn_failure_reason_required",
            ));
        }
        let completed = matches!(
            next,
            AskTurnState::Completed
                | AskTurnState::Cancelled
                | AskTurnState::Failed
                | AskTurnState::Interrupted
        )
        .then(|| Utc::now().to_rfc3339());
        self.conn
            .execute(
                "UPDATE ask_turns SET state=?1, failure_reason=?2, completed_at=?3 WHERE id=?4",
                params![ask_state(next), reason, completed, id],
            )
            .map_err(db_error)?;
        self.ask_turn(id)
    }
}

/// Universal persistence boundary for token-free core state. It rejects
/// credential field names and high-confidence token shapes, while allowing
/// ordinary prose such as discussion of OAuth or bearer authentication.
pub fn validate_store_ingress<T: Serialize>(value: &T) -> Result<(), DomainError> {
    let value = serde_json::to_value(value).map_err(db_error)?;
    validate_ingress_json(&value)
}

fn validate_ingress<T: Serialize>(value: &T) -> Result<(), DomainError> {
    validate_store_ingress(value)
}

fn validate_ingress_json(value: &serde_json::Value) -> Result<(), DomainError> {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let key = key.to_ascii_lowercase().replace('-', "_");
                if [
                    "credential",
                    "credentials",
                    "token",
                    "access_token",
                    "refresh_token",
                    "github_token",
                    "copilot_token",
                    "api_key",
                    "client_secret",
                    "device_code",
                    "code_verifier",
                    "password",
                    "passphrase",
                    "private_key",
                    "identity_file",
                    "authorization",
                ]
                .contains(&key.as_str())
                {
                    return Err(token_ingress_error());
                }
                validate_ingress_json(value)?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                validate_ingress_json(value)?;
            }
        }
        serde_json::Value::String(value) => validate_ingress_string(value)?,
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
    Ok(())
}

fn validate_ingress_string(value: &str) -> Result<(), DomainError> {
    let lower = value.to_ascii_lowercase();
    let pem = lower.contains("-----begin private key-----")
        || lower.contains("-----begin openssh private key-----");
    let prefixed = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"]
        .iter()
        .any(|prefix| {
            lower.match_indices(prefix).any(|(start, _)| {
                lower[start + prefix.len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                    .count()
                    >= 8
            })
        });
    let long_secret_key = lower.match_indices("sk-").any(|(start, _)| {
        lower[start + 3..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            .count()
            >= 16
    });
    let jwt = value.split_whitespace().any(|word| {
        let word = word.trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'
            )
        });
        let mut parts = word.split('.');
        matches!(
            (parts.next(), parts.next(), parts.next(), parts.next()),
            (Some(header), Some(payload), Some(signature), None)
                if header.starts_with("eyJ")
                    && payload.len() >= 8
                    && signature.len() >= 8
        )
    });
    let authorization = lower
        .find("authorization:")
        .and_then(|index| {
            lower[index + "authorization:".len()..]
                .split_whitespace()
                .nth(1)
        })
        .is_some_and(credential_candidate);
    let bearer = lower.match_indices("bearer ").any(|(start, _)| {
        lower[start + "bearer ".len()..]
            .split_whitespace()
            .next()
            .is_some_and(credential_candidate)
    });
    if pem || prefixed || long_secret_key || jwt || authorization || bearer {
        return Err(token_ingress_error());
    }
    Ok(())
}

fn validate_persisted_text(value: &str) -> Result<(), DomainError> {
    validate_ingress_string(value)?;
    let trimmed = value.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(value)
    {
        validate_ingress_json(&json)?;
    }
    Ok(())
}

fn credential_candidate(candidate: &str) -> bool {
    let candidate = candidate
        .trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | ')' | ']' | '}' | '.'));
    candidate.len() >= 12
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '~' | '+' | '/'))
        && (candidate.len() >= 20
            || candidate
                .chars()
                .any(|c| c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | '+' | '/')))
}

fn token_ingress_error() -> DomainError {
    DomainError::actionable(
        "Token-shaped material cannot be saved in Review Queue core state.",
        "The value was rejected before any database write.",
        "Remove credentials and pass only normalized, token-free metadata.",
        "token_shaped_ingress",
    )
}

fn ask_not_found(code: &str) -> DomainError {
    DomainError::actionable(
        "That chat item no longer exists.",
        "No prompt was sent and saved history is unchanged.",
        "Refresh the review round and try again.",
        code,
    )
}
fn insert_conversation_conn(
    conn: &Connection,
    chat: &AskConversation,
    active: bool,
) -> Result<(), DomainError> {
    conn.execute("INSERT INTO conversations(id,round_id,active,history_only,options_json,created_at,session_state,history_only_reason,provider_session_label,archived_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![chat.id,chat.round_id,i64::from(active),i64::from(chat.session_state==ConversationSessionState::HistoryOnly),json(&chat.options)?,chat.created_at.to_rfc3339(),conversation_state(chat.session_state),chat.history_only_reason,chat.provider_session_label,chat.archived_at.map(|x|x.to_rfc3339())]).map_err(db_error)?;
    Ok(())
}
fn insert_conversation_tx(
    tx: &Transaction<'_>,
    chat: &AskConversation,
    active: bool,
) -> Result<(), DomainError> {
    tx.execute("INSERT INTO conversations(id,round_id,active,history_only,options_json,created_at,session_state,history_only_reason,provider_session_label,archived_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![chat.id,chat.round_id,i64::from(active),i64::from(chat.session_state==ConversationSessionState::HistoryOnly),json(&chat.options)?,chat.created_at.to_rfc3339(),conversation_state(chat.session_state),chat.history_only_reason,chat.provider_session_label,chat.archived_at.map(|x|x.to_rfc3339())]).map_err(db_error)?;
    Ok(())
}
fn conversation_state(state: ConversationSessionState) -> &'static str {
    match state {
        ConversationSessionState::CanContinue => "can_continue",
        ConversationSessionState::HistoryOnly => "history_only",
    }
}
fn read_conversation_state(value: String) -> Result<ConversationSessionState, DomainError> {
    match value.as_str() {
        "can_continue" => Ok(ConversationSessionState::CanContinue),
        "history_only" => Ok(ConversationSessionState::HistoryOnly),
        _ => Err(db_error("invalid conversation state")),
    }
}
fn ask_state(state: AskTurnState) -> &'static str {
    match state {
        AskTurnState::Queued => "queued",
        AskTurnState::Streaming => "streaming",
        AskTurnState::Completed => "completed",
        AskTurnState::Cancelled => "cancelled",
        AskTurnState::Failed => "failed",
        AskTurnState::Interrupted => "interrupted",
    }
}
fn read_ask_state(value: String) -> Result<AskTurnState, DomainError> {
    match value.as_str() {
        "queued" => Ok(AskTurnState::Queued),
        "streaming" => Ok(AskTurnState::Streaming),
        "completed" => Ok(AskTurnState::Completed),
        "cancelled" => Ok(AskTurnState::Cancelled),
        "failed" => Ok(AskTurnState::Failed),
        "interrupted" => Ok(AskTurnState::Interrupted),
        _ => Err(db_error("invalid ask turn state")),
    }
}
fn conversation_from_row(row: &rusqlite::Row<'_>) -> Result<AskConversation, rusqlite::Error> {
    let read = || -> Result<AskConversation, DomainError> {
        Ok(AskConversation {
            id: row.get(0).map_err(db_error)?,
            round_id: row.get(1).map_err(db_error)?,
            session_state: read_conversation_state(row.get(2).map_err(db_error)?)?,
            history_only_reason: row.get(3).map_err(db_error)?,
            provider_session_label: row.get(4).map_err(db_error)?,
            options: parse(row.get(5).map_err(db_error)?)?,
            created_at: parse_time(row.get(6).map_err(db_error)?)?,
            archived_at: row
                .get::<_, Option<String>>(7)
                .map_err(db_error)?
                .map(parse_time)
                .transpose()?,
        })
    };
    read().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}
fn ask_turn_from_row(row: &rusqlite::Row<'_>) -> Result<AskTurn, rusqlite::Error> {
    let read = || -> Result<AskTurn, DomainError> {
        Ok(AskTurn {
            id: row.get(0).map_err(db_error)?,
            conversation_id: row.get(1).map_err(db_error)?,
            idempotency_key: row.get(2).map_err(db_error)?,
            prompt: row.get(3).map_err(db_error)?,
            anchor: row
                .get::<_, Option<String>>(4)
                .map_err(db_error)?
                .map(parse)
                .transpose()?,
            option_values: parse(row.get(5).map_err(db_error)?)?,
            state: read_ask_state(row.get(6).map_err(db_error)?)?,
            created_at: parse_time(row.get(7).map_err(db_error)?)?,
            completed_at: row
                .get::<_, Option<String>>(8)
                .map_err(db_error)?
                .map(parse_time)
                .transpose()?,
            failure_reason: row.get(9).map_err(db_error)?,
            response_text: row.get(10).map_err(db_error)?,
        })
    };
    read().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn db_error(error: impl std::fmt::Display) -> DomainError {
    DomainError::actionable(
        format!("Review Queue could not update its local database: {error}"),
        "Source files and Git commits are unchanged.",
        "Retry the action; if it persists, open redacted diagnostics.",
        "database_error",
    )
}
fn table_has_column(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}
fn json<T: serde::Serialize>(v: &T) -> Result<String, DomainError> {
    serde_json::to_string(v).map_err(db_error)
}
fn parse<T: serde::de::DeserializeOwned>(v: String) -> Result<T, DomainError> {
    serde_json::from_str(&v).map_err(db_error)
}
fn parse_time(v: String) -> Result<DateTime<Utc>, DomainError> {
    DateTime::parse_from_rfc3339(&v)
        .map(|t| t.with_timezone(&Utc))
        .map_err(db_error)
}
fn collection(v: String) -> Result<Collection, DomainError> {
    serde_json::from_str(&format!("\"{v}\"")).map_err(db_error)
}
fn lifecycle(v: String) -> Result<Lifecycle, DomainError> {
    serde_json::from_str(&format!("\"{v}\"")).map_err(db_error)
}
fn lifecycle_event_kind(v: String) -> Result<LifecycleEventKind, DomainError> {
    match v.as_str() {
        "request_changes" => Ok(LifecycleEventKind::RequestChanges),
        "approve_local" => Ok(LifecycleEventKind::ApproveLocal),
        "approve_remote" => Ok(LifecycleEventKind::ApproveRemote),
        "complete" => Ok(LifecycleEventKind::Complete),
        "requeue" => Ok(LifecycleEventKind::Requeue),
        "purge" => Ok(LifecycleEventKind::Purge),
        _ => Err(db_error("invalid lifecycle event kind")),
    }
}
fn decision_string(d: Decision) -> &'static str {
    match d {
        Decision::Approve => "approve",
        Decision::RequestChanges => "request_changes",
    }
}
fn decision(v: String) -> Result<Decision, DomainError> {
    serde_json::from_str(&format!("\"{v}\"")).map_err(db_error)
}
fn github_publish_status(v: String) -> Result<GithubPublishStatus, DomainError> {
    serde_json::from_str(&format!("\"{v}\"")).map_err(db_error)
}
fn github_publish_attempt_from_row(
    round_id: &str,
    row: &rusqlite::Row<'_>,
) -> Result<GithubPublishAttempt, rusqlite::Error> {
    github_publish_attempt_from_row_offset(round_id, row, 0)
}
fn github_publish_attempt_from_row_offset(
    round_id: &str,
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> Result<GithubPublishAttempt, rusqlite::Error> {
    let read = || -> Result<GithubPublishAttempt, DomainError> {
        Ok(GithubPublishAttempt {
            id: row.get(offset).map_err(db_error)?,
            round_id: round_id.to_owned(),
            preview: parse(row.get(offset + 1).map_err(db_error)?)?,
            request: parse(row.get(offset + 2).map_err(db_error)?)?,
            status: github_publish_status(row.get(offset + 3).map_err(db_error)?)?,
            review_id: row.get(offset + 4).map_err(db_error)?,
            created_at: parse_time(row.get(offset + 5).map_err(db_error)?)?,
            completed_at: row
                .get::<_, Option<String>>(offset + 6)
                .map_err(db_error)?
                .map(parse_time)
                .transpose()?,
            replies: Vec::new(),
        })
    };
    read().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}
fn github_reply_attempt_from_row(
    round_id: &str,
    row: &rusqlite::Row<'_>,
) -> Result<GithubReplyAttempt, rusqlite::Error> {
    github_reply_attempt_from_row_offset(round_id, row, 0)
}
fn github_reply_attempt_from_row_offset(
    round_id: &str,
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> Result<GithubReplyAttempt, rusqlite::Error> {
    let read = || -> Result<GithubReplyAttempt, DomainError> {
        Ok(GithubReplyAttempt {
            id: row.get(offset).map_err(db_error)?,
            round_id: round_id.to_owned(),
            request: parse(row.get(offset + 1).map_err(db_error)?)?,
            status: github_publish_status(row.get(offset + 2).map_err(db_error)?)?,
            comment_id: row.get(offset + 3).map_err(db_error)?,
            created_at: parse_time(row.get(offset + 4).map_err(db_error)?)?,
            completed_at: row
                .get::<_, Option<String>>(offset + 5)
                .map_err(db_error)?
                .map(parse_time)
                .transpose()?,
        })
    };
    read().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}
fn github_publish_state_error() -> DomainError {
    DomainError::actionable(
        "This GitHub publish attempt cannot be posted again.",
        "No additional GitHub write was made and local drafts are preserved.",
        "If its outcome is unknown, inspect the pull request before preparing a new review.",
        "github_publish_state_conflict",
    )
}
fn lifecycle_event_from_row(row: &rusqlite::Row<'_>) -> Result<LifecycleEvent, rusqlite::Error> {
    let read = || -> Result<LifecycleEvent, DomainError> {
        Ok(LifecycleEvent {
            id: row.get(0).map_err(db_error)?,
            round_id: row.get(1).map_err(db_error)?,
            kind: lifecycle_event_kind(row.get(2).map_err(db_error)?)?,
            created_at: parse_time(row.get(3).map_err(db_error)?)?,
        })
    };
    read().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}
fn insert_lifecycle_event(
    tx: &Transaction<'_>,
    round_id: &str,
    kind: LifecycleEventKind,
) -> Result<(), DomainError> {
    tx.execute(
        "INSERT INTO lifecycle_events(id,round_id,kind,created_at) VALUES(?1,?2,?3,?4)",
        params![
            Uuid::new_v4().to_string(),
            round_id,
            kind.as_str(),
            Utc::now().to_rfc3339()
        ],
    )
    .map_err(db_error)?;
    Ok(())
}
fn route_from_row(row: &rusqlite::Row<'_>) -> Result<AgentRoute, rusqlite::Error> {
    let read = || -> Result<AgentRoute, DomainError> {
        Ok(AgentRoute {
            id: row.get(0).map_err(db_error)?,
            adapter_kind: row.get(1).map_err(db_error)?,
            agent_id: row.get(2).map_err(db_error)?,
            endpoint: row.get(3).map_err(db_error)?,
            session_id: row.get(4).map_err(db_error)?,
            status: row.get(5).map_err(db_error)?,
            last_heartbeat: parse_time(row.get(6).map_err(db_error)?)?,
            provenance: row
                .get::<_, Option<String>>(7)
                .map_err(db_error)?
                .map(parse)
                .transpose()?,
        })
    };
    read().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}
fn delivery_from_row(row: &rusqlite::Row<'_>) -> Result<DurableDelivery, rusqlite::Error> {
    let read = || -> Result<DurableDelivery, DomainError> {
        Ok(DurableDelivery {
            id: row.get(0).map_err(db_error)?,
            idempotency_key: row.get(1).map_err(db_error)?,
            payload: parse(row.get(2).map_err(db_error)?)?,
        })
    };
    read().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}
fn delivery_history_from_row(
    row: &rusqlite::Row<'_>,
) -> Result<DeliveryHistoryEntry, rusqlite::Error> {
    let read = || -> Result<DeliveryHistoryEntry, DomainError> {
        Ok(DeliveryHistoryEntry {
            delivery: DurableDelivery {
                id: row.get(0).map_err(db_error)?,
                idempotency_key: row.get(1).map_err(db_error)?,
                payload: parse(row.get(2).map_err(db_error)?)?,
            },
            created_at: parse_time(row.get(3).map_err(db_error)?)?,
            delivered_at: row
                .get::<_, Option<String>>(4)
                .map_err(db_error)?
                .map(parse_time)
                .transpose()?,
            outcome: row.get(5).map_err(db_error)?,
        })
    };
    read().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}
fn validate_formal_comment(thread_id: &str, body: &str) -> Result<(), DomainError> {
    if thread_id.trim().is_empty() || body.trim().is_empty() {
        return Err(DomainError::actionable(
            "A formal comment needs both a thread and comment text.",
            "Nothing was changed or delivered.",
            "Choose a comment thread and enter feedback, then try again.",
            "formal_comment_required",
        ));
    }
    Ok(())
}
fn formal_comment_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FormalComment> {
    formal_comment_from_row_offset(row, 0)
}
fn formal_comment_from_row_offset(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<FormalComment> {
    let read = || -> Result<FormalComment, DomainError> {
        Ok(FormalComment {
            id: row.get(offset).map_err(db_error)?,
            thread_id: row.get(offset + 1).map_err(db_error)?,
            body: row.get(offset + 2).map_err(db_error)?,
            anchor: row
                .get::<_, Option<String>>(offset + 3)
                .map_err(db_error)?
                .map(parse)
                .transpose()?,
            revision: row.get(offset + 4).map_err(db_error)?,
            delivered_revision: row.get(offset + 5).map_err(db_error)?,
        })
    };
    read().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}
fn formal_comments_tx(
    tx: &Transaction<'_>,
    round_id: &str,
) -> Result<Vec<FormalComment>, DomainError> {
    let mut statement = tx.prepare("SELECT id,thread_id,body,anchor_json,revision,delivered_revision FROM comments WHERE round_id = ?1 ORDER BY created_at, id").map_err(db_error)?;
    statement
        .query_map(params![round_id], formal_comment_from_row)
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)
}
fn recorded_decision(conn: &Connection, round_id: &str) -> Result<Option<Decision>, DomainError> {
    conn.query_row(
        "SELECT decision FROM decisions WHERE round_id = ?1",
        params![round_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(db_error)?
    .map(decision)
    .transpose()
}
/// A repeat submit after a successful capture has no working-tree changes, so
/// Git reports the saved review commit as both its base and head. The pinned
/// heads still identify precisely the same immutable source and must focus
/// the existing active round rather than create a duplicate.
fn same_pinned_source(old: &WorkspaceManifest, new: &WorkspaceManifest) -> bool {
    old.workspace_id == new.workspace_id
        && old.topic == new.topic
        && old.repositories.len() == new.repositories.len()
        && old.repositories.iter().all(|old_repo| {
            new.repositories.iter().any(|new_repo| {
                old_repo.repository_id == new_repo.repository_id
                    && old_repo.head_sha == new_repo.head_sha
                    && old_repo.object_checksum == new_repo.object_checksum
            })
        })
}
fn ensure_mutable(round: &Round) -> Result<(), DomainError> {
    if round.superseded_by.is_some() {
        return Err(DomainError::actionable(
            "This round is superseded and read-only.",
            "Its review history is retained unchanged.",
            "Open the active successor for new review work.",
            "round_read_only",
        ));
    }
    if !round.lifecycle.active() {
        return Err(DomainError::actionable(
            "This round is completed and read-only.",
            "Its review history is retained unchanged.",
            "Requeue it to review again.",
            "round_read_only",
        ));
    }
    Ok(())
}

fn require_adapter(
    round: &Round,
    adapter_id: &str,
    capability: SourceCapability,
    action: &str,
) -> Result<(), DomainError> {
    if round.source_adapter.adapter_id != adapter_id {
        return Err(DomainError::actionable(
            format!("This review source cannot provide {action}."),
            "No review state or source data was changed.",
            "Open a round from the matching source.",
            "source_adapter_required",
        ));
    }
    round.source_adapter.require(capability, action)
}

fn validate_source_adapter_binding(
    submission: &Submission,
    source_adapter: &SourceAdapterContract,
) -> Result<(), DomainError> {
    let (expected_adapter, required_capabilities) = match &submission.source_metadata {
        Some(crate::SourceMetadata::Github { .. }) => (
            "github_pull_request_mirror",
            [
                SourceCapability::Publish,
                SourceCapability::UpstreamDiscussion,
                SourceCapability::RemoteRefresh,
            ]
            .as_slice(),
        ),
        Some(crate::SourceMetadata::Machine { .. }) => (
            "connected_daemon_workspace",
            [SourceCapability::RemoteRefresh].as_slice(),
        ),
        // Queue intake persists source metadata in a following atomic store
        // operation for historical clients. Keep that protocol compatible,
        // while still resolving the adapter contract immediately.
        None => match submission.collection {
            Collection::Local => (
                "local_workspace_snapshot",
                [SourceCapability::OriginatingAgent].as_slice(),
            ),
            Collection::Github => (
                "github_pull_request_mirror",
                [SourceCapability::UpstreamDiscussion].as_slice(),
            ),
            Collection::Machine => (
                "connected_daemon_workspace",
                [SourceCapability::RemoteRefresh].as_slice(),
            ),
        },
    };
    if source_adapter.adapter_id != expected_adapter {
        return Err(DomainError::actionable(
            "The source adapter does not match this review source.",
            "No queue item was created.",
            "Use the adapter declared by the source and retry.",
            "source_adapter_mismatch",
        ));
    }
    for capability in required_capabilities {
        source_adapter.require(*capability, "this source operation")?;
    }
    Ok(())
}

fn round_from_row(row: &rusqlite::Row<'_>) -> Result<Round, rusqlite::Error> {
    // Deserialization has an actionable wrapper at public boundaries. SQLite's
    // conversion error is used here only to satisfy query_row's callback type.
    let read = || -> Result<Round, DomainError> {
        let collection = collection(row.get(1).map_err(db_error)?)?;
        Ok(Round {
            id: row.get(0).map_err(db_error)?,
            collection,
            topic_identity: row.get(2).map_err(db_error)?,
            manifest_hash: row.get(3).map_err(db_error)?,
            brief: parse(row.get(4).map_err(db_error)?)?,
            manifest: parse(row.get(5).map_err(db_error)?)?,
            rank: row.get(6).map_err(db_error)?,
            lifecycle: lifecycle(row.get(7).map_err(db_error)?)?,
            superseded_by: row.get(8).map_err(db_error)?,
            created_at: parse_time(row.get(9).map_err(db_error)?)?,
            origin_route_id: row.get(10).map_err(db_error)?,
            source_metadata: row
                .get::<_, Option<String>>(11)
                .map_err(db_error)?
                .map(parse)
                .transpose()?,
            origin_route: row
                .get::<_, Option<String>>(12)
                .map_err(db_error)?
                .map(parse)
                .transpose()?,
            source_adapter: row
                .get::<_, Option<String>>(13)
                .map_err(db_error)?
                .map(parse)
                .transpose()?
                .unwrap_or_else(|| SourceAdapterContract::legacy_for_collection(collection)),
        })
    };
    read().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn find_active_by_topic(
    tx: &Transaction<'_>,
    collection_: Collection,
    topic: &str,
) -> Result<Option<Round>, DomainError> {
    tx.query_row("SELECT * FROM rounds WHERE collection = ?1 AND topic_identity = ?2 AND lifecycle != 'completed' AND superseded_by IS NULL ORDER BY created_at DESC LIMIT 1", params![collection_.as_str(), topic], round_from_row).optional().map_err(db_error)
}
fn next_rank(tx: &Transaction<'_>, collection_: Collection) -> Result<i64, DomainError> {
    tx.query_row("SELECT COALESCE(MAX(rank), -1) + 1 FROM rounds WHERE collection = ?1 AND lifecycle != 'completed' AND superseded_by IS NULL", params![collection_.as_str()], |r| r.get(0)).map_err(db_error)
}
fn insert_round(tx: &Transaction<'_>, r: &Round) -> Result<(), DomainError> {
    tx.execute("INSERT INTO rounds(id,collection,topic_identity,manifest_hash,brief_json,manifest_json,rank,lifecycle,superseded_by,created_at,origin_route_id,source_metadata_json,origin_route_json,source_adapter_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", params![r.id,r.collection.as_str(),r.topic_identity,r.manifest_hash,json(&r.brief)?,json(&r.manifest)?,r.rank,r.lifecycle.as_str(),r.superseded_by,r.created_at.to_rfc3339(),r.origin_route_id,r.source_metadata.as_ref().map(json).transpose()?,r.origin_route.as_ref().map(json).transpose()?,json(&r.source_adapter)?]).map_err(db_error)?;
    Ok(())
}
fn upsert_route_tx(tx: &Transaction<'_>, route: &AgentRoute) -> Result<(), DomainError> {
    tx.execute("INSERT INTO routes(id,adapter_kind,agent_id,endpoint,session_id,status,last_heartbeat,provenance_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET adapter_kind=excluded.adapter_kind,agent_id=excluded.agent_id,status=excluded.status,last_heartbeat=excluded.last_heartbeat,endpoint=excluded.endpoint,session_id=excluded.session_id,provenance_json=COALESCE(excluded.provenance_json,routes.provenance_json)", params![route.id,route.adapter_kind,route.agent_id,route.endpoint,route.session_id,route.status,route.last_heartbeat.to_rfc3339(),route.provenance.as_ref().map(json).transpose()?]).map_err(db_error)?;
    Ok(())
}
fn upsert_route(conn: &Connection, route: &AgentRoute) -> Result<(), DomainError> {
    conn.execute("INSERT INTO routes(id,adapter_kind,agent_id,endpoint,session_id,status,last_heartbeat,provenance_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET adapter_kind=excluded.adapter_kind,agent_id=excluded.agent_id,status=excluded.status,last_heartbeat=excluded.last_heartbeat,endpoint=excluded.endpoint,session_id=excluded.session_id,provenance_json=COALESCE(excluded.provenance_json,routes.provenance_json)", params![route.id,route.adapter_kind,route.agent_id,route.endpoint,route.session_id,route.status,route.last_heartbeat.to_rfc3339(),route.provenance.as_ref().map(json).transpose()?]).map_err(db_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{AskTurn, SessionOptionKind};
    use crate::capture;
    use crate::{AgentLastTurnMetadata, AgentRouteProvenance, WorkspaceManifest};
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::{fs, path::Path, process::Command};

    fn run_git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim_end().into()
    }

    fn local_capture_fixture() -> (tempfile::TempDir, std::path::PathBuf, CaptureRequest) {
        let workspace = tempfile::tempdir().unwrap();
        let repository = workspace.path().join("app");
        fs::create_dir_all(&repository).unwrap();
        run_git(&repository, &["init", "-q"]);
        run_git(
            &repository,
            &["config", "user.email", "review@example.test"],
        );
        run_git(&repository, &["config", "user.name", "Review Test"]);
        fs::write(repository.join("tracked.txt"), "initial\n").unwrap();
        run_git(&repository, &["add", "tracked.txt"]);
        run_git(&repository, &["commit", "-qm", "initial"]);
        fs::write(repository.join("tracked.txt"), "staged\n").unwrap();
        run_git(&repository, &["add", "tracked.txt"]);
        fs::write(repository.join("tracked.txt"), "staged and unstaged\n").unwrap();
        fs::write(repository.join("untracked.txt"), "new\n").unwrap();
        let mut request = CaptureRequest {
            workspace_root: workspace.path().into(),
            topic: "atomic".into(),
            brief: ReviewBrief {
                title: "Atomic capture".into(),
                what: "Capture all selected changes.".into(),
                why: "Prevent orphan snapshot commits.".into(),
                approach_alternatives: "Use a guarded transaction.".into(),
                testing: "Inject persistence and index failures.".into(),
            },
            origin_route_id: None,
            participating_repository_ids: Vec::new(),
            preflight_token: None,
        };
        let preflight = capture::preflight(&request).unwrap();
        request.participating_repository_ids = preflight.participating_repository_ids;
        request.preflight_token = Some(preflight.preflight_token);
        (workspace, repository, request)
    }

    fn repository_state(repository: &Path) -> (String, String, Vec<u8>) {
        (
            run_git(repository, &["rev-parse", "HEAD"]),
            run_git(
                repository,
                &["status", "--porcelain=v1", "--untracked-files=all"],
            ),
            fs::read(repository.join(".git/index")).unwrap(),
        )
    }
    fn submission(topic: &str, head: &str) -> Submission {
        Submission {
            collection: Collection::Local,
            topic_identity: format!("workspace:{topic}"),
            brief: ReviewBrief {
                title: "Parser cleanup".into(),
                what: String::new(),
                why: String::new(),
                approach_alternatives: String::new(),
                testing: String::new(),
            },
            manifest: WorkspaceManifest {
                workspace_id: "workspace".into(),
                workspace_root: "/work".into(),
                topic: topic.into(),
                repositories: vec![crate::RepositorySnapshot {
                    repository_id: "app".into(),
                    root: "app".into(),
                    branch: "main".into(),
                    base_sha: "base".into(),
                    head_sha: head.into(),
                    remote_fingerprint: None,
                    object_checksum: String::new(),
                    capture_metadata: None,
                }],
                before_fingerprint: "before".into(),
                after_fingerprint: "after".into(),
                created_at: Utc::now(),
            },
            origin_route: None,
            source_metadata: None,
            source_adapter: None,
        }
    }

    #[test]
    fn local_capture_database_failure_restores_ref_and_exact_index() {
        let (_workspace, repository, request) = local_capture_fixture();
        let before = repository_state(&repository);
        let mut store = Store::in_memory().unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER inject_round_insert_failure
                 BEFORE INSERT ON rounds
                 BEGIN SELECT RAISE(FAIL, 'injected database failure'); END;",
            )
            .unwrap();

        let error = store.ingest_local_capture(&request).unwrap_err();
        assert_eq!(error.error.code, "database_error");
        assert_eq!(repository_state(&repository), before);
        assert!(
            store
                .list(Some(Collection::Local), true)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn local_capture_finalize_failure_rolls_back_database_ref_and_exact_index() {
        let (_workspace, repository, request) = local_capture_fixture();
        let before = repository_state(&repository);
        fs::write(repository.join(".git/index.lock"), "held").unwrap();
        let mut store = Store::in_memory().unwrap();

        let error = store.ingest_local_capture(&request).unwrap_err();
        assert_eq!(error.error.code, "submission_index_finalize_failed");
        assert_eq!(repository_state(&repository), before);
        assert!(
            store
                .list(Some(Collection::Local), true)
                .unwrap()
                .is_empty()
        );
        fs::remove_file(repository.join(".git/index.lock")).unwrap();
    }

    #[test]
    fn local_capture_requires_selected_repositories_and_exact_preflight() {
        let (workspace, _repository, mut request) = local_capture_fixture();
        let mut store = Store::in_memory().unwrap();
        request.participating_repository_ids.clear();
        assert_eq!(
            store.ingest_local_capture(&request).unwrap_err().error.code,
            "repository_selection_required"
        );
        let preflight = capture::preflight(&CaptureRequest {
            workspace_root: workspace.path().into(),
            topic: request.topic.clone(),
            brief: request.brief.clone(),
            origin_route_id: None,
            participating_repository_ids: Vec::new(),
            preflight_token: None,
        })
        .unwrap();
        request.participating_repository_ids = preflight.participating_repository_ids;
        request.preflight_token = Some(preflight.preflight_token);
        request.brief.testing.push_str(" changed");
        assert_eq!(
            store.ingest_local_capture(&request).unwrap_err().error.code,
            "preflight_stale"
        );
    }

    #[test]
    fn local_capture_binds_and_freezes_registered_route_provenance() {
        let (_workspace, repository, mut request) = local_capture_fixture();
        let database_directory = tempfile::tempdir().unwrap();
        let database = database_directory.path().join("route-snapshot.sqlite3");
        let mut store = Store::open(&database).unwrap();
        let route = AgentRoute {
            id: "route-capture".into(),
            adapter_kind: "acp".into(),
            agent_id: "agent-capture".into(),
            endpoint: Some("127.0.0.1:4777".into()),
            session_id: Some("session-capture".into()),
            status: "busy".into(),
            last_heartbeat: Utc::now(),
            provenance: Some(Box::new(AgentRouteProvenance {
                schema_version: Some(1),
                adapter_version: Some("1.4.0".into()),
                provider: Some("copilot-cli".into()),
                provider_version: Some("0.0.350".into()),
                machine_id: Some("local-mac".into()),
                original_cwd: Some(repository.to_string_lossy().into_owned()),
                cmux_workspace: Some("review-workspace".into()),
                cmux_surface: Some("surface-7".into()),
                reconnect_recipe: Some("Resume the saved agent session.".into()),
                provider_resume_handle: Some("resume-42".into()),
                transcript_reference: Some("transcript-42".into()),
                mode: Some("code".into()),
                model: Some("gpt-5.6".into()),
                thinking: Some("high".into()),
                context: Some("originating review task".into()),
                last_turn: None,
            })),
        };
        store.register_route(&route).unwrap();

        let preflight = store.preflight_local_capture(&request).unwrap();
        assert_eq!(
            preflight.origin_route_id.as_deref(),
            Some(route.id.as_str())
        );
        request.origin_route_id = preflight.origin_route_id;
        request.participating_repository_ids = preflight.participating_repository_ids;
        request.preflight_token = Some(preflight.preflight_token);
        let round = match store.ingest_local_capture(&request).unwrap() {
            SubmissionResult::Created(round) => round,
            result => panic!("expected created round, got {result:?}"),
        };
        assert_eq!(round.origin_route_id.as_deref(), Some(route.id.as_str()));
        assert_eq!(round.origin_route.as_ref(), Some(&route));
        assert_eq!(
            store.round(&round.id).unwrap().origin_route,
            Some(route.clone())
        );

        store.heartbeat(&route.id, "idle").unwrap();
        let mut changed = route.clone();
        changed.provenance.as_mut().unwrap().model = Some("different-model".into());
        store.register_route(&changed).unwrap();
        assert_eq!(
            store.route(&route.id).unwrap().provenance,
            changed.provenance
        );
        assert_eq!(
            store.round(&round.id).unwrap().origin_route,
            Some(route.clone()),
            "the capture-point snapshot must not follow mutable route updates"
        );
        drop(store);
        let reopened = Store::open(&database).unwrap();
        assert_eq!(
            reopened.round(&round.id).unwrap().origin_route,
            Some(route),
            "the immutable route snapshot must survive restart"
        );
    }

    #[test]
    fn unknown_explicit_route_is_rejected_before_git_or_sqlite_mutation() {
        let (_workspace, repository, mut request) = local_capture_fixture();
        let before = repository_state(&repository);
        let mut store = Store::in_memory().unwrap();
        request.origin_route_id = Some("missing-route".into());

        let error = store.ingest_local_capture(&request).unwrap_err();
        assert_eq!(error.error.code, "route_not_found");
        assert_eq!(repository_state(&repository), before);
        assert!(
            store
                .list(Some(Collection::Local), true)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn arrivals_join_back_and_updates_keep_rank() {
        let mut store = Store::in_memory().unwrap();
        let first = match store.submit(submission("one", "a")).unwrap() {
            SubmissionResult::Created(r) => r,
            _ => unreachable!(),
        };
        let second = match store.submit(submission("two", "b")).unwrap() {
            SubmissionResult::Created(r) => r,
            _ => unreachable!(),
        };
        assert_eq!((first.rank, second.rank), (0, 1));
        let new = match store.submit(submission("one", "c")).unwrap() {
            SubmissionResult::Superseded { round, .. } => round,
            _ => unreachable!(),
        };
        assert_eq!(new.rank, 0);
        assert_eq!(
            store.round(&first.id).unwrap().lifecycle,
            Lifecycle::Completed
        );
    }

    #[test]
    fn resolved_source_adapter_persists_with_a_round() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut store = Store::open(temp.path()).unwrap();
        let round = match store.submit(submission("adapter", "a")).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        assert_eq!(round.source_adapter.adapter_id, "local_workspace_snapshot");
        drop(store);

        let reopened = Store::open(temp.path()).unwrap();
        let persisted = reopened.round(&round.id).unwrap();
        assert_eq!(
            persisted.source_adapter.adapter_id,
            "local_workspace_snapshot"
        );
        assert_eq!(
            persisted.source_adapter.approval,
            ApprovalDisposition::PurgeRound
        );
    }

    #[test]
    fn adapter_declaration_not_collection_controls_approval_behavior() {
        let mut input = submission("adapter-approval", "a");
        input.collection = Collection::Github;
        input.source_metadata = Some(crate::SourceMetadata::Github {
            host: "github.com".into(),
            owner: "octo".into(),
            repository: "queue".into(),
            pull_number: 1,
            base_sha: "base".into(),
            head_sha: "head".into(),
            state: crate::adapters::GithubPullRequestState::Open,
            is_draft: false,
            staleness: None,
        });
        let mut adapter = SourceAdapterContract::legacy_for_collection(Collection::Github);
        adapter.approval = ApprovalDisposition::PurgeRound;
        input.source_adapter = Some(adapter);

        let mut store = Store::in_memory().unwrap();
        let round = match store.submit(input).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        assert_eq!(
            store.approve_remote(&round.id).unwrap_err().error.code,
            "local_approval_requires_confirmation"
        );
        store.approve_local(&round.id).unwrap();
    }

    #[test]
    fn connected_machine_config_uses_shared_validation_and_is_idempotent() {
        let store = Store::in_memory().unwrap();
        let config = MachineConfig {
            name: "  Build Mac  ".into(),
            endpoint: MachineEndpoint::Ssh {
                target: "review-build".into(),
                remote_socket: "/tmp/review-queue-daemon.sock".into(),
                adapter: SshAdapter::SystemOpenSsh,
            },
            source_type: MachineSourceType::ReviewQueueDaemon,
        };
        let (created, was_created) = store.add_machine_config(&config).unwrap();
        assert!(was_created);
        assert_eq!(created.config.name, "Build Mac");
        let (existing, was_created) = store
            .add_machine_config(&created.config)
            .expect("identical configuration is an idempotent success");
        assert!(!was_created);
        assert_eq!(created.id, existing.id);
        assert_eq!(store.machine_configs().unwrap(), vec![created]);

        let mut invalid = config;
        if let MachineEndpoint::Ssh { target, .. } = &mut invalid.endpoint {
            *target = "-oIdentityFile=/tmp/key".into();
        }
        assert_eq!(
            store.add_machine_config(&invalid).unwrap_err().error.code,
            "machine_ssh_target_invalid"
        );
    }

    #[test]
    fn machine_origin_metadata_round_trips_with_the_round() {
        let mut store = Store::in_memory().unwrap();
        let mut input = submission("remote-topic", "remote-head");
        input.collection = Collection::Machine;
        input.topic_identity = "machine-1:workspace:remote-topic".into();
        input.source_metadata = Some(crate::SourceMetadata::Machine {
            machine_id: "machine-1".into(),
            machine_name: "Build Mac".into(),
            source_item_id: "remote-round-1".into(),
            remote_workspace_id: "workspace".into(),
            remote_workspace_path: "/remote/project".into(),
            cursor: "cursor-1".into(),
            cached_at: Utc::now(),
        });
        let round = match store.submit(input).unwrap() {
            SubmissionResult::Created(round) => round,
            other => panic!("machine round should be created, got {other:?}"),
        };
        assert_eq!(
            store.round(&round.id).unwrap().source_metadata,
            round.source_metadata
        );
        let snapshot = crate::machine::MachineSnapshot {
            source_item_id: "remote-round-1".into(),
            snapshot_version: "snapshot-1".into(),
            manifest: round.manifest.clone(),
            files: vec![crate::machine::MachineSnapshotFile {
                repository_id: "app".into(),
                workspace_relative_path: "src/lib.rs".into(),
                status: "modified".into(),
                base_blob_sha: "base-blob".into(),
                head_blob_sha: "head-blob".into(),
                unified_diff: "-old\n+new".into(),
                is_binary: false,
                base_content_base64: Some("b2xk".into()),
                head_content_base64: Some("bmV3".into()),
                materialized: None,
            }],
            repository_packs: vec![],
        };
        store.save_machine_snapshot(&round.id, &snapshot).unwrap();
        assert_eq!(store.machine_snapshot(&round.id).unwrap(), snapshot);
    }

    #[test]
    fn complete_and_requeue_retains_history_and_moves_to_top() {
        let mut store = Store::in_memory().unwrap();
        let one = match store.submit(submission("one", "a")).unwrap() {
            SubmissionResult::Created(r) => r,
            _ => unreachable!(),
        };
        let two = match store.submit(submission("two", "b")).unwrap() {
            SubmissionResult::Created(r) => r,
            _ => unreachable!(),
        };
        store.complete(&two.id).unwrap();
        store.requeue(&two.id).unwrap();
        assert_eq!(store.round(&two.id).unwrap().rank, 0);
        assert_eq!(store.round(&one.id).unwrap().rank, 1);
    }

    #[test]
    fn purge_only_removes_app_state() {
        let mut store = Store::in_memory().unwrap();
        let round = match store.submit(submission("one", "a")).unwrap() {
            SubmissionResult::Created(r) => r,
            _ => unreachable!(),
        };
        store.purge(&round.id).unwrap();
        assert_eq!(store.list(None, true).unwrap().len(), 0);
    }

    #[test]
    fn same_source_with_a_new_capture_time_is_idempotent() {
        let mut store = Store::in_memory().unwrap();
        store.submit(submission("one", "a")).unwrap();
        let result = store.submit(submission("one", "a")).unwrap();
        assert!(matches!(result, SubmissionResult::Existing(_)));
    }

    #[test]
    fn repeat_submit_of_saved_review_heads_is_idempotent() {
        let mut store = Store::in_memory().unwrap();
        let original = submission("one", "review-commit");
        store.submit(original.clone()).unwrap();
        let mut repeat = original;
        repeat.manifest.repositories[0].base_sha = "review-commit".into();
        repeat.manifest.created_at = Utc::now();
        let result = store.submit(repeat).unwrap();
        assert!(matches!(result, SubmissionResult::Existing(_)));
    }

    #[test]
    fn local_approve_is_not_a_recorded_limbo_state() {
        let mut store = Store::in_memory().unwrap();
        let round = match store.submit(submission("one", "a")).unwrap() {
            SubmissionResult::Created(r) => r,
            _ => unreachable!(),
        };
        assert_eq!(
            store.approve_remote(&round.id).unwrap_err().error.code,
            "local_approval_requires_confirmation"
        );
        assert_eq!(store.round(&round.id).unwrap().lifecycle, Lifecycle::Queued);
    }

    #[test]
    fn superseded_round_cannot_be_requeued() {
        let mut store = Store::in_memory().unwrap();
        let old = match store.submit(submission("one", "a")).unwrap() {
            SubmissionResult::Created(r) => r,
            _ => unreachable!(),
        };
        store.submit(submission("one", "b")).unwrap();
        assert_eq!(
            store.requeue(&old.id).unwrap_err().error.code,
            "superseded_round_read_only"
        );
    }

    #[test]
    fn viewed_state_is_round_local_persistent_and_read_only_for_old_rounds() {
        let mut store = Store::in_memory().unwrap();
        let round = match store.submit(submission("one", "a")).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        store
            .set_file_viewed(&round.id, "app", "src/parser.rs", true)
            .unwrap();
        assert_eq!(
            store.viewed_files(&round.id).unwrap(),
            vec![("app".into(), "src/parser.rs".into())]
        );
        store.complete(&round.id).unwrap();
        assert_eq!(
            store
                .set_file_viewed(&round.id, "app", "src/other.rs", true)
                .unwrap_err()
                .error
                .code,
            "round_read_only"
        );
    }

    #[test]
    fn reorder_clamps_to_active_collection_bounds() {
        let mut store = Store::in_memory().unwrap();
        let first = match store.submit(submission("one", "a")).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        let second = match store.submit(submission("two", "b")).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        store.move_rank(&first.id, 99).unwrap();
        assert_eq!(store.round(&first.id).unwrap().rank, 1);
        assert_eq!(store.round(&second.id).unwrap().rank, 0);
        store.move_rank(&first.id, -20).unwrap();
        assert_eq!(store.round(&first.id).unwrap().rank, 0);
    }

    #[test]
    fn delivery_requires_decision_and_contains_only_undelivered_revisions() {
        let mut store = Store::in_memory().unwrap();
        let mut input = submission("one", "a");
        input.collection = Collection::Github;
        let round = match store.submit(input).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        let comment = store
            .create_formal_comment(&round.id, "round", "Please simplify this.", None)
            .unwrap();
        assert_eq!(
            store.prepare_delivery(&round.id).unwrap_err().error.code,
            "decision_required"
        );
        store.request_changes(&round.id).unwrap();
        let first = store.prepare_delivery(&round.id).unwrap();
        assert_eq!(first.payload.decision, Decision::RequestChanges);
        assert_eq!(first.payload.comments, vec![comment.clone()]);
        store.mark_delivery(&first.id).unwrap();
        assert_eq!(
            store.formal_comments(&round.id).unwrap()[0].delivered_revision,
            Some(1)
        );
        let no_new_comments = store.prepare_delivery(&round.id).unwrap_err();
        assert_eq!(no_new_comments.error.code, "formal_comments_required");

        let revision = store
            .edit_formal_comment(&comment.id, "Please simplify this path.", None)
            .unwrap();
        assert_eq!(revision.revision, 2);
        let second = store.prepare_delivery(&round.id).unwrap();
        assert_eq!(second.payload.comments, vec![revision]);
        assert_ne!(first.idempotency_key, second.idempotency_key);
    }

    #[test]
    fn pending_delivery_is_reused_and_history_records_outcome() {
        let mut store = Store::in_memory().unwrap();
        let mut input = submission("retry", "a");
        input.collection = Collection::Github;
        let round = match store.submit(input).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        store.request_changes(&round.id).unwrap();
        store
            .create_formal_comment(&round.id, "round", "Retry this exact payload.", None)
            .unwrap();

        let first = store.prepare_delivery(&round.id).unwrap();
        let retry = store.prepare_delivery(&round.id).unwrap();
        assert_eq!(retry.id, first.id);
        assert_eq!(retry.idempotency_key, first.idempotency_key);
        assert_eq!(
            store.pending_delivery(&round.id).unwrap().unwrap().id,
            first.id
        );
        let pending_history = store.delivery_history(&round.id).unwrap();
        assert_eq!(pending_history.len(), 1);
        assert!(pending_history[0].delivered_at.is_none());
        assert!(pending_history[0].outcome.is_none());

        store.mark_delivery(&first.id).unwrap();
        assert!(store.pending_delivery(&round.id).unwrap().is_none());
        let delivered = store.delivery_history(&round.id).unwrap();
        assert!(delivered[0].delivered_at.is_some());
        assert_eq!(
            delivered[0].outcome.as_deref(),
            Some("manual_submission_confirmed")
        );
    }

    #[test]
    fn legacy_empty_pending_delivery_is_retired_before_preparing_feedback() {
        let mut store = Store::in_memory().unwrap();
        let mut input = submission("legacy-empty", "a");
        input.collection = Collection::Github;
        let round = match store.submit(input).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        store.request_changes(&round.id).unwrap();
        let empty_payload = DeliveryPayload {
            round_id: round.id.clone(),
            decision: Decision::RequestChanges,
            comments: vec![],
        };
        store
            .conn
            .execute(
                "INSERT INTO deliveries(
                   id,round_id,idempotency_key,payload_json,created_at,delivered_at,outcome
                 ) VALUES('legacy-empty',?1,'legacy-empty-key',?2,?3,NULL,NULL)",
                params![
                    round.id,
                    json(&empty_payload).unwrap(),
                    Utc::now().to_rfc3339()
                ],
            )
            .unwrap();

        assert!(store.pending_delivery(&round.id).unwrap().is_none());
        let history = store.delivery_history(&round.id).unwrap();
        assert_eq!(history[0].outcome.as_deref(), Some("invalid_empty_payload"));

        store
            .create_formal_comment(&round.id, "round", "A valid revision.", None)
            .unwrap();
        let corrected = store.prepare_delivery(&round.id).unwrap();
        assert_eq!(corrected.payload.comments.len(), 1);
        assert_ne!(corrected.id, "legacy-empty");
    }

    #[test]
    fn completed_round_feedback_and_delivery_history_are_read_only_but_visible() {
        let mut store = Store::in_memory().unwrap();
        let round = match store
            .submit(submission("historical-feedback", "a"))
            .unwrap()
        {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        let comment = store
            .create_formal_comment(
                &round.id,
                "round",
                "Preserve this historical feedback.",
                None,
            )
            .unwrap();
        store.request_changes(&round.id).unwrap();
        let delivery = store.prepare_delivery(&round.id).unwrap();
        store.complete(&round.id).unwrap();

        assert_eq!(
            store.formal_comments(&round.id).unwrap(),
            vec![comment.clone()]
        );
        assert_eq!(
            store.delivery_history(&round.id).unwrap()[0].delivery,
            delivery
        );
        for error in [
            store
                .edit_formal_comment(&comment.id, "mutated", None)
                .unwrap_err(),
            store.delete_formal_comment(&comment.id).unwrap_err(),
            store.prepare_delivery(&round.id).unwrap_err(),
            store
                .mark_delivery_manually_submitted(&delivery.id)
                .unwrap_err(),
        ] {
            assert_eq!(error.error.code, "round_read_only");
        }
    }

    #[test]
    fn routes_lifecycle_and_token_free_ingress_are_explicit() {
        let mut store = Store::in_memory().unwrap();
        let round = match store.submit(submission("audit", "a")).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        assert!(store.lifecycle_events(&round.id).unwrap().is_empty());

        let route = AgentRoute {
            id: "route-1".into(),
            adapter_kind: "acp".into(),
            agent_id: "agent-1".into(),
            endpoint: Some("127.0.0.1:4000".into()),
            session_id: Some("session-1".into()),
            status: "idle".into(),
            last_heartbeat: Utc::now(),
            provenance: Some(Box::new(AgentRouteProvenance {
                schema_version: Some(1),
                adapter_version: Some("1.4.0".into()),
                provider: Some("codex".into()),
                provider_version: Some("5.6".into()),
                machine_id: Some("machine-a".into()),
                original_cwd: Some("/work/parser".into()),
                cmux_workspace: Some("review-workspace".into()),
                cmux_surface: Some("surface-7".into()),
                reconnect_recipe: Some("Open the saved workspace and resume the session.".into()),
                provider_resume_handle: Some("resume-42".into()),
                transcript_reference: Some("transcript-42".into()),
                mode: Some("code".into()),
                model: Some("gpt-5.6".into()),
                thinking: Some("high".into()),
                context: Some("review".into()),
                last_turn: Some(AgentLastTurnMetadata {
                    turn_id: Some("turn-9".into()),
                    status: Some("completed".into()),
                    started_at: Some(Utc::now()),
                    completed_at: Some(Utc::now()),
                }),
            })),
        };
        store.register_route(&route).unwrap();
        store.heartbeat(&route.id, "busy").unwrap();
        let persisted = store.route(&route.id).unwrap();
        assert_eq!(persisted.status, "busy");
        assert_eq!(persisted.provenance, route.provenance);
        let mut legacy_update = route.clone();
        legacy_update.status = "idle".into();
        legacy_update.provenance = None;
        store.register_route(&legacy_update).unwrap();
        assert_eq!(store.route(&route.id).unwrap().provenance, route.provenance);
        assert_eq!(store.routes().unwrap().len(), 1);
        assert!(store.lifecycle_events(&round.id).unwrap().is_empty());

        store.request_changes(&round.id).unwrap();
        store.complete(&round.id).unwrap();
        store.requeue(&round.id).unwrap();
        assert_eq!(
            store
                .lifecycle_events(&round.id)
                .unwrap()
                .into_iter()
                .map(|event| event.kind)
                .collect::<Vec<_>>(),
            vec![
                LifecycleEventKind::RequestChanges,
                LifecycleEventKind::Complete,
                LifecycleEventKind::Requeue,
            ]
        );

        let error = store
            .create_formal_comment(
                &round.id,
                "secret",
                "accidentally copied ghp_fixture_secret",
                None,
            )
            .unwrap_err();
        assert_eq!(error.error.code, "token_shaped_ingress");
        assert!(store.formal_comments(&round.id).unwrap().is_empty());
        store
            .create_formal_comment(
                &round.id,
                "prose",
                "Discuss OAuth and bearer authentication without storing credentials.",
                None,
            )
            .unwrap();
        let export = store
            .export_redacted_artifact(&serde_json::json!({"operation": "verify"}))
            .unwrap();
        assert!(!export.contains_raw_values);
        assert_eq!(export.table_row_counts["routes"], 1);
        assert_eq!(
            store
                .export_redacted_artifact(
                    &serde_json::json!({"access_token": "not-even-a-real-token"})
                )
                .unwrap_err()
                .error
                .code,
            "token_shaped_ingress"
        );

        let mut unsafe_route = route.clone();
        unsafe_route.provenance.as_mut().unwrap().reconnect_recipe =
            Some("resume with ghp_1234567890abcdef".into());
        let error = store
            .register_route(&unsafe_route)
            .expect_err("route provenance must remain token-free");
        assert_eq!(error.error.code, "token_shaped_ingress");
    }

    #[test]
    fn legacy_routes_migrate_with_defaulted_provenance() {
        let temporary = tempfile::tempdir().unwrap();
        let database = temporary.path().join("legacy.sqlite3");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE routes (
                   id TEXT PRIMARY KEY, adapter_kind TEXT NOT NULL, agent_id TEXT NOT NULL,
                   endpoint TEXT, session_id TEXT, status TEXT NOT NULL,
                   last_heartbeat TEXT NOT NULL
                 );
                 INSERT INTO routes(
                   id,adapter_kind,agent_id,endpoint,session_id,status,last_heartbeat
                 ) VALUES(
                   'legacy-route','acp','legacy-agent',NULL,NULL,'closed',
                   '2025-01-01T00:00:00Z'
                 );",
            )
            .unwrap();
        drop(connection);

        let store = Store::open(&database).unwrap();
        let route = store.route("legacy-route").unwrap();
        assert_eq!(route.agent_id, "legacy-agent");
        assert_eq!(route.provenance, None);
        assert!(table_has_column(&store.conn, "routes", "provenance_json").unwrap());
        assert!(table_has_column(&store.conn, "rounds", "origin_route_json").unwrap());
    }

    #[test]
    fn purge_event_survives_round_deletion() {
        let mut store = Store::in_memory().unwrap();
        let round = match store.submit(submission("purge-event", "a")).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        store.purge(&round.id).unwrap();
        assert_eq!(
            store.lifecycle_events(&round.id).unwrap()[0].kind,
            LifecycleEventKind::Purge
        );
    }

    #[test]
    fn local_approval_is_a_terminal_audited_app_only_delete() {
        let mut store = Store::in_memory().unwrap();
        let round = match store.submit(submission("approve-local", "a")).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        store.approve_local(&round.id).unwrap();
        assert_eq!(
            store.lifecycle_events(&round.id).unwrap()[0].kind,
            LifecycleEventKind::ApproveLocal
        );
        assert_eq!(
            store.round(&round.id).unwrap_err().error.code,
            "round_not_found"
        );
    }

    #[test]
    fn late_delivery_does_not_mark_a_newer_revision_delivered() {
        let mut store = Store::in_memory().unwrap();
        let mut input = submission("one", "a");
        input.collection = Collection::Github;
        let round = match store.submit(input).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        store.request_changes(&round.id).unwrap();
        let comment = store
            .create_formal_comment(&round.id, "round", "Original feedback", None)
            .unwrap();
        let first = store.prepare_delivery(&round.id).unwrap();
        store
            .edit_formal_comment(&comment.id, "Corrected feedback", None)
            .unwrap();
        store.mark_delivery(&first.id).unwrap();
        let comment = &store.formal_comments(&round.id).unwrap()[0];
        assert_eq!(comment.revision, 2);
        assert_eq!(comment.delivered_revision, Some(1));
        assert_eq!(
            store
                .prepare_delivery(&round.id)
                .unwrap()
                .payload
                .comments
                .len(),
            1
        );
    }

    fn queued_turn(chat: &AskConversation, key: &str) -> AskTurn {
        AskTurn {
            id: Uuid::new_v4().to_string(),
            conversation_id: chat.id.clone(),
            idempotency_key: key.into(),
            prompt: "Explain this diff".into(),
            anchor: None,
            option_values: BTreeMap::from([("model".into(), "reviewer".into())]),
            state: AskTurnState::Queued,
            created_at: Utc::now(),
            completed_at: None,
            failure_reason: None,
            response_text: String::new(),
        }
    }

    #[test]
    fn ask_turns_are_queued_once_streamed_durably_and_never_replayed() {
        let mut store = Store::in_memory().unwrap();
        let round = match store.submit(submission("ask", "a")).unwrap() {
            SubmissionResult::Created(r) => r,
            _ => unreachable!(),
        };
        let chat = store.active_conversation(&round.id, vec![]).unwrap();
        let turn = queued_turn(&chat, "request-1");
        let saved = store.queue_ask_turn(turn.clone()).unwrap();
        assert_eq!(saved.id, turn.id);
        // A retry gets the already persisted turn and cannot start a second provider call.
        assert_eq!(store.queue_ask_turn(turn).unwrap().id, saved.id);
        store.begin_ask_turn(&saved.id).unwrap();
        store.append_ask_chunk(&saved.id, "First ").unwrap();
        let done = store.append_ask_chunk(&saved.id, "answer").unwrap();
        assert_eq!(done.response_text, "First answer");
        assert_eq!(done.option_values["model"], "reviewer");
        assert_eq!(
            store.complete_ask_turn(&saved.id).unwrap().state,
            AskTurnState::Completed
        );
        assert_eq!(
            store.begin_ask_turn(&saved.id).unwrap_err().error.code,
            "invalid_ask_turn_transition"
        );
    }

    #[test]
    fn restart_retires_only_conversations_with_started_provider_sessions() {
        let mut store = Store::in_memory().unwrap();
        let untouched_round = match store.submit(submission("untouched-chat", "a")).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        let started_round = match store.submit(submission("started-chat", "b")).unwrap() {
            SubmissionResult::Created(round) => round,
            _ => unreachable!(),
        };
        let untouched = store
            .active_conversation(&untouched_round.id, vec![])
            .unwrap();
        let started = store
            .active_conversation(&started_round.id, vec![])
            .unwrap();
        let options = vec![DiscoveredSessionOption {
            key: "model".into(),
            label: "Model".into(),
            kind: SessionOptionKind::Select,
            values: vec!["reviewer".into(), "fast".into()],
            selected: Some("reviewer".into()),
            supported: true,
            unavailable_reason: None,
        }];
        let marked = store
            .mark_conversation_provider_started(&started.id, "provider-session-7", &options)
            .unwrap();
        assert_eq!(
            marked.provider_session_label.as_deref(),
            Some("provider-session-7")
        );
        assert_eq!(marked.options, options);
        let changed = store
            .update_conversation_option_selection(&started.id, "model", "fast")
            .unwrap();
        assert_eq!(changed.options[0].selected.as_deref(), Some("fast"));

        store.migrate().unwrap();

        let untouched = store.conversation(&untouched.id).unwrap();
        assert_eq!(
            untouched.session_state,
            ConversationSessionState::CanContinue
        );
        assert_eq!(untouched.provider_session_label, None);
        let started = store.conversation(&started.id).unwrap();
        assert_eq!(started.session_state, ConversationSessionState::HistoryOnly);
        assert_eq!(started.provider_session_label, None);
        assert_eq!(started.options[0].selected.as_deref(), Some("fast"));

        let error = store
            .mark_conversation_provider_started(&untouched.id, "ghp_1234567890abcdef", &[])
            .unwrap_err();
        assert_eq!(error.error.code, "token_shaped_ingress");
    }

    #[test]
    fn clear_archives_history_and_reuses_options_without_prompting() {
        let mut store = Store::in_memory().unwrap();
        let round = match store.submit(submission("clear", "a")).unwrap() {
            SubmissionResult::Created(r) => r,
            _ => unreachable!(),
        };
        let chat = store.active_conversation(&round.id, vec![]).unwrap();
        store.queue_ask_turn(queued_turn(&chat, "old")).unwrap();
        let new_chat = store.clear_conversation(&round.id).unwrap();
        assert_ne!(chat.id, new_chat.id);
        assert!(store.ask_turns(&new_chat.id).unwrap().is_empty());
        let history = store.conversation_history(&round.id).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(
            history[0].session_state,
            ConversationSessionState::HistoryOnly
        );
        assert!(history[0].archived_at.is_some());
        assert_eq!(
            store
                .queue_ask_turn(queued_turn(&history[0], "nope"))
                .unwrap_err()
                .error
                .code,
            "history_only_conversation"
        );
    }

    #[test]
    fn completed_or_superseded_round_rejects_new_prompts() {
        let mut store = Store::in_memory().unwrap();
        let round = match store.submit(submission("old", "a")).unwrap() {
            SubmissionResult::Created(r) => r,
            _ => unreachable!(),
        };
        let chat = store.active_conversation(&round.id, vec![]).unwrap();
        store.complete(&round.id).unwrap();
        assert_eq!(
            store
                .queue_ask_turn(queued_turn(&chat, "closed"))
                .unwrap_err()
                .error
                .code,
            "round_read_only"
        );
    }

    #[test]
    fn startup_interrupts_unfinished_turn_without_replaying_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review-queue.sqlite3");
        let (round_id, chat_id, turn_id) = {
            let mut store = Store::open(&path).unwrap();
            let round = match store.submit(submission("restart", "a")).unwrap() {
                SubmissionResult::Created(round) => round,
                _ => unreachable!(),
            };
            let chat = store.active_conversation(&round.id, vec![]).unwrap();
            store
                .mark_conversation_provider_started(&chat.id, "provider-session-restart", &[])
                .unwrap();
            let turn = store
                .queue_ask_turn(queued_turn(&chat, "restart-turn"))
                .unwrap();
            store.begin_ask_turn(&turn.id).unwrap();
            store.append_ask_chunk(&turn.id, "partial").unwrap();
            (round.id, chat.id, turn.id)
        };
        let store = Store::open(&path).unwrap();
        let turn = store.ask_turns(&chat_id).unwrap().pop().unwrap();
        assert_eq!(turn.id, turn_id);
        assert_eq!(turn.state, AskTurnState::Interrupted);
        assert_eq!(turn.response_text, "partial");
        assert!(turn.failure_reason.unwrap().contains("restarted"));
        let conversation = store.current_conversation(&round_id).unwrap().unwrap();
        assert_eq!(
            conversation.session_state,
            ConversationSessionState::HistoryOnly
        );
        assert!(
            conversation
                .history_only_reason
                .as_deref()
                .unwrap()
                .contains("not resumed")
        );
        assert_eq!(store.ask_turns(&conversation.id).unwrap().len(), 1);
    }
}
