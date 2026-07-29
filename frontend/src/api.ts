import { invoke, isTauri } from "@tauri-apps/api/core";
import type {
  Collection,
  AskConversation,
  AskTurn,
  Anchor,
  FormalComment,
  DeliveryHistoryEntry,
  LocalSubmissionRequest,
  LocalPreflight,
  MaterializedDiff,
  ReviewBrief,
  ReviewRound,
  ReproductionPreview,
  ReproductionResult,
  PinnedFileContent,
  SubmissionOutcome,
  ViewedFile,
  ConnectionHealth,
  ConnectionSource,
  DeviceFlowPublicState,
  DeviceFlowPollResult,
  AgentRoute,
  PreparedFeedbackPrompt,
  UpdateCheck,
  MachineEndpoint,
  MachineIndexResult,
  MachineStatus,
  GithubOpenedPullRequest,
  GithubPullRequestIntakePreview,
  GithubPublishAttempt,
  GithubCommentRefreshResult,
  CopilotCapabilities,
  CopilotPollResult,
  CopilotSessionInfo,
  SessionOption,
  UpdateInstall,
  DiagnosticsExport,
} from "./types";

export const desktopAvailable = isTauri();

export async function checkForUpdate(): Promise<UpdateCheck> {
  return invoke("check_for_update");
}

export async function installUpdate(expectedVersion: string): Promise<UpdateInstall> {
  return invoke("install_update", {
    request: { expectedVersion, confirmed: true },
  });
}

export async function relaunchAfterUpdate(): Promise<void> {
  return invoke("relaunch_after_update", { confirmed: true });
}

export async function exportRedactedDiagnostics(
  openInFinder: boolean,
): Promise<DiagnosticsExport> {
  return invoke("export_redacted_diagnostics", { openInFinder });
}

export async function connectionStatus(): Promise<ConnectionHealth> {
  return invoke("connection_status");
}

export async function retryConnection(): Promise<ConnectionHealth> {
  return invoke("retry_connection");
}

export async function selectExistingCopilotCli(): Promise<ConnectionHealth> {
  return invoke("select_existing_copilot_cli");
}

export async function startDeviceFlow(capability: string): Promise<DeviceFlowPublicState> {
  return invoke("start_device_flow", { request: { capability } });
}

export async function cancelDeviceFlow(): Promise<ConnectionHealth> {
  return invoke("cancel_device_flow");
}

export async function completeDeviceFlow(): Promise<DeviceFlowPollResult> {
  return invoke("complete_device_flow");
}

export async function openKeychainAccess(): Promise<void> {
  return invoke("open_keychain_access");
}

export async function disconnectCapability(
  capability: string,
  source: ConnectionSource,
): Promise<ConnectionHealth> {
  return invoke("disconnect_capability", { request: { capability, source } });
}

export async function setPublicClientId(
  publicClientId: string,
  confirmed: boolean,
): Promise<{ changed: boolean; appOwnedRecordsCleared: boolean; sqlitePreserved: boolean }> {
  return invoke("set_public_client_id", { request: { publicClientId, confirmed } });
}

export async function listRounds(
  collection?: Collection,
  includeOld = false,
): Promise<ReviewRound[]> {
  return invoke("list_rounds", { collection: collection ?? null, includeOld });
}

export async function listMachines(): Promise<MachineStatus[]> {
  return invoke("list_machines");
}

export async function addMachine(
  name: string,
  endpoint: MachineEndpoint,
): Promise<{ machine: MachineStatus["machine"]; created: boolean }> {
  return invoke("add_machine", {
    config: { name, endpoint, source_type: "review_queue_daemon" },
  });
}

export async function removeMachine(idOrName: string): Promise<{ id: string; removed: boolean }> {
  return invoke("remove_machine", { idOrName });
}

export async function connectMachine(id: string): Promise<MachineStatus> {
  return invoke("connect_machine", { id });
}

export async function disconnectMachine(id: string): Promise<MachineStatus> {
  return invoke("disconnect_machine", { id });
}

export async function fetchMachineIndex(id: string): Promise<MachineIndexResult> {
  return invoke("fetch_machine_index", { id });
}

export async function materializeMachineRound(
  id: string,
  sourceItemId: string,
): Promise<{ outcome: "created" | "existing" | "superseded"; round: ReviewRound }> {
  return invoke("materialize_machine_round", { id, sourceItemId });
}

