export type Collection = "local" | "github" | "machine";
export type Lifecycle = "queued" | "changes_requested" | "completed";

export interface UpdateCheck {
  available: boolean;
  currentVersion: string;
  version?: string | null;
  date?: string | null;
  notes?: string | null;
}

export interface UpdateInstall {
  installed: boolean;
  version: string;
  relaunchRequired: boolean;
}

export interface DiagnosticsExport {
  path: string;
  artifact: {
    schema_version: number;
    verified_at: string;
    table_row_counts: Record<string, number>;
    contains_raw_values: false;
  };
}

export interface ReviewBrief {
  title: string;
  what: string;
  why: string;
  approach_alternatives: string;
  testing: string;
}

export interface RepositorySnapshot {
  repository_id: string;
  root: string;
  branch: string;
  base_sha: string;
  head_sha: string;
  remote_fingerprint?: string | null;
  object_checksum: string;
}

export interface WorkspaceManifest {
  workspace_id: string;
  workspace_root: string;
  topic: string;
  repositories: RepositorySnapshot[];
  before_fingerprint: string;
  after_fingerprint: string;
  created_at: string;
  origin_route_id?: string | null;
}

export interface ReviewRound {
  id: string;
  collection: Collection;
  topic_identity: string;
  manifest_hash: string;
  brief: ReviewBrief;
  manifest: WorkspaceManifest;
  rank: number;
  lifecycle: Lifecycle;
  superseded_by?: string | null;
  created_at: string;
  origin_route_id?: string | null;
  origin_route?: AgentRoute | null;
  source_metadata?:
    | {
        kind: "machine";
        machine_id: string;
        machine_name: string;
        source_item_id: string;
        remote_workspace_id: string;
        remote_workspace_path: string;
        cursor: string;
        cached_at: string;
      }
    | {
        kind: "github";
        [key: string]: unknown;
      }
    | null;
}

export type MachineEndpoint =
  | { kind: "loopback"; socket_path: string }
  | {
      kind: "ssh";
      target: string;
      remote_socket: string;
      adapter: "system_open_ssh";
    };

export interface MachineRecord {
  id: string;
  config: {
    name: string;
    endpoint: MachineEndpoint;
    source_type: "review_queue_daemon";
  };
}

export interface CacheFreshness {
  cached: boolean;
  cursor?: { version: string } | null;
  cached_at?: string | null;
  age_seconds?: number | null;
}

export interface MachineStatus {
  machine: MachineRecord;
  connection: "connected" | "disconnected" | "unreachable";
  health?: {
    protocol_version: number;
    daemon_version: string;
    state: "healthy" | "degraded";
    cursor: { version: string };
  } | null;
  cachedItemCount: number;
  freshness: CacheFreshness;
  lastError?: CommandError | null;
}

export interface MachineItemSummary {
  source_item_id: string;
  remote_workspace_id: string;
  remote_workspace_path: string;
  topic_key: string;
  title: string;
  manifest_hash: string;
  snapshot_version: string;
}

export interface MachineIndexResult {
  index: {
    cursor: { version: string };
    items: MachineItemSummary[];
  };
  freshness: CacheFreshness;
}

export interface GithubPullRequestMetadata {
  host: string;
  owner: string;
  repository: string;
  pull_number: number;
  title: string;
  body: string;
  base_sha: string;
  head_sha: string;
  state: "open" | "closed" | "merged";
  is_draft: boolean;
  web_url?: string | null;
}

export interface GithubMaterializedFile {
  path: string;
  status: string;
  base_blob_sha: string;
  head_blob_sha: string;
  is_binary: boolean;
  base_content?: string | null;
  head_content?: string | null;
  base_content_base64?: string | null;
  head_content_base64?: string | null;
  unified_diff: string;
}

export interface GithubOpenedPullRequest {
  payload: {
    locator: { host: string; owner: string; repository: string; pull_number: number };
    metadata: GithubPullRequestMetadata;
    source_materialized: boolean;
  };
  files: GithubMaterializedFile[];
}

export interface ImportedComment {
  id: string;
  thread_id: string;
  body: string;
  upstream_author: string;
  upstream_created_at: string;
  source_url: string;
  kind: "unknown" | "review_thread_comment" | "pull_request_comment" | "review_summary";
  upstream_resolved?: boolean | null;
  upstream_review_state?: string | null;
  anchor?: Anchor | null;
}

export interface GithubStalenessStatus {
  pinned_head_sha: string;
  observed_head_sha: string;
  checked_at: string;
}

export interface GithubCommentRefreshResult {
  imported: ImportedComment[];
  staleness: GithubStalenessStatus;
}

export interface GithubPublishAttempt {
  id: string;
  round_id: string;
  preview: {
    target: GithubPullRequestMetadata;
    decision: "approve" | "request_changes";
    event: "approve" | "request_changes";
    comments: Array<{
      formal_comment_id: string;
      disposition: "inline" | "reply_to_imported_thread" | "review_body" | "body_fallback";
      fallback_reference?: string | null;
    }>;
  };
  request: {
    idempotency_key: string;
    target: GithubPullRequestMetadata;
    decision: "approve" | "request_changes";
    event: "approve" | "request_changes";
    comments: Array<{
      formal_comment_id: string;
      thread_id: string;
      body: string;
      disposition: "inline" | "reply_to_imported_thread" | "review_body" | "body_fallback";
      fallback_reference?: string | null;
      anchor?: Anchor | null;
    }>;
  };
  replies: Array<{
    id: string;
    round_id: string;
    request: {
      idempotency_key: string;
      target: GithubPullRequestMetadata;
      formal_comment_id: string;
      formal_revision: number;
      upstream_comment_id: number;
      body: string;
    };
    status: "prepared" | "posting" | "completed" | "unknown";
    comment_id?: string | null;
    created_at: string;
    completed_at?: string | null;
  }>;
  status: "prepared" | "posting" | "completed" | "unknown";
  review_id?: string | null;
  created_at: string;
  completed_at?: string | null;
}

export interface CopilotCapabilityGroup {
  key: string;
  label: string;
  supported: boolean;
  unsupported_reason?: string | null;
  apply_policy: "applies_now" | "requires_fresh_session";
  choices: Array<{ value: string; label: string }>;
  selected?: string | null;
}

export interface CopilotCapabilities {
  supported: boolean;
  unsupported_reason?: string | null;
  option_groups: CopilotCapabilityGroup[];
}

export interface CopilotSessionInfo {
  sessionId: string;
  authSource: "existing_cli_sign_in_read_only" | "app_owned_oauth";
  account?: string | null;
  capabilities: CopilotCapabilities;
  activeOptions: Record<string, string>;
}

export interface CopilotPollResult {
  turn: AskTurn;
  update:
    | { state: "chunk"; prompt_id: string; sequence: number; text: string }
    | { state: "completed"; prompt_id: string }
    | { state: "failed"; prompt_id: string; error: CommandError };
}

export interface CommandError {
  code: string;
  message: string;
  data_safety: string;
  next_step: string;
}

export interface LocalSubmissionRequest {
  workspacePath: string;
  topic: string;
  brief: ReviewBrief;
  originRouteId?: string | null;
  participatingRepositoryIds: string[];
  preflightToken?: string | null;
}

export interface SubmissionOutcome {
  outcome: "created" | "existing" | "superseded";
  round: ReviewRound;
  supersededRoundId?: string;
}

export interface LocalPreflightRepository {
  root: string;
  repositoryId: string;
  branch: string;
  headSha: string;
  status: string;
  hasChanges: boolean;
  participating: boolean;
}

export interface LocalPreflight {
  repositories: LocalPreflightRepository[];
  beforeFingerprint: string;
  participatingRepositoryIds: string[];
  originRouteId?: string | null;
  preflightToken: string;
}

export type DiffFileStatus = "added" | "deleted" | "modified";
export type DiffLineKind = "context" | "addition" | "deletion";

export interface DiffLine {
  type: DiffLineKind;
  content: string;
}

export interface DiffHunk {
  repository_id: string;
  old_start: number;
  old_lines: number;
  new_start: number;
  new_lines: number;
  header: string;
  lines: DiffLine[];
}

export interface DiffFile {
  repository_id: string;
  old_path?: string | null;
  new_path?: string | null;
  old_blob_sha?: string | null;
  new_blob_sha?: string | null;
  status: DiffFileStatus;
  is_binary: boolean;
  patch: string;
  hunks: DiffHunk[];
}

export interface RepositoryDiff {
  repository_id: string;
  root: string;
  base_sha: string;
  head_sha: string;
  files: DiffFile[];
}

export interface MaterializedDiff {
  repositories: RepositoryDiff[];
}

export interface PinnedFileContent {
  repository_id: string;
  path: string;
  side: "LEFT" | "RIGHT";
  blob_sha: string;
  is_binary: boolean;
  content?: string | null;
}

export interface ViewedFile {
  repositoryId: string;
  path: string;
}