export async function queueGithubPullRequest(url: string): Promise<SubmissionOutcome> {
  return invoke("github_queue_pull_request", { request: { url } });
}

export async function previewGithubPullRequest(
  url: string,
): Promise<GithubPullRequestIntakePreview> {
  return invoke("github_preview_pull_request", { request: { url } });
}

export async function confirmGithubPullRequest(
  preview: GithubPullRequestIntakePreview,
): Promise<SubmissionOutcome> {
  return invoke("github_confirm_queue_pull_request", { request: { preview } });
}

export async function openGithubPullRequest(roundId: string): Promise<GithubOpenedPullRequest> {
  return invoke("github_open_pull_request", { roundId });
}

export async function refreshGithubComments(roundId: string): Promise<GithubCommentRefreshResult> {
  return invoke("github_refresh_comments", { roundId });
}

export async function checkGithubStaleness(
  roundId: string,
): Promise<{ pinned_head_sha: string; observed_head_sha: string; checked_at: string }> {
  return invoke("github_check_staleness", { roundId });
}

export async function refreshGithubRound(roundId: string): Promise<SubmissionOutcome> {
  return invoke("github_refresh_round", { roundId });
}

export async function prepareGithubPublish(roundId: string): Promise<GithubPublishAttempt> {
  return invoke("github_prepare_publish", { roundId });
}

export async function publishGithub(attemptId: string): Promise<GithubPublishAttempt> {
  return invoke("github_publish", {
    request: {
      attemptId,
      confirmation: { confirmed: true, token: `publish-github:${attemptId}` },
    },
  });
}

export async function getRound(id: string): Promise<ReviewRound> {
  return invoke("get_round", { id });
}

export async function materializeRoundDiff(id: string): Promise<MaterializedDiff> {
  return invoke("materialize_round_diff", { id });
}

export async function materializeRoundFile(
  roundId: string,
  repositoryId: string,
  path: string,
  side: "LEFT" | "RIGHT",
): Promise<PinnedFileContent> {
  return invoke("materialize_round_file", { roundId, repositoryId, path, side });
}

export async function previewRoundReproduction(
  roundId: string,
  destination: string,
): Promise<ReproductionPreview> {
  return invoke("preview_round_reproduction", { request: { roundId, destination } });
}

export async function materializeRoundReproduction(
  roundId: string,
  destination: string,
): Promise<ReproductionResult> {
  return invoke("materialize_round_reproduction", {
    request: {
      roundId,
      destination,
      confirmation: { confirmed: true, token: `reproduce:${roundId}` },
    },
  });
}

export async function listViewedFiles(roundId: string): Promise<ViewedFile[]> {
  return invoke("list_viewed_files", { roundId });
}

export async function setFileViewed(
  roundId: string,
  repositoryId: string,
  path: string,
  viewed: boolean,
): Promise<void> {
  return invoke("set_file_viewed", {
    request: { roundId, repositoryId, path, viewed },
  });
}

export async function listFormalComments(roundId: string): Promise<FormalComment[]> {
  return invoke("list_formal_comments", { roundId });
}

export async function createFormalComment(
  roundId: string,
  body: string,
  anchor: Anchor | null = null,
  threadId?: string,
): Promise<FormalComment> {
  return invoke("create_formal_comment", {
    request: {
      roundId,
      threadId: threadId ?? (anchor
        ? `${anchor.repository_id}:${anchor.workspace_relative_path}:${anchor.side}:${anchor.start_line}:${anchor.end_line}`
        : "round"),
      body,
      anchor,
    },
  });
}

export async function editFormalComment(
  commentId: string,
  body: string,
  anchor: Anchor | null = null,
): Promise<FormalComment> {
  return invoke("edit_formal_comment", {
    request: { commentId, body, anchor },
  });
}

export async function deleteFormalComment(commentId: string): Promise<void> {
  return invoke("delete_formal_comment", { commentId });
}

export async function getRoundDecision(
  roundId: string,
): Promise<"approve" | "request_changes" | null> {
  return invoke("get_round_decision", { id: roundId });
}

export async function listAgentRoutes(): Promise<AgentRoute[]> {
  return invoke("list_agent_routes");
}