export interface Anchor {
  repository_id: string;
  workspace_relative_path: string;
  side: string;
  start_line: number;
  end_line: number;
  blob_sha: string;
  selected_code: string;
}

export interface FormalComment {
  id: string;
  thread_id: string;
  body: string;
  anchor?: Anchor | null;
  revision: number;
  delivered_revision?: number | null;
}

export interface AgentLastTurnMetadata {
  turn_id?: string | null;
  status?: string | null;
  started_at?: string | null;
  completed_at?: string | null;
}

export interface AgentRouteProvenance {
  schema_version?: number | null;
  adapter_version?: string | null;
  provider?: string | null;
  provider_version?: string | null;
  machine_id?: string | null;
  original_cwd?: string | null;
  cmux_workspace?: string | null;
  cmux_surface?: string | null;
  reconnect_recipe?: string | null;
  provider_resume_handle?: string | null;
  transcript_reference?: string | null;
  mode?: string | null;
  model?: string | null;
  thinking?: string | null;
  context?: string | null;
  last_turn?: AgentLastTurnMetadata | null;
}

export interface AgentRoute {
  id: string;
  adapter_kind: string;
  agent_id: string;
  endpoint?: string | null;
  session_id?: string | null;
  status: string;
  last_heartbeat: string;
  provenance?: AgentRouteProvenance | null;
}

export interface DeliveryHistoryEntry {
  delivery: {
    id: string;
    idempotency_key: string;
    payload: {
      round_id: string;
      decision: "approve" | "request_changes";
      comments: FormalComment[];
    };
  };
  created_at: string;
  delivered_at?: string | null;
  outcome?: string | null;
}

export interface PreparedFeedbackPrompt {
  delivery_id: string;
  idempotency_key: string;
  prompt: string;
  route_id?: string | null;
  agent_id?: string | null;
  session_id?: string | null;
  route_status?: string | null;
  handoff_path: "existing_session" | "reproduce_and_start_fresh";
  manual_submission_required: true;
  reproduction_required: boolean;
  guidance: string;
}

export interface SessionOption {
  key: string;
  label: string;
  kind: "select" | "text" | "boolean" | "number";
  values: string[];
  selected?: string | null;
  supported: boolean;
  unavailable_reason?: string | null;
}

export interface AskConversation {
  id: string;
  round_id: string;
  session_state: "can_continue" | "history_only";
  history_only_reason?: string | null;
  provider_session_label?: string | null;
  options: SessionOption[];
  created_at: string;
  archived_at?: string | null;
}

export interface AskTurn {
  id: string;
  conversation_id: string;
  idempotency_key: string;
  prompt: string;
  anchor?: Anchor | null;
  option_values: Record<string, string>;
  state: "queued" | "streaming" | "completed" | "cancelled" | "failed" | "interrupted";
  created_at: string;
  completed_at?: string | null;
  failure_reason?: string | null;
  response_text: string;
}

export interface ReproductionRepository {
  repository_id: string;
  source: string;
  destination: string;
  head_sha: string;
}

export interface ReproductionPreview {
  destination: string;
  repositories: ReproductionRepository[];
  command_bundle: string;
  agent_working_directory: string;
  launch_guidance: string;
}

export interface ReproductionResult extends ReproductionPreview {}

export type ConnectionSource = "existing_copilot_cli" | "app_owned_oauth" | "none";
export type ConnectionState = "connected" | "not_connected" | "unavailable";

export interface CapabilityStatus {
  capability: "copilot_app" | "pr_read" | "pr_publish";
  state: ConnectionState;
  source: ConnectionSource;
  account?: string | null;
  optional: boolean;
  explanation: string;
}

export interface DeviceFlowPublicState {
  capability: string;
  userCode: string;
  verificationUri: string;
  expiresAtUnixSeconds: number;
  secondsRemaining: number;
  phase: "pending" | "expired";
  canCancel: boolean;
}

export interface DeviceFlowPollResult {
  capability: string;
  phase: "pending" | "slow_down" | "connected" | "expired" | "denied" | "account_mismatch";
  account?: string | null;
  secondsRemaining: number;
  retryAfterSeconds?: number | null;
  message: string;
}

export interface ConnectionHealth {
  cli: {
    installed: boolean;
    signedIn: boolean;
    account?: string | null;
    validationIsReadOnly: boolean;
  };
  copilot: CapabilityStatus;
  prRead: CapabilityStatus;
  prPublish: CapabilityStatus;
  keychain: {
    available: boolean;
    service: string;
    recoveryInstructions?: string | null;
  };
  publicClientId?: string | null;
  pendingDeviceFlow?: DeviceFlowPublicState | null;
}