export async function listFeedbackDeliveryHistory(
  roundId: string,
): Promise<DeliveryHistoryEntry[]> {
  return invoke("list_feedback_delivery_history", { roundId });
}

export async function prepareFeedbackHandoff(
  roundId: string,
  routeId: string | null,
): Promise<PreparedFeedbackPrompt> {
  return invoke("prepare_feedback_handoff", {
    request: { roundId, routeId },
  });
}

export async function confirmManualFeedbackSubmission(deliveryId: string): Promise<void> {
  return invoke("confirm_manual_feedback_submission", {
    request: {
      deliveryId,
      confirmation: {
        confirmed: true,
        token: `manual-submit:${deliveryId}`,
      },
    },
  });
}

export async function activeConversation(
  roundId: string,
  options: SessionOption[] = [],
): Promise<AskConversation> {
  return invoke("active_conversation", { request: { roundId, options } });
}

export async function currentConversation(roundId: string): Promise<AskConversation | null> {
  return invoke("current_conversation", { roundId });
}

export async function listPreviousChats(roundId: string): Promise<AskConversation[]> {
  return invoke("list_previous_chats", { roundId });
}

export async function listAskTurns(conversationId: string): Promise<AskTurn[]> {
  return invoke("list_ask_turns", { conversationId });
}

export async function clearChat(roundId: string): Promise<AskConversation> {
  return invoke("clear_chat", { roundId });
}

export async function copilotCapabilities(): Promise<CopilotCapabilities> {
  return invoke("copilot_capabilities");
}

export async function startCopilotSession(
  roundId: string,
  conversationId: string,
  optionValues: Record<string, string>,
): Promise<CopilotSessionInfo> {
  return invoke("copilot_start_session", {
    request: { roundId, conversationId, optionValues },
  });
}

export async function changeCopilotOption(
  roundId: string,
  conversationId: string,
  key: string,
  value: string,
): Promise<{ key: string; requested_value: string; effect: string; active_option_stamp: Record<string, string> }> {
  return invoke("copilot_change_option", {
    request: { roundId, conversationId, key, value },
  });
}

export async function sendCopilotPrompt(
  roundId: string,
  conversationId: string,
  prompt: string,
  anchor: Anchor | null,
  optionValues: Record<string, string>,
): Promise<AskTurn> {
  return invoke("copilot_send_prompt", {
    request: {
      roundId,
      conversationId,
      prompt,
      idempotencyKey: crypto.randomUUID(),
      anchor,
      optionValues,
    },
  });
}

export async function pollCopilotPrompt(turnId: string): Promise<CopilotPollResult> {
  return invoke("copilot_poll_prompt", { request: { turnId } });
}

export async function cancelCopilotPrompt(turnId: string): Promise<AskTurn> {
  return invoke("copilot_cancel_prompt", { turnId });
}

export async function clearCopilotChat(
  roundId: string,
  conversationId: string,
): Promise<AskConversation> {
  return invoke("copilot_clear_chat", { roundId, conversationId });
}

export async function submitLocal(
  request: LocalSubmissionRequest,
): Promise<SubmissionOutcome> {
  return invoke("submit_local", { request });
}

export async function preflightLocal(
  request: LocalSubmissionRequest,
): Promise<LocalPreflight> {
  return invoke("preflight_local", { request });
}

export async function editRoundBrief(id: string, brief: ReviewBrief): Promise<void> {
  return invoke("edit_round_brief", { id, brief });
}

export async function requestChanges(id: string): Promise<void> {
  return invoke("request_changes", { id });
}

export async function approveRemote(id: string): Promise<void> {
  return invoke("approve_remote", { id });
}

export async function completeRound(id: string): Promise<void> {
  return invoke("complete_round", { id });
}

export async function requeueRound(id: string): Promise<void> {
  return invoke("requeue_round", { id });
}

export async function moveRound(id: string, targetRank: number): Promise<void> {
  return invoke("move_round", { id, targetRank });
}

export async function purgeRound(
  id: string,
  confirmation: "delete" | "approve_local",
): Promise<void> {
  if (confirmation === "approve_local") {
    return invoke("approve_local", {
      id,
      confirmation: { confirmed: true, token: `approve-local:${id}` },
    });
  }
  return invoke("purge_round", {
    id,
    confirmation: { confirmed: true, token: `purge:${id}` },
  });
}
