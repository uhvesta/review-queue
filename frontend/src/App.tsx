import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  activeConversation,
  addMachine,
  approveRemote,
  clearCopilotChat,
  cancelDeviceFlow,
  cachedGithubRound,
  completeDeviceFlow,
  copilotCapabilities,
  completeRound,
  connectionStatus,
  createFormalComment,
  currentConversation,
  deleteFormalComment,
  desktopAvailable,
  disconnectCapability,
  disconnectMachine,
  editRoundBrief,
  editFormalComment,
  getRoundDecision,
  getRound,
  fetchMachineIndex,
  checkGithubStaleness,
  listRounds,
  listMachines,
  openGithubPullRequest,
  listFormalComments,
  listFeedbackDeliveryHistory,
  listAskTurns,
  listAgentRoutes,
  listPreviousChats,
  listViewedFiles,
  materializeRoundDiff,
  materializeRoundFile,
  materializeRoundReproduction,
  materializeMachineRound,
  prepareGithubPublish,
  moveRound,
  openKeychainAccess,
  preflightLocal,
  previewRoundReproduction,
  purgeRound,
  requestChanges,
  retryConnection,
  selectExistingCopilotCli,
  refreshGithubComments,
  refreshGithubRound,
  connectMachine,
  requeueRound,
  setFileViewed,
  setPublicClientId,
  prepareFeedbackHandoff,
  confirmManualFeedbackSubmission,
  startDeviceFlow,
  submitLocal,
  removeMachine,
  previewGithubPullRequest,
  confirmGithubPullRequest,
  publishGithub,
  startCopilotSession,
  sendCopilotPrompt,
  pollCopilotPrompt,
  cancelCopilotPrompt,
  checkForUpdate,
  changeCopilotOption,
  exportRedactedDiagnostics,
  installUpdate,
  relaunchAfterUpdate,
} from "./api";
import type {
  AskConversation,
  AskTurn,
  Anchor,
  AgentRoute,
  CommandError,
  ConnectionHealth,
  DeviceFlowPublicState,
  DeliveryHistoryEntry,
  DiffFile,
  DiffHunk,
  DiffLine,
  FormalComment,
  LocalSubmissionRequest,
  MaterializedDiff,
  LocalPreflight,
  ReviewBrief,
  ReviewRound,
  ReproductionPreview,
  PinnedFileContent,
  RepositoryDiff,
  MachineEndpoint,
  MachineIndexResult,
  MachineStatus,
  CopilotCapabilities,
  CopilotCapabilityGroup,
  SessionOption,
  GithubMaterializedFile,
  GithubPullRequestIntakePreview,
  GithubPublishAttempt,
  ImportedComment,
  PreparedFeedbackPrompt,
  SourceCapability,
  UpdateCheck,
} from "./types";
import {
  RepositoryFileTree,
  diffLineCounts,
  repositoryFileKey as fileKey,
} from "./RepositoryFileTree";
import { applicationVersion } from "./version";

type Modal = "submit" | "github" | "details" | "reproduce" | "settings" | "machine" | null;
type PurgeIntent = { round: ReviewRound; kind: "delete" | "approve_local" };
type ReviewerRecovery =
  | { kind: "load_viewed" }
  | { kind: "load_inline_state" }
  | { kind: "set_viewed"; repositoryId: string; path: string; viewed: boolean }
  | { kind: "load_decision" }
  | { kind: "load_cached_github" }
  | { kind: "refresh_comments" }
  | { kind: "check_head" }
  | { kind: "prepare_publish" }
  | { kind: "refresh_round" };
type ReviewerActionFailure = { error: CommandError; recovery: ReviewerRecovery };

const emptyBrief = (): ReviewBrief => ({
  title: "",
  what: "",
  why: "",
  approach_alternatives: "",
  testing: "",
});

function mediaMatches(query: string, fallback: (width: number) => boolean) {
  return typeof window.matchMedia === "function"
    ? window.matchMedia(query).matches
    : fallback(window.innerWidth);
}

const dialogFocusableSelector = [
  "button:not([disabled])",
  "[href]",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

function useDialogFocus(onClose: () => void) {
  const dialogRef = useRef<HTMLElement>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    const previousFocus = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    const dialog = dialogRef.current;
    if (!dialog) return;
    const frame = window.requestAnimationFrame(() => {
      const initial = dialog.querySelector<HTMLElement>(
        "[data-dialog-initial-focus], [autofocus]",
      ) ?? dialog.querySelector<HTMLElement>(dialogFocusableSelector) ?? dialog;
      initial.focus();
    });
    return () => {
      window.cancelAnimationFrame(frame);
      if (previousFocus?.isConnected) previousFocus.focus();
    };
  }, []);

  const onKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      onCloseRef.current();
      return;
    }
    if (event.key !== "Tab") return;
    const dialog = dialogRef.current;
    if (!dialog) return;
    const focusable = [...dialog.querySelectorAll<HTMLElement>(dialogFocusableSelector)]
      .filter((element) => !element.hidden && element.getAttribute("aria-hidden") !== "true");
    if (!focusable.length) {
      event.preventDefault();
      dialog.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (!dialog.contains(document.activeElement)) {
      event.preventDefault();
      (event.shiftKey ? last : first).focus();
    } else if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };

  return { ref: dialogRef, onKeyDown, tabIndex: -1 };
}

export function App() {
  const [rounds, setRounds] = useState<ReviewRound[]>([]);
  const [selected, setSelected] = useState<ReviewRound | null>(null);
  const [showOld, setShowOld] = useState(false);
  const [loading, setLoading] = useState(desktopAvailable);
  const [error, setError] = useState<CommandError | null>(null);
  const [modal, setModal] = useState<Modal>(null);
  const [purgeIntent, setPurgeIntent] = useState<PurgeIntent | null>(null);
  const [connectionHealth, setConnectionHealth] = useState<ConnectionHealth | null>(null);
  const [machines, setMachines] = useState<MachineStatus[]>([]);
  const [activeMachineId, setActiveMachineId] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!desktopAvailable) return;
    setLoading(true);
    setError(null);
    try {
      setRounds(await listRounds(undefined, showOld));
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setLoading(false);
    }
  }, [showOld]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (!desktopAvailable) return;
    connectionStatus().then(setConnectionHealth).catch((problem) => setError(toCommandError(problem)));
    listMachines().then(setMachines).catch((problem) => setError(toCommandError(problem)));
  }, []);

  const refreshMachines = useCallback(async () => {
    const next = await listMachines();
    setMachines(next);
    return next;
  }, []);

  const openRound = async (round: ReviewRound) => {
    setError(null);
    try {
      setSelected(await getRound(round.id));
    } catch (problem) {
      setError(toCommandError(problem));
    }
  };

  const mutate = async (action: () => Promise<unknown>) => {
    setError(null);
    try {
      await action();
      setSelected(null);
      await refresh();
    } catch (problem) {
      setError(toCommandError(problem));
    }
  };

  const mutateSelected = async (id: string, action: () => Promise<unknown>) => {
    setError(null);
    try {
      await action();
      const next = await getRound(id);
      await refresh();
      setSelected(next);
      return true;
    } catch (problem) {
      setError(toCommandError(problem));
      return false;
    }
  };

  const openRoundModal = async (round: ReviewRound, target: "details" | "reproduce") => {
    setError(null);
    try {
      setSelected(await getRound(round.id));
      setModal(target);
    } catch (problem) {
      setError(toCommandError(problem));
    }
  };

  const copyRoundFeedback = async (round: ReviewRound) => {
    setError(null);
    try {
      const [decision, comments] = await Promise.all([
        getRoundDecision(round.id),
        listFormalComments(round.id),
      ]);
      const text = [
        `Review feedback for ${round.brief.title}`,
        `Decision: ${decision?.replace("_", " ") ?? "not recorded"}`,
        ...comments.map((comment, index) => `${index + 1}. ${comment.body}`),
      ].join("\n");
      await navigator.clipboard.writeText(text);
    } catch (problem) {
      setError(toCommandError(problem));
    }
  };

  if (!desktopAvailable) {
    return <DesktopRequired />;
  }

  return (
    <div className="app-shell">
      <Sidebar
        activeCount={rounds.filter(isActive).length}
        machines={machines}
        activeMachineId={activeMachineId}
        onThisMac={() => setActiveMachineId(null)}
        onMachine={setActiveMachineId}
        onAdd={() => setModal("machine")}
      />
      {selected ? (
        <Reviewer
          key={selected.id}
          round={selected}
          onBack={() => setSelected(null)}
          onDetails={() => setModal("details")}
          onReproduce={() => setModal("reproduce")}
          onSettings={() => setModal("settings")}
          onRequestChanges={() => mutateSelected(selected.id, () => requestChanges(selected.id))}
          onApproveRemote={() => mutateSelected(selected.id, () => approveRemote(selected.id))}
          onComplete={() => mutate(() => completeRound(selected.id))}
          onPurge={(kind) => setPurgeIntent({ round: selected, kind })}
          onGithubRoundRefreshed={async (round) => {
            await refresh();
            setSelected(round);
          }}
        />
      ) : activeMachineId ? (
        <MachineQueue
          status={machines.find((machine) => machine.machine.id === activeMachineId) ?? null}
          cachedRounds={rounds.filter((round) =>
            round.collection === "machine"
            && round.source_metadata?.kind === "machine"
            && round.source_metadata.machine_id === activeMachineId
          )}
          onBack={() => setActiveMachineId(null)}
          onChanged={refreshMachines}
          onOpen={async (round) => {
            await refresh();
            await openRound(round);
          }}
          onComplete={(round) => mutate(() => completeRound(round.id))}
          onRequeue={(round) => mutate(() => requeueRound(round.id))}
          onMove={(round, rank) => mutate(() => moveRound(round.id, rank))}
          onDelete={(round) => setPurgeIntent({ round, kind: "delete" })}
          onDetails={(round) => void openRoundModal(round, "details")}
          onReproduce={(round) => void openRoundModal(round, "reproduce")}
          onCopyFeedback={(round) => void copyRoundFeedback(round)}
          onShowOld={() => setShowOld(true)}
          onError={setError}
        />
      ) : (
        <QueueHome
          rounds={rounds}
          loading={loading}
          error={error}
          showOld={showOld}
          onOld={() => setShowOld((value) => !value)}
          onOpen={openRound}
          onSubmit={() => setModal("submit")}
          onReviewPr={() => setModal("github")}
          connectionHealth={connectionHealth}
          machines={machines}
          onSettings={() => setModal("settings")}
          onRefresh={refresh}
          onComplete={(round) => mutate(() => completeRound(round.id))}
          onRequeue={(round) => mutate(() => requeueRound(round.id))}
          onMove={(round, rank) => mutate(() => moveRound(round.id, rank))}
          onDelete={(round) => setPurgeIntent({ round, kind: "delete" })}
          onDetails={(round) => void openRoundModal(round, "details")}
          onReproduce={(round) => void openRoundModal(round, "reproduce")}
          onCopyFeedback={(round) => void copyRoundFeedback(round)}
          onRefreshGithub={(round) => mutate(() => refreshGithubRound(round.id))}
        />
      )}
      {error && (selected || activeMachineId) && (
        <ErrorBanner error={error} onDismiss={() => setError(null)} />
      )}
      {modal === "submit" && (
        <SubmitLocalDialog
          onClose={() => setModal(null)}
          onSubmitted={async (round) => {
            setModal(null);
            await refresh();
            await openRound(round);
          }}
        />
      )}
      {modal === "github" && (
        <AddGithubPullRequestDialog
          onClose={() => setModal(null)}
          onAdded={async () => {
            setModal(null);
            await refresh();
          }}
        />
      )}
      {modal === "details" && selected && (
        <DetailsDialog
          round={selected}
          onClose={() => setModal(null)}
          onReproduce={() => setModal("reproduce")}
          onSaved={async (brief) => {
            await editRoundBrief(selected.id, brief);
            setSelected(await getRound(selected.id));
            await refresh();
          }}
        />
      )}
      {modal === "reproduce" && selected && (
        <ReproductionDialog round={selected} onClose={() => setModal("details")} />
      )}
      {modal === "settings" && (
        <SettingsDialog
          initialHealth={connectionHealth}
          onHealth={setConnectionHealth}
          onClose={() => setModal(null)}
        />
      )}
      {modal === "machine" && (
        <AddMachineDialog
          onClose={() => setModal(null)}
          onAdded={async (machine) => {
            setModal(null);
            await refreshMachines();
            setActiveMachineId(machine.machine.id);
          }}
        />
      )}
      {purgeIntent && (
        <PurgeDialog
          intent={purgeIntent}
          onCancel={() => setPurgeIntent(null)}
          onConfirm={async () => {
            const intent = purgeIntent;
            setPurgeIntent(null);
            await mutate(() => purgeRound(intent.round.id, intent.kind));
          }}
        />
      )}
    </div>
  );
}

function DesktopRequired() {
  return (
    <main className="standalone-state">
      <div className="brand-mark">RQ</div>
      <p className="eyebrow">REVIEW QUEUE</p>
      <h1>Open the desktop app</h1>
      <p>
        This browser preview has no access to review data. Run <code>npm run tauri dev</code>{" "}
        from the project to use the local queue.
      </p>
      <p className="safe-copy">No prompt, publish, delivery, or source mutation occurred.</p>
    </main>
  );
}

function Sidebar({
  activeCount,
  machines,
  activeMachineId,
  onThisMac,
  onMachine,
  onAdd,
}: {
  activeCount: number;
  machines: MachineStatus[];
  activeMachineId: string | null;
  onThisMac: () => void;
  onMachine: (id: string) => void;
  onAdd: () => void;
}) {
  return (
    <aside className="sidebar" aria-label="Sources">
      <div className="brand-mark">RQ</div>
      <h2>SOURCES</h2>
      <button
        className={`machine ${activeMachineId ? "" : "current"}`}
        aria-current={activeMachineId ? undefined : "page"}
        aria-label={`this Mac, ${activeCount} active`}
        title={`this Mac; ${activeCount} active`}
        onClick={onThisMac}
      >
        <span>●</span><b>this Mac</b><small>{activeCount} active</small>
      </button>
      {machines.map((status) => (
        <button
          className={`machine ${activeMachineId === status.machine.id ? "current" : ""}`}
          aria-current={activeMachineId === status.machine.id ? "page" : undefined}
          aria-label={`${status.machine.config.name}, ${status.connection}, ${status.cachedItemCount} cached`}
          key={status.machine.id}
          onClick={() => onMachine(status.machine.id)}
          title={`${status.machine.config.name}; ${status.connection}; ${status.cachedItemCount} cached`}
        >
          <span className={status.connection === "connected" ? "health" : ""}>●</span>
          <b>{status.machine.config.name}</b>
          <small>{status.cachedItemCount} cached · {formatCacheAge(status.freshness.age_seconds)}</small>
        </button>
      ))}
      <button className="add-machine" aria-label="Add machine" title="Add machine" onClick={onAdd}>
        ＋ <span>Add machine</span>
      </button>
    </aside>
  );
}

function MachineQueue({
  status,
  cachedRounds,
  onBack,
  onChanged,
  onOpen,
  onComplete,
  onRequeue,
  onMove,
  onDelete,
  onDetails,
  onReproduce,
  onCopyFeedback,
  onShowOld,
  onError,
}: Pick<QueueHomeProps, "onComplete" | "onRequeue" | "onMove" | "onDelete" | "onDetails" | "onReproduce" | "onCopyFeedback"> & {
  status: MachineStatus | null;
  cachedRounds: ReviewRound[];
  onBack: () => void;
  onChanged: () => Promise<MachineStatus[]>;
  onOpen: (round: ReviewRound) => Promise<void>;
  onShowOld: () => void;
  onError: (error: CommandError | null) => void;
}) {
  const [index, setIndex] = useState<MachineIndexResult | null>(null);
  const [working, setWorking] = useState(false);
  const [localStatus, setLocalStatus] = useState(status);

  useEffect(() => setLocalStatus(status), [status]);

  if (!localStatus) {
    return (
      <main className="main">
        <header className="topbar"><h1>Machine unavailable</h1><button onClick={onBack}>this Mac</button></header>
      </main>
    );
  }

  const run = async (action: () => Promise<void>) => {
    setWorking(true);
    onError(null);
    try {
      await action();
    } catch (problem) {
      onError(toCommandError(problem));
    } finally {
      setWorking(false);
    }
  };

  const refreshIndex = () => run(async () => {
    const next = await fetchMachineIndex(localStatus.machine.id);
    setIndex(next);
    const statuses = await onChanged();
    setLocalStatus(statuses.find((item) => item.machine.id === localStatus.machine.id) ?? localStatus);
  });
  const uncachedItems = index?.index.items.filter((item) => !cachedRounds.some((round) =>
    round.source_metadata?.kind === "machine"
    && round.source_metadata.source_item_id === item.source_item_id
  )) ?? [];

  return (
    <main className="main">
      <header className="topbar">
        <div><p className="eyebrow">CONNECTED MACHINE</p><h1>{localStatus.machine.config.name}</h1></div>
        <div className="actions">
          {localStatus.connection === "connected" ? (
            <button disabled={working} onClick={() => void run(async () => {
              setLocalStatus(await disconnectMachine(localStatus.machine.id));
              setIndex(null);
              await onChanged();
            })}>Disconnect</button>
          ) : (
            <button className="primary" disabled={working} onClick={() => void run(async () => {
              const connected = await connectMachine(localStatus.machine.id);
              setLocalStatus(connected);
              await onChanged();
            })}>Connect</button>
          )}
          <button disabled={working || localStatus.connection !== "connected"} onClick={() => void refreshIndex()}>
            {working ? "Working…" : "Refresh machine queue"}
          </button>
          <button className="danger-text" disabled={working} onClick={() => void run(async () => {
            if (!window.confirm(`Remove ${localStatus.machine.config.name}? Cached review rounds remain local.`)) return;
            await removeMachine(localStatus.machine.id);
            await onChanged();
            onBack();
          })}>Remove</button>
        </div>
      </header>
      <section className="machine-summary">
        <p><b>Connection</b> {localStatus.connection}</p>
        <p><b>Cache</b> {localStatus.cachedItemCount} items · {formatCacheAge(localStatus.freshness.age_seconds)}</p>
        <p className="muted">Remote reads happen only when you choose Connect, Refresh, or Open review. Review Queue stores no SSH credentials.</p>
      </section>
      <section className="queue-grid machine-grid">
        <QueueColumn
          title={`${localStatus.machine.config.name.toUpperCase()} CACHED (${cachedRounds.filter(isActive).length})`}
          rounds={cachedRounds}
          empty={`No cached rounds on ${localStatus.machine.config.name}.`}
          onOpen={(round) => void onOpen(round)}
          onComplete={onComplete}
          onRequeue={onRequeue}
          onMove={onMove}
          onDelete={onDelete}
          onDetails={onDetails}
          onReproduce={onReproduce}
          onCopyFeedback={onCopyFeedback}
          onShowOld={onShowOld}
          onRefreshGithub={(round) => void run(async () => {
            const metadata = round.source_metadata;
            if (metadata?.kind !== "machine" || metadata.machine_id !== localStatus.machine.id) {
              throw {
                code: "machine_source_metadata_required",
                message: "This cached round is not bound to the selected machine.",
                data_safety: "No cached round or remote source was changed.",
                next_step: "Open the machine that originally supplied this round, then retry.",
              } satisfies CommandError;
            }
            const next = await fetchMachineIndex(localStatus.machine.id);
            setIndex(next);
            const result = await materializeMachineRound(
              localStatus.machine.id,
              metadata.source_item_id,
            );
            const statuses = await onChanged();
            setLocalStatus(
              statuses.find((item) => item.machine.id === localStatus.machine.id) ?? localStatus,
            );
            await onOpen(result.round);
          })}
        />
        <section className="queue-column machine-items">
          <h2>REMOTE INDEX ({uncachedItems.length})</h2>
          {!index && (
            <p className="empty-state">
              {localStatus.connection === "connected"
                ? `Choose Refresh machine queue to list rounds on ${localStatus.machine.config.name}.`
                : `Connect ${localStatus.machine.config.name} to refresh its queue.`}
            </p>
          )}
          {index && uncachedItems.length === 0 && <p className="empty-state">No uncached rounds on {localStatus.machine.config.name}.</p>}
          {uncachedItems.length > 0 && (
            <>
            {uncachedItems.map((item) => (
              <article className="queue-card" key={item.source_item_id}>
                <div className="card-title"><span className="state-dot" /><strong>{item.title}</strong></div>
                <p>{item.remote_workspace_path}</p>
                <p>{item.topic_key} · snapshot {shortSha(item.manifest_hash)}</p>
                <footer>
                  <span className="status good">remote cached index</span>
                  <button className="open" disabled={working} onClick={() => void run(async () => {
                    const result = await materializeMachineRound(localStatus.machine.id, item.source_item_id);
                    await onOpen(result.round);
                  })}>Open review</button>
                </footer>
              </article>
            ))}
            </>
          )}
        </section>
      </section>
    </main>
  );
}

function AddMachineDialog({
  onClose,
  onAdded,
}: {
  onClose: () => void;
  onAdded: (machine: { machine: MachineStatus["machine"]; created: boolean }) => Promise<void>;
}) {
  const [name, setName] = useState("");
  const [kind, setKind] = useState<"ssh" | "loopback">("ssh");
  const [target, setTarget] = useState("");
  const [remoteSocket, setRemoteSocket] = useState("/tmp/review-queue-daemon.sock");
  const [socketPath, setSocketPath] = useState("");
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<CommandError | null>(null);
  const dialog = useDialogFocus(onClose);

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setWorking(true);
    setError(null);
    const endpoint: MachineEndpoint = kind === "ssh"
      ? { kind: "ssh", target, remote_socket: remoteSocket, adapter: "system_open_ssh" }
      : { kind: "loopback", socket_path: socketPath };
    try {
      await onAdded(await addMachine(name, endpoint));
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setWorking(false);
    }
  };

  return (
    <div className="modal-backdrop">
      <section {...dialog} className="modal" role="dialog" aria-modal="true" aria-labelledby="add-machine-title">
        <header><h2 id="add-machine-title">Add connected machine</h2><button aria-label="Close" onClick={onClose}>×</button></header>
        <form className="form" onSubmit={(event) => void submit(event)}>
          {error && <ErrorPanel error={error} />}
          <label>Name<input required value={name} onChange={(event) => setName(event.target.value)} placeholder="buildbox" autoFocus /></label>
          <label>Connection
            <select value={kind} onChange={(event) => setKind(event.target.value as "ssh" | "loopback")}>
              <option value="ssh">System OpenSSH</option>
              <option value="loopback">Local daemon socket</option>
            </select>
          </label>
          {kind === "ssh" ? (
            <>
              <label>SSH config host<input required value={target} onChange={(event) => setTarget(event.target.value)} placeholder="buildbox" /></label>
              <label>Remote daemon socket<input required value={remoteSocket} onChange={(event) => setRemoteSocket(event.target.value)} /></label>
            </>
          ) : (
            <label>Daemon socket path<input required value={socketPath} onChange={(event) => setSocketPath(event.target.value)} placeholder="/tmp/review-queue-daemon.sock" /></label>
          )}
          <p className="notice">Only an SSH config host and socket path are saved. Existing system SSH configuration and agent credentials remain outside Review Queue.</p>
          <footer><button type="button" onClick={onClose}>Cancel</button><button className="primary" disabled={working}>{working ? "Saving…" : "Add machine"}</button></footer>
        </form>
      </section>
    </div>
  );
}

function AddGithubPullRequestDialog({
  onClose,
  onAdded,
}: {
  onClose: () => void;
  onAdded: (round: ReviewRound) => Promise<void>;
}) {
  const [url, setUrl] = useState("");
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<CommandError | null>(null);
  const [preview, setPreview] = useState<GithubPullRequestIntakePreview | null>(null);
  const dialog = useDialogFocus(onClose);
  const resolve = async (event: React.FormEvent) => {
    event.preventDefault();
    setWorking(true);
    setError(null);
    try {
      setPreview(await previewGithubPullRequest(url));
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setWorking(false);
    }
  };
  const confirm = async () => {
    if (!preview) return;
    setWorking(true);
    setError(null);
    try {
      const result = await confirmGithubPullRequest(preview);
      await onAdded(result.round);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setWorking(false);
    }
  };
  return (
    <div className="modal-backdrop">
      <section {...dialog} className="modal" role="dialog" aria-modal="true" aria-labelledby="add-pr-title">
        <header><h2 id="add-pr-title">Review pull request</h2><button aria-label="Close" onClick={onClose}>×</button></header>
        <form className="form" onSubmit={(event) => void resolve(event)}>
          {error && <ErrorPanel error={error} />}
          <label>GitHub pull request URL
            <input required type="url" value={url} onChange={(event) => { setUrl(event.target.value); setPreview(null); }} placeholder="https://github.com/owner/repo/pull/42" autoFocus />
          </label>
          {preview ? (
            <section className="preflight" aria-label="Pull request preview">
              <b>Confirm pull request</b>
              <p><span>Identity</span><span>{preview.metadata.host}/{preview.metadata.owner}/{preview.metadata.repository}#{preview.metadata.pull_number}</span></p>
              <p><span>Title</span><span>{preview.metadata.title}</span></p>
              <p><span>Base SHA</span><span><code>{preview.metadata.base_sha}</code></span></p>
              <p><span>Head SHA</span><span><code>{preview.metadata.head_sha}</code></span></p>
              <p><span>State</span><span>{preview.metadata.state} · {preview.metadata.is_draft ? "draft" : "not draft"}</span></p>
              {preview.metadata.web_url && <p><span>GitHub URL</span><span><a href={preview.metadata.web_url} target="_blank" rel="noreferrer">Open pull request</a></span></p>}
            </section>
          ) : (
            <p className="notice">Resolve shows read-only metadata only. It creates no queue item and does not pull files or comments.</p>
          )}
          <p className="notice">Confirmation resolves the pull request again. If its identity or metadata changed, no queue item is created; resolve it again before confirming. Complete file blobs and comments are pulled only when you explicitly open the review.</p>
          <footer>
            <button type="button" onClick={onClose}>Cancel</button>
            {preview ? (
              <>
                <button type="submit" disabled={working}>{working ? "Resolving…" : "Resolve again"}</button>
                <button className="primary" type="button" disabled={working} onClick={() => void confirm()}>{working ? "Confirming…" : "Confirm and add to queue"}</button>
              </>
            ) : <button className="primary" disabled={working}>{working ? "Resolving…" : "Resolve pull request"}</button>}
          </footer>
        </form>
      </section>
    </div>
  );
}

function ConnectionWelcome({
  health,
  onSettings,
  onSubmit,
  onReviewPr,
}: {
  health: ConnectionHealth | null;
  onSettings: () => void;
  onSubmit: () => void;
  onReviewPr: () => void;
}) {
  return (
    <section className="welcome-panel" aria-label="Connection health">
      <div>
        <p className="eyebrow">WELCOME</p>
        <h2>Review locally first; connect only what you need.</h2>
      </div>
      <ConnectionSummary label="Copilot /ask" status={health?.copilot} />
      <ConnectionSummary label="PR read" status={health?.prRead} />
      <ConnectionSummary label="PR publish" status={health?.prPublish} />
      {!health?.keychain.available && health?.keychain.recoveryInstructions && (
        <p className="danger-text">{health.keychain.recoveryInstructions}</p>
      )}
      <div className="dialog-actions">
        <button onClick={onSettings}>Connection settings</button>
        <button className="primary" onClick={onSubmit}>Review a local workspace</button>
        <button disabled={health?.prRead.state !== "connected"} onClick={onReviewPr}>Review a GitHub PR locally</button>
      </div>
    </section>
  );
}

function ConnectionSummary({
  label,
  status,
}: {
  label: string;
  status?: ConnectionHealth["copilot"];
}) {
  const connected = status?.state === "connected";
  return (
    <p className="connection-summary">
      <b>{label}</b>
      <span className={connected ? "status good" : "status"}>
        {status ? (connected ? `✓ ${status.account ?? status.source.replaceAll("_", " ")}` : status.state.replaceAll("_", " ")) : "checking…"}
      </span>
    </p>
  );
}

interface QueueHomeProps {
  rounds: ReviewRound[];
  loading: boolean;
  error: CommandError | null;
  showOld: boolean;
  onOld: () => void;
  onOpen: (round: ReviewRound) => void;
  onSubmit: () => void;
  onReviewPr: () => void;
  connectionHealth: ConnectionHealth | null;
  machines: MachineStatus[];
  onSettings: () => void;
  onRefresh: () => Promise<void>;
  onComplete: (round: ReviewRound) => void;
  onRequeue: (round: ReviewRound) => void;
  onMove: (round: ReviewRound, rank: number) => void;
  onDelete: (round: ReviewRound) => void;
  onDetails: (round: ReviewRound) => void;
  onReproduce: (round: ReviewRound) => void;
  onCopyFeedback: (round: ReviewRound) => void;
  onRefreshGithub: (round: ReviewRound) => void;
}

function QueueHome(props: QueueHomeProps) {
  const [nextScope, setNextScope] = useState("overall");
  const local = props.rounds.filter((round) => round.collection === "local");
  const github = props.rounds.filter((round) => round.collection === "github");
  const activeLocal = local.filter(isActive).length;
  const activeGithub = github.filter(isActive).length;
  const next = [...props.rounds].filter((round) =>
    isActive(round) && (
      nextScope === "overall"
      || round.collection === nextScope
      || (
        nextScope.startsWith("machine:")
        && round.collection === "machine"
        && round.source_metadata?.kind === "machine"
        && round.source_metadata.machine_id === nextScope.slice("machine:".length)
      )
    ),
  ).sort((a, b) => {
    const sourceOrder = { local: 0, github: 1, machine: 2 };
    return sourceOrder[a.collection] - sourceOrder[b.collection] || a.rank - b.rank;
  })[0];

  return (
    <main className="main">
      <header className="topbar">
        <div><p className="eyebrow">REVIEW QUEUE</p><h1>Queue Home</h1></div>
        <div className="actions">
          <button className="primary" onClick={props.onSubmit}>Submit local</button>
          <button
            onClick={props.onReviewPr}
            disabled={props.connectionHealth?.prRead.state !== "connected"}
            title={props.connectionHealth?.prRead.state === "connected" ? "Add a pull request to the GitHub queue" : "Connect PR read before adding a pull request"}
          >
            Review PR
          </button>
          <button onClick={() => void props.onRefresh()} disabled={props.loading}>
            {props.loading ? "Refreshing…" : "Refresh"}
          </button>
          <button aria-label="Settings" onClick={props.onSettings}>⚙</button>
        </div>
      </header>
      {props.error && <ErrorPanel error={props.error} onRetry={props.onRefresh} />}
      {!props.loading && props.rounds.length === 0 && (
        <ConnectionWelcome health={props.connectionHealth} onSettings={props.onSettings} onSubmit={props.onSubmit} onReviewPr={props.onReviewPr} />
      )}
      <section className="queue-grid" aria-busy={props.loading}>
        <QueueColumn
          title={`LOCAL (${activeLocal})`}
          rounds={local}
          empty="No local rounds — Submit local or run /localreview-submit."
          onOpen={props.onOpen}
          onComplete={props.onComplete}
          onRequeue={props.onRequeue}
          onMove={props.onMove}
          onDelete={props.onDelete}
          onDetails={props.onDetails}
          onReproduce={props.onReproduce}
          onCopyFeedback={props.onCopyFeedback}
          onShowOld={() => { if (!props.showOld) props.onOld(); }}
          onRefreshGithub={props.onRefreshGithub}
        />
        <QueueColumn
          title={`GITHUB (${activeGithub})`}
          rounds={github}
          empty="No PRs — connect PR read, then Review PR."
          onOpen={props.onOpen}
          onComplete={props.onComplete}
          onRequeue={props.onRequeue}
          onMove={props.onMove}
          onDelete={props.onDelete}
          onDetails={props.onDetails}
          onReproduce={props.onReproduce}
          onCopyFeedback={props.onCopyFeedback}
          onShowOld={() => { if (!props.showOld) props.onOld(); }}
          onRefreshGithub={props.onRefreshGithub}
        />
      </section>
      <div className="queue-footer">
        <div className="open-next">
          <button className="next" disabled={!next} onClick={() => next && props.onOpen(next)}>
            {next ? `Open next · ${next.brief.title}` : "Open next"}
          </button>
          <select aria-label="Open next source" value={nextScope} onChange={(event) => setNextScope(event.target.value)}>
            <option value="overall">Overall</option>
            <option value="local">Local</option>
            <option value="github">GitHub</option>
            {props.machines.map((machine) => (
              <option value={`machine:${machine.machine.id}`} key={machine.machine.id}>
                {machine.machine.config.name}
              </option>
            ))}
          </select>
        </div>
        <label className="toggle">
          <input type="checkbox" checked={props.showOld} onChange={props.onOld} />
          Show completed / old rounds
        </label>
      </div>
    </main>
  );
}

function QueueColumn({
  title,
  rounds,
  empty,
  onOpen,
  onComplete,
  onRequeue,
  onMove,
  onDelete,
  onDetails,
  onReproduce,
  onCopyFeedback,
  onShowOld,
  onRefreshGithub,
}: Pick<QueueHomeProps, "onOpen" | "onComplete" | "onRequeue" | "onMove" | "onDelete" | "onDetails" | "onReproduce" | "onCopyFeedback" | "onRefreshGithub"> & {
  title: string;
  rounds: ReviewRound[];
  empty: string;
  onShowOld: () => void;
}) {
  const [draggedId, setDraggedId] = useState<string | null>(null);
  const activeRounds = rounds.filter(isActive);
  const bottomRank = activeRounds.reduce((rank, round) => Math.max(rank, round.rank), 0);
  return (
    <section className="queue-column">
      <h2>{title}</h2>
      {rounds.length === 0 && <p className="empty-state">{empty}</p>}
      {rounds.map((round) => {
        const readOnly = !isActive(round) || Boolean(round.superseded_by);
        return (
          <article
            className={`queue-card ${readOnly ? "old-round" : ""}`}
            key={round.id}
            tabIndex={0}
            aria-keyshortcuts={readOnly ? undefined : "Alt+ArrowUp Alt+ArrowDown Alt+Home Alt+End"}
            aria-label={readOnly ? round.brief.title : `${round.brief.title}. Press Alt+Arrow Up/Down, Alt+Home, or Alt+End to reorder.`}
            onDragOver={(event) => {
              if (draggedId && !readOnly) event.preventDefault();
            }}
            onDrop={(event) => {
              event.preventDefault();
              const dragged = rounds.find((item) => item.id === draggedId);
              if (dragged && dragged.id !== round.id && isActive(dragged) && !readOnly) {
                onMove(dragged, round.rank);
              }
              setDraggedId(null);
            }}
            onKeyDown={(event) => {
              if (event.target !== event.currentTarget || readOnly || !event.altKey) return;
              let rank: number;
              if (event.key === "ArrowUp") rank = Math.max(0, round.rank - 1);
              else if (event.key === "ArrowDown") rank = round.rank + 1;
              else if (event.key === "Home") rank = 0;
              else if (event.key === "End") rank = bottomRank;
              else return;
              event.preventDefault();
              event.stopPropagation();
              onMove(round, rank);
            }}
          >
            <div className="card-title">
              <button
                className="drag"
                aria-label={`Drag ${round.brief.title} to reorder`}
                title="Drag to reorder · Alt+Arrow/Home/End also works"
                draggable={!readOnly}
                disabled={readOnly}
                onDragStart={(event) => {
                  setDraggedId(round.id);
                  event.dataTransfer.effectAllowed = "move";
                  event.dataTransfer.setData("text/plain", round.id);
                }}
                onDragEnd={() => setDraggedId(null)}
              >≡</button>
              <span className="state-dot" data-state={displayLifecycle(round.lifecycle)} />
              <strong>{round.brief.title}</strong>
              <span className="rank" aria-label={`Queue position ${round.rank + 1}`}>#{round.rank + 1}</span>
            </div>
            <p>{round.manifest.repositories.length} {round.manifest.repositories.length === 1 ? "repo" : "repos"} · {round.manifest.workspace_root}</p>
            <p>{round.manifest.topic} · snapshot {shortSha(round.manifest_hash)}</p>
            {round.brief.why && <p className="muted">why: {round.brief.why}</p>}
            <footer>
              <span className="status" data-state={displayLifecycle(round.lifecycle)}>{displayLifecycle(round.lifecycle)}</span>
              <button className="open" onClick={() => onOpen(round)}>Open review</button>
              {readOnly ? (
                <button onClick={() => onRequeue(round)} disabled={Boolean(round.superseded_by)}>
                  Requeue
                </button>
              ) : (
                <>
                  <button aria-label={`Move ${round.brief.title} up`} onClick={() => onMove(round, Math.max(0, round.rank - 1))}>↑</button>
                  <button aria-label={`Move ${round.brief.title} down`} onClick={() => onMove(round, round.rank + 1)}>↓</button>
                </>
              )}
              <details className="card-overflow">
                <summary aria-label={`More actions for ${round.brief.title}`}>•••</summary>
                <div>
                  <button onClick={() => onDetails(round)}>Edit brief / details</button>
                  <button onClick={() => onReproduce(round)}>Reproduce…</button>
                  <button onClick={() => onCopyFeedback(round)}>Copy feedback prompt</button>
                  <button onClick={onShowOld}>Show completed / old topic rounds</button>
                  {!readOnly && <button onClick={() => onComplete(round)}>Complete</button>}
                  {supportsCapability(round, "remote_refresh") && !readOnly && (
                    <button onClick={() => onRefreshGithub(round)}>
                      {round.source_adapter.adapter_id === "github_pull_request_mirror"
                        ? "Refresh remote PR"
                        : "Refresh remote source"}
                    </button>
                  )}
                  {!readOnly && <button onClick={() => onMove(round, 0)}>Move to top</button>}
                  {!readOnly && <button onClick={() => onMove(round, bottomRank)}>Move to bottom</button>}
                  <button className="danger-text" onClick={() => onDelete(round)}>Delete</button>
                </div>
              </details>
            </footer>
          </article>
        );
      })}
    </section>
  );
}

function Reviewer({
  round,
  onBack,
  onDetails,
  onReproduce,
  onSettings,
  onRequestChanges,
  onApproveRemote,
  onComplete,
  onPurge,
  onGithubRoundRefreshed,
}: {
  round: ReviewRound;
  onBack: () => void;
  onDetails: () => void;
  onReproduce: () => void;
  onSettings: () => void;
  onRequestChanges: () => Promise<boolean>;
  onApproveRemote: () => Promise<boolean>;
  onComplete: () => void;
  onPurge: (kind: "delete" | "approve_local") => void;
  onGithubRoundRefreshed: (round: ReviewRound) => Promise<void>;
}) {
  const [sidebarCollapsed, setSidebarCollapsed] = useState(
    () => mediaMatches("(max-width: 580px)", (width) => width <= 580),
  );
  const [chatOpen, setChatOpen] = useState(
    () => mediaMatches("(min-width: 1121px)", (width) => width >= 1121),
  );
  const [diff, setDiff] = useState<MaterializedDiff | null>(null);
  const [coreDiffError, setCoreDiffError] = useState<CommandError | null>(null);
  const [diffLoadVersion, setDiffLoadVersion] = useState(0);
  const [actionFailure, setActionFailure] = useState<ReviewerActionFailure | null>(null);
  const [selectedKey, setSelectedKey] = useState("");
  const [activeHunkKey, setActiveHunkKey] = useState("");
  const [collapsedFiles, setCollapsedFiles] = useState<Set<string>>(new Set());
  const [viewed, setViewed] = useState<Set<string>>(new Set());
  const [diffLoading, setDiffLoading] = useState(true);
  const [feedbackOpen, setFeedbackOpen] = useState(false);
  const [pendingAnchor, setPendingAnchor] = useState<Anchor | null>(null);
  const [pendingThreadId, setPendingThreadId] = useState<string | undefined>();
  const [pendingFeedbackDraft, setPendingFeedbackDraft] = useState("");
  const [pendingAskAnchor, setPendingAskAnchor] = useState<Anchor | null>(null);
  const [formalComments, setFormalComments] = useState<FormalComment[]>([]);
  const [inlineAskTurns, setInlineAskTurns] = useState<AskTurn[]>([]);
  const [viewMode, setViewMode] = useState<"unified" | "split" | "file">("unified");
  const [githubFiles, setGithubFiles] = useState<GithubMaterializedFile[]>([]);
  const [importedComments, setImportedComments] = useState<ImportedComment[]>([]);
  const [staleness, setStaleness] = useState<{ pinned_head_sha: string; observed_head_sha: string } | null>(null);
  const [publishAttempt, setPublishAttempt] = useState<GithubPublishAttempt | null>(null);
  const [githubDecision, setGithubDecision] = useState<"approve" | "request_changes" | null>(null);
  const [githubWorking, setGithubWorking] = useState(false);
  const readOnly = !isActive(round) || Boolean(round.superseded_by);
  const reason = round.superseded_by
    ? `Superseded by round ${shortSha(round.superseded_by)}`
    : round.lifecycle === "completed"
      ? "Completed — Requeue to review again"
      : "";
  const canPublish = supportsCapability(round, "publish");
  const hasUpstreamDiscussion = supportsCapability(round, "upstream_discussion");
  const canRefreshRemote = supportsCapability(round, "remote_refresh");
  const usesGithubMirror = round.source_adapter.adapter_id === "github_pull_request_mirror";
  const purgesOnApproval = round.source_adapter.approval === "purge_round";

  const loadViewedState = async () => {
    const viewedFiles = await listViewedFiles(round.id);
    setViewed(new Set(viewedFiles.map((file) => fileKey(file.repositoryId, file.path))));
  };
  const loadInlineState = async () => {
    const [comments, current, previous] = await Promise.all([
      listFormalComments(round.id),
      currentConversation(round.id),
      listPreviousChats(round.id),
    ]);
    const conversations = [current, ...previous].filter(
      (conversation): conversation is AskConversation => Boolean(conversation),
    );
    const turns = (await Promise.all(
      conversations.map((conversation) => listAskTurns(conversation.id)),
    )).flat();
    setFormalComments(comments);
    setInlineAskTurns(turns);
  };
  const setViewedValue = async (repositoryId: string, path: string, next: boolean) => {
    await setFileViewed(round.id, repositoryId, path, next);
    const key = fileKey(repositoryId, path);
    setViewed((current) => {
      const updated = new Set(current);
      if (next) updated.add(key); else updated.delete(key);
      return updated;
    });
  };
  const loadGithubDecision = async () => {
    setGithubDecision(await getRoundDecision(round.id));
  };
  const loadCachedGithubState = async () => {
    const cached = await cachedGithubRound(round.id);
    setImportedComments(cached.imported_comments);
    setStaleness(cached.last_staleness ?? null);
  };
  const refreshComments = async () => {
    const result = await refreshGithubComments(round.id);
    setImportedComments(result.imported);
    setStaleness(result.staleness);
  };
  const checkHead = async () => {
    setStaleness(await checkGithubStaleness(round.id));
  };
  const preparePublish = async () => {
    setPublishAttempt(await prepareGithubPublish(round.id));
  };
  const refreshRound = async () => {
    const result = await refreshGithubRound(round.id);
    await onGithubRoundRefreshed(result.round);
  };
  const runReviewerAction = async (
    recovery: ReviewerRecovery,
    action: () => Promise<void>,
    showWorking = false,
  ) => {
    setActionFailure((current) => current?.recovery.kind === recovery.kind ? null : current);
    if (showWorking) setGithubWorking(true);
    try {
      await action();
    } catch (problem) {
      setActionFailure({ error: toCommandError(problem), recovery });
    } finally {
      if (showWorking) setGithubWorking(false);
    }
  };
  const retryReviewerAction = async () => {
    if (!actionFailure) return;
    const { recovery } = actionFailure;
    switch (recovery.kind) {
      case "load_viewed":
        await runReviewerAction(recovery, loadViewedState);
        break;
      case "load_inline_state":
        await runReviewerAction(recovery, loadInlineState);
        break;
      case "set_viewed":
        await runReviewerAction(
          recovery,
          () => setViewedValue(recovery.repositoryId, recovery.path, recovery.viewed),
        );
        break;
      case "load_decision":
        await runReviewerAction(recovery, loadGithubDecision);
        break;
      case "load_cached_github":
        await runReviewerAction(recovery, loadCachedGithubState);
        break;
      case "refresh_comments":
        await runReviewerAction(recovery, refreshComments, true);
        break;
      case "check_head":
        await runReviewerAction(recovery, checkHead, true);
        break;
      case "prepare_publish":
        await runReviewerAction(recovery, preparePublish, true);
        break;
      case "refresh_round":
        await runReviewerAction(recovery, refreshRound, true);
        break;
    }
  };

  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const filesQuery = window.matchMedia("(max-width: 580px)");
    const chatQuery = window.matchMedia("(min-width: 1121px)");
    const updateFiles = (event: MediaQueryListEvent) => setSidebarCollapsed(event.matches);
    const updateChat = (event: MediaQueryListEvent) => setChatOpen(event.matches);
    filesQuery.addEventListener("change", updateFiles);
    chatQuery.addEventListener("change", updateChat);
    return () => {
      filesQuery.removeEventListener("change", updateFiles);
      chatQuery.removeEventListener("change", updateChat);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    setDiffLoading(true);
    setCoreDiffError(null);
    setDiff(null);
    setSelectedKey("");
    setGithubFiles([]);
    const load = usesGithubMirror
      ? openGithubPullRequest(round.id).then((opened) => {
          if (!cancelled) setGithubFiles(opened.files);
          return githubFilesToDiff(round, opened.files);
        })
      : materializeRoundDiff(round.id);
    load
      .then((materialized) => {
        if (cancelled) return;
        setDiff(materialized);
        setCoreDiffError(null);
      })
      .catch((problem) => {
        if (!cancelled) setCoreDiffError(toCommandError(problem));
      })
      .finally(() => {
        if (!cancelled) setDiffLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [diffLoadVersion, round.id, usesGithubMirror]);

  useEffect(() => {
    let cancelled = false;
    setActionFailure(null);
    setViewed(new Set());
    setGithubDecision(null);
    setImportedComments([]);
    setStaleness(null);
    setPublishAttempt(null);
    listViewedFiles(round.id)
      .then((viewedFiles) => {
        if (cancelled) return;
        setViewed(new Set(viewedFiles.map((file) => fileKey(file.repositoryId, file.path))));
      })
      .catch((problem) => {
        if (!cancelled) {
          setActionFailure({ error: toCommandError(problem), recovery: { kind: "load_viewed" } });
        }
      });
    if (!hasUpstreamDiscussion && !canPublish) {
      return () => {
        cancelled = true;
      };
    }
    if (canPublish) {
      getRoundDecision(round.id)
        .then((decision) => {
          if (!cancelled) setGithubDecision(decision);
        })
        .catch((problem) => {
          if (!cancelled) {
            setActionFailure({ error: toCommandError(problem), recovery: { kind: "load_decision" } });
          }
        });
    }
    if (hasUpstreamDiscussion) {
      cachedGithubRound(round.id)
        .then((cached) => {
          if (cancelled) return;
          setImportedComments(cached.imported_comments);
          setStaleness(cached.last_staleness ?? null);
        })
        .catch((problem) => {
          if (!cancelled) {
            setActionFailure({ error: toCommandError(problem), recovery: { kind: "load_cached_github" } });
          }
        });
    }
    return () => {
      cancelled = true;
    };
  }, [canPublish, hasUpstreamDiscussion, round.id]);

  useEffect(() => {
    let cancelled = false;
    setFormalComments([]);
    setInlineAskTurns([]);
    Promise.all([
      listFormalComments(round.id),
      currentConversation(round.id),
      listPreviousChats(round.id),
    ])
      .then(async ([comments, current, previous]) => {
        const conversations = [current, ...previous].filter(
          (conversation): conversation is AskConversation => Boolean(conversation),
        );
        const turns = (await Promise.all(
          conversations.map((conversation) => listAskTurns(conversation.id)),
        )).flat();
        if (cancelled) return;
        setFormalComments(comments);
        setInlineAskTurns(turns);
      })
      .catch((problem) => {
        if (!cancelled) {
          setActionFailure({
            error: toCommandError(problem),
            recovery: { kind: "load_inline_state" },
          });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [round.id]);

  const mergeConversationTurns = useCallback((conversationId: string, turns: AskTurn[]) => {
    setInlineAskTurns((current) => [
      ...current.filter((turn) => turn.conversation_id !== conversationId),
      ...turns,
    ].sort((left, right) => left.created_at.localeCompare(right.created_at)));
  }, []);

  const files = useMemo(
    () => (diff?.repositories ?? []).flatMap((repository) =>
      repository.files.map((file) => ({
        repository,
        file,
        path: file.new_path ?? file.old_path ?? "(unknown path)",
      }))),
    [diff],
  );
  const commentCountsByFile = useMemo(() => {
    const counts = new Map<string, number>();
    const anchors = [
      ...formalComments.map((comment) => comment.anchor),
      ...importedComments.map((comment) => comment.anchor),
    ].filter((item): item is Anchor => Boolean(item));
    for (const item of anchors) {
      const match = files.find(({ repository, file, path }) => {
        const workspacePath = repository.root === "." ? path : `${repository.root}/${path}`;
        return file.repository_id === item.repository_id
          && workspacePath === item.workspace_relative_path;
      });
      if (!match) continue;
      const key = fileKey(match.file.repository_id, match.path);
      counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    return counts;
  }, [files, formalComments, importedComments]);
  const hunkTargets = useMemo(() => files.flatMap(({ file, path }, fileIndex) => {
    const key = fileKey(file.repository_id, path);
    return file.hunks.map((_, hunkIndex) => ({
      key: `${key}:${hunkIndex}`,
      fileKey: key,
      fileIndex,
      hunkIndex,
      path,
      elementId: `review-queue-hunk-${fileIndex}-${hunkIndex}`,
    }));
  }), [files]);
  const activeHunkPosition = hunkTargets.findIndex((target) => target.key === activeHunkKey);
  const activeHunkTarget = activeHunkPosition >= 0 ? hunkTargets[activeHunkPosition] : null;
  const selected = files.find(({ file, path }) =>
    fileKey(file.repository_id, path) === selectedKey) ?? files[0];
  const diffFileElements = useRef(new Map<string, HTMLElement>());

  useEffect(() => {
    setCollapsedFiles(new Set());
    setActiveHunkKey("");
  }, [round.id]);

  useEffect(() => {
    if (!hunkTargets.length || activeHunkPosition >= 0) return;
    setActiveHunkKey(hunkTargets[0].key);
  }, [activeHunkPosition, hunkTargets]);

  useEffect(() => {
    if (!selectedKey || viewMode === "file") return;
    const element = diffFileElements.current.get(selectedKey);
    if (!element) return;
    const frame = window.requestAnimationFrame(() => {
      element.scrollIntoView({ block: "start", behavior: "smooth" });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [selectedKey, viewMode]);

  const selectDiffFile = (key: string) => {
    setCollapsedFiles((current) => {
      if (!current.has(key)) return current;
      const next = new Set(current);
      next.delete(key);
      return next;
    });
    setSelectedKey(key);
    const firstHunk = hunkTargets.find((target) => target.fileKey === key);
    if (firstHunk) setActiveHunkKey(firstHunk.key);
  };

  const navigateToHunk = (position: number) => {
    const target = hunkTargets[Math.max(0, Math.min(position, hunkTargets.length - 1))];
    if (!target) return;
    setActiveHunkKey(target.key);
    setSelectedKey(target.fileKey);
    setCollapsedFiles((current) => {
      if (!current.has(target.fileKey)) return current;
      const next = new Set(current);
      next.delete(target.fileKey);
      return next;
    });
    window.requestAnimationFrame(() => {
      document.getElementById(target.elementId)?.scrollIntoView({ block: "center", behavior: "smooth" });
    });
  };
  const navigateDiffModeTabs = (event: React.KeyboardEvent<HTMLDivElement>) => {
    const modes = ["unified", "split"] as const;
    const current = viewMode === "split" ? 1 : 0;
    let next = current;
    if (event.key === "ArrowLeft" || event.key === "ArrowUp") next = (current + modes.length - 1) % modes.length;
    else if (event.key === "ArrowRight" || event.key === "ArrowDown") next = (current + 1) % modes.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = modes.length - 1;
    else return;
    event.preventDefault();
    setViewMode(modes[next]);
    window.requestAnimationFrame(() => {
      document.getElementById(`diff-view-${modes[next]}`)?.focus();
    });
  };

  const toggleDiffFileCollapsed = (key: string) => {
    setCollapsedFiles((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const toggleViewedFor = async (repositoryId: string, path: string) => {
    if (readOnly) return;
    const key = fileKey(repositoryId, path);
    const next = !viewed.has(key);
    await runReviewerAction(
      { kind: "set_viewed", repositoryId, path, viewed: next },
      () => setViewedValue(repositoryId, path, next),
    );
  };
  const toggleViewed = () => selected
    ? toggleViewedFor(selected.file.repository_id, selected.path)
    : Promise.resolve();

  const aggregateStats = useMemo(() => {
    let additions = 0;
    let deletions = 0;
    for (const { file } of files) {
      const counts = diffLineCounts(file);
      additions += counts.additions;
      deletions += counts.deletions;
    }
    return { additions, deletions };
  }, [files]);
  const viewedCount = files.filter(({ file, path }) => viewed.has(fileKey(file.repository_id, path))).length;
  const totalFiles = files.length;
  const viewedPercent = totalFiles ? Math.round((viewedCount / totalFiles) * 100) : 0;
  const roundDiscussion = importedComments.filter((comment) => !comment.anchor);
  const roundFormalComments = formalComments.filter((comment) => !comment.anchor);

  return (
    <main className="reviewer">
      <header className={`review-header ${canPublish || hasUpstreamDiscussion ? "github-review-header" : ""}`}>
        <button className="back" onClick={onBack}>← Queue Home</button>
        <div className="review-identity">
          <b>{round.manifest.topic}</b>
          <span> · {round.manifest.repositories.length} repositories @ {shortSha(round.manifest_hash)}</span>
        </div>
        <button onClick={onDetails}>Details</button>
        {(hasUpstreamDiscussion || canPublish || (canRefreshRemote && usesGithubMirror)) && (
          <div className="review-header-actions" role="group" aria-label="GitHub review actions">
            {hasUpstreamDiscussion && (
              <button
                disabled={githubWorking}
                onClick={() => void runReviewerAction({ kind: "refresh_comments" }, refreshComments, true)}
              >Refresh comments</button>
            )}
            {canRefreshRemote && usesGithubMirror && (
              <button
                disabled={githubWorking}
                onClick={() => void runReviewerAction({ kind: "check_head" }, checkHead, true)}
              >Check head</button>
            )}
            {canPublish && (
              <button
                disabled={githubWorking || readOnly || !githubDecision}
                title={!githubDecision ? "Record Approve or Request changes before publishing" : readOnly ? reason : "Preview the exact GitHub review request"}
                onClick={() => void runReviewerAction({ kind: "prepare_publish" }, preparePublish, true)}
              >Publish review</button>
            )}
          </div>
        )}
      </header>
      <ReviewBriefView brief={round.brief} />
      {actionFailure && (
        <div className="reviewer-action-error">
          <ErrorPanel error={actionFailure.error} onRetry={retryReviewerAction} />
          <button onClick={() => setActionFailure(null)}>Dismiss</button>
        </div>
      )}
      {(hasUpstreamDiscussion || roundFormalComments.length > 0) && (
        <section className="upstream-discussion">
          {staleness && staleness.pinned_head_sha !== staleness.observed_head_sha && (
            <p className="danger-text">
              Head moved from {shortSha(staleness.pinned_head_sha)} to {shortSha(staleness.observed_head_sha)}.{" "}
              <button
                disabled={githubWorking}
                onClick={() => void runReviewerAction({ kind: "refresh_round" }, refreshRound, true)}
              >Refresh into new round</button>
            </p>
          )}
          <details>
            <summary>Round discussion ({roundDiscussion.length + roundFormalComments.length})</summary>
            {roundDiscussion.length === 0 && roundFormalComments.length === 0 && (
              <p className="muted">No PR-level discussion or round-level formal comments.</p>
            )}
            {roundDiscussion.map((comment) => (
              <details
                className={`imported-discussion ${comment.upstream_resolved ? "resolved-upstream" : ""}`}
                key={comment.id}
                open={!comment.upstream_resolved}
              >
                <summary>{importedDiscussionLabel(comment)}</summary>
                <article>
                  <b>{comment.upstream_author}</b> · <time>{new Date(comment.upstream_created_at).toLocaleString()}</time>
                  {comment.anchor && <small>{comment.anchor.workspace_relative_path}:{comment.anchor.start_line}</small>}
                  {comment.body ? <p>{comment.body}</p> : <p className="muted">No review summary text.</p>}
                  <a href={comment.source_url} target="_blank" rel="noreferrer">Open on GitHub</a>
                </article>
              </details>
            ))}
            {roundFormalComments.map((comment) => (
              <article className="formal-comment" key={comment.id}>
                <small>Formal round comment · revision {comment.revision}</small>
                <p>{comment.body}</p>
              </article>
            ))}
          </details>
        </section>
      )}
      <div className="review-toolbar">
        <div className="toolbar-brand" aria-hidden="true">RQ</div>
        <button
          className="pane-toggle"
          aria-label={sidebarCollapsed ? "Open files" : "Close files"}
          aria-expanded={!sidebarCollapsed}
          aria-controls="review-files"
          onClick={() => setSidebarCollapsed((value) => !value)}
        >
          <span aria-hidden="true">☰</span>
          <span>Files</span>
        </button>
        <button className="icon-button" aria-label="Settings" onClick={onSettings}>⚙</button>
        <div className="view-modes toolbar-view-modes" role="tablist" aria-label="Diff layout" onKeyDown={navigateDiffModeTabs}>
          {(["unified", "split"] as const).map((mode) => (
            <button
              id={`diff-view-${mode}`}
              role="tab"
              className={viewMode === mode || (viewMode === "file" && mode === "unified") ? "selected-mode" : ""}
              aria-selected={viewMode === mode || (viewMode === "file" && mode === "unified")}
              aria-controls="review-diff-panel"
              tabIndex={viewMode === mode || (viewMode === "file" && mode === "unified") ? 0 : -1}
              key={mode}
              onClick={() => setViewMode(mode)}
            >
              {mode}
            </button>
          ))}
        </div>
        <nav className="global-hunk-navigation" aria-label="Review hunk navigation">
          <button
            aria-label="Previous hunk in review"
            disabled={activeHunkPosition <= 0 || viewMode === "file"}
            onClick={() => navigateToHunk(activeHunkPosition - 1)}
          >↑</button>
          <span aria-live="polite">
            {hunkTargets.length
              ? `${Math.max(0, activeHunkPosition) + 1} / ${hunkTargets.length} hunks`
              : "No hunks"}
          </span>
          <button
            aria-label="Next hunk in review"
            disabled={activeHunkPosition < 0 || activeHunkPosition >= hunkTargets.length - 1 || viewMode === "file"}
            onClick={() => navigateToHunk(activeHunkPosition + 1)}
          >↓</button>
        </nav>
        <div
          className="viewed-progress"
          aria-label={`${viewedCount} of ${totalFiles} files viewed`}
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={totalFiles}
          aria-valuenow={viewedCount}
        >
          <span>{viewedCount} / {totalFiles} files viewed</span>
          <div className="progress-track">
            <div className="progress-fill" style={{ width: `${viewedPercent}%` }} />
          </div>
        </div>
        <button
          className="pane-toggle"
          aria-label={chatOpen ? "Close chat" : "Open chat"}
          aria-expanded={chatOpen}
          aria-controls="round-chat"
          onClick={() => setChatOpen((value) => !value)}
        >
          <span aria-hidden="true">◧</span>
          <span>Chat</span>
        </button>
        <span className="revision-pill" title={`Manifest ${round.manifest_hash}`}>{shortSha(round.manifest_hash)}</span>
      </div>
      <div className={`workspace snapshot-workspace ${sidebarCollapsed ? "sidebar-collapsed" : ""} ${chatOpen ? "chat-open" : "chat-collapsed"}`}>
        <aside
          className="files"
          id="review-files"
          hidden={sidebarCollapsed}
          aria-hidden={sidebarCollapsed}
        >
          <div className="pane-title">Repositories</div>
          <div className="files-summary">
            <span>{totalFiles} file{totalFiles === 1 ? "" : "s"} changed</span>
            <span className="diff-stat">
              <span className="added">+{aggregateStats.additions}</span>{" "}
              <span className="removed">-{aggregateStats.deletions}</span>
            </span>
          </div>
          <RepositoryFileTree
            repositories={diff?.repositories ?? []}
            selectedKey={selected ? fileKey(selected.file.repository_id, selected.path) : null}
            viewedKeys={viewed}
            commentCounts={commentCountsByFile}
            viewedDisabled={readOnly}
            filterPlaceholder="Filter paths"
            onSelect={(entry) => selectDiffFile(entry.key)}
            onToggleViewed={(entry) => {
              void toggleViewedFor(entry.repositoryId, entry.path);
            }}
          />
        </aside>
        <section className="diff snapshot-pane" id="review-diff-panel" role="tabpanel" aria-label="Immutable diff">
          <div className="diff-head">
            <div className="diff-head-title">
              <b>{selected?.path ?? "Immutable review snapshot"}</b>
              {selected ? (
                <>
                  <span className={`status-badge status-${selected.file.status}`}>{selected.file.status}</span>
                  {(() => {
                    const counts = diffLineCounts(selected.file);
                    return (
                      <span className="diff-stat">
                        {counts.additions > 0 && <span className="added">+{counts.additions}</span>}
                        {counts.deletions > 0 && <span className="removed">-{counts.deletions}</span>}
                      </span>
                    );
                  })()}
                </>
              ) : (
                <span>{round.collection}</span>
              )}
            </div>
            <button
              className={viewMode === "file" ? "full-file-toggle selected-mode" : "full-file-toggle"}
              aria-pressed={viewMode === "file"}
              onClick={() => setViewMode(viewMode === "file" ? "unified" : "file")}
            >
              Full file
            </button>
            <button
              className={selected && viewed.has(fileKey(selected.file.repository_id, selected.path)) ? "viewed-toggle viewed" : "viewed-toggle"}
              disabled={!selected || readOnly}
              title={readOnly ? reason : ""}
              onClick={() => void toggleViewed()}
            >
              {selected && viewed.has(fileKey(selected.file.repository_id, selected.path)) ? "✓ Viewed" : "Mark viewed"}
            </button>
          </div>
          {diffLoading && <p className="loading-state">Materializing pinned commits…</p>}
          {coreDiffError && (
            <ErrorPanel
              error={coreDiffError}
              onRetry={async () => setDiffLoadVersion((version) => version + 1)}
            />
          )}
          {!diffLoading && !coreDiffError && viewMode !== "file" && files.length > 0 && (
            <div className="diff-scroll" aria-label="Changed file diffs">
              {files.map(({ repository, file, path }, index) => {
                const key = fileKey(file.repository_id, path);
                const counts = diffLineCounts(file);
                const isCollapsed = collapsedFiles.has(key);
                const isViewed = viewed.has(key);
                const isSelected = selected?.file === file && selected?.path === path;
                return (
                  <article
                    className={`continuous-diff-file ${isSelected ? "selected-diff-file" : ""}`}
                    data-selected={isSelected || undefined}
                    key={key}
                    ref={(element) => {
                      if (element) diffFileElements.current.set(key, element);
                      else diffFileElements.current.delete(key);
                    }}
                  >
                    <header className="continuous-diff-file-head">
                      <div className="continuous-diff-file-title">
                        <button
                          className="continuous-diff-collapse"
                          aria-expanded={!isCollapsed}
                          aria-label={`${isCollapsed ? "Expand" : "Collapse"} ${repository.root}/${path}`}
                          onClick={() => toggleDiffFileCollapsed(key)}
                        >
                          <span aria-hidden="true">{isCollapsed ? "▸" : "▾"}</span>
                        </button>
                        <button
                          className="continuous-diff-path"
                          aria-current={isSelected ? "true" : undefined}
                          onClick={() => selectDiffFile(key)}
                          title={`${repository.root}/${path}`}
                        >
                          {repository.root}/{path}
                        </button>
                        <span className={`status-badge status-${file.status}`}>{file.status}</span>
                      </div>
                      <div className="continuous-diff-file-actions">
                        <span className="diff-stat">
                          {counts.additions > 0 && <span className="added">+{counts.additions}</span>}
                          {counts.deletions > 0 && <span className="removed">-{counts.deletions}</span>}
                        </span>
                        <button
                          className={isViewed ? "viewed-toggle viewed" : "viewed-toggle"}
                          disabled={readOnly}
                          title={readOnly ? reason : isViewed ? "Mark file not viewed" : "Mark file viewed"}
                          onClick={() => void toggleViewedFor(file.repository_id, path)}
                        >
                          {isViewed ? "✓ Viewed" : "Mark viewed"}
                        </button>
                      </div>
                    </header>
                    {!isCollapsed && (
                      <DiffFileView
                        file={file}
                        fileIndex={index}
                        continuous
                        activeHunk={activeHunkTarget?.fileKey === key ? activeHunkTarget.hunkIndex : null}
                        onActiveHunkChange={(hunkIndex) => {
                          const position = hunkTargets.findIndex((target) =>
                            target.fileKey === key && target.hunkIndex === hunkIndex);
                          if (position >= 0) navigateToHunk(position);
                        }}
                        layout={viewMode}
                        repositoryRoot={repository.root}
                        importedComments={importedComments}
                        formalComments={formalComments}
                        askTurns={inlineAskTurns}
                        readOnly={readOnly}
                        onComment={(anchor, threadId) => {
                          setPendingAnchor(anchor);
                          setPendingThreadId(threadId);
                          setPendingFeedbackDraft("");
                          setFeedbackOpen(true);
                        }}
                        onAsk={(anchor) => {
                          setPendingAskAnchor(anchor);
                          setChatOpen(true);
                        }}
                        onOpenAskTurn={(anchor) => {
                          setPendingAskAnchor(anchor);
                          setChatOpen(true);
                        }}
                        onConvertAskTurn={(turn) => {
                          setPendingAnchor(turn.anchor ?? null);
                          setPendingThreadId(undefined);
                          setPendingFeedbackDraft(turn.response_text ?? "");
                          setFeedbackOpen(true);
                        }}
                      />
                    )}
                  </article>
                );
              })}
            </div>
          )}
          {!diffLoading && !coreDiffError && selected && viewMode === "file" && (
            <PinnedFilePane
              round={round}
              selected={selected}
              githubFile={githubFiles.find((file) => file.path === selected.path)}
            />
          )}
          {!diffLoading && !coreDiffError && !selected && (
            <p className="loading-state">No changed files in this review round.</p>
          )}
          <div className="decision">
            <span>{readOnly ? reason : "Formal review"}</span>
            <button
              className="approve"
              disabled={readOnly}
              title={readOnly ? reason : purgesOnApproval ? "Approval purges this local round after confirmation" : "Records a local decision; it does not publish or deliver"}
              onClick={() => purgesOnApproval
                ? onPurge("approve_local")
                : void onApproveRemote().then((saved) => {
                    if (saved) setGithubDecision("approve");
                  })}
            >
              Approve
            </button>
            <button
              className="changes"
              disabled={readOnly}
              title={readOnly ? reason : ""}
              onClick={() => void onRequestChanges().then((saved) => {
                if (saved) setGithubDecision("request_changes");
              })}
            >
              Request changes
            </button>
            <button
              title={readOnly ? "Inspect and copy saved formal feedback history" : ""}
              onClick={() => {
                setPendingAnchor(null);
                setPendingThreadId(undefined);
                setPendingFeedbackDraft("");
                setFeedbackOpen(true);
              }}
            >
              Formal feedback
            </button>
            <button disabled={readOnly} onClick={onComplete}>Complete</button>
          </div>
        </section>
        <ChatSheet
          round={round}
          open={chatOpen}
          onClose={() => setChatOpen(false)}
          readOnly={readOnly}
          readOnlyReason={reason}
          pendingAnchor={pendingAskAnchor}
          onAnchorConsumed={() => setPendingAskAnchor(null)}
          onTurnsChange={mergeConversationTurns}
        />
      </div>
      {feedbackOpen && (
        <FormalFeedbackDrawer
          round={round}
          initialAnchor={pendingAnchor}
          initialThreadId={pendingThreadId}
          initialDraft={pendingFeedbackDraft}
          readOnly={readOnly}
          readOnlyReason={reason}
          onCommentsChange={setFormalComments}
          onClose={() => {
            setFeedbackOpen(false);
            setPendingAnchor(null);
            setPendingThreadId(undefined);
            setPendingFeedbackDraft("");
          }}
          onReproduce={() => {
            setFeedbackOpen(false);
            setPendingAnchor(null);
            setPendingThreadId(undefined);
            setPendingFeedbackDraft("");
            onReproduce();
          }}
        />
      )}
      {publishAttempt && (
        <GithubPublishDialog
          attempt={publishAttempt}
          onClose={() => setPublishAttempt(null)}
          onConfirm={async () => {
            setPublishAttempt(await publishGithub(publishAttempt.id));
          }}
        />
      )}
    </main>
  );
}

function ChatSheet({
  round,
  open,
  onClose,
  readOnly,
  readOnlyReason,
  pendingAnchor,
  onAnchorConsumed,
  onTurnsChange,
}: {
  round: ReviewRound;
  open: boolean;
  onClose: () => void;
  readOnly: boolean;
  readOnlyReason: string;
  pendingAnchor: Anchor | null;
  onAnchorConsumed: () => void;
  onTurnsChange: (conversationId: string, turns: AskTurn[]) => void;
}) {
  const [active, setActive] = useState<AskConversation | null>(null);
  const [previous, setPrevious] = useState<AskConversation[]>([]);
  const [shown, setShown] = useState<AskConversation | null>(null);
  const [turns, setTurns] = useState<AskTurn[]>([]);
  const [error, setError] = useState<CommandError | null>(null);
  const [loading, setLoading] = useState(true);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [optionValues, setOptionValues] = useState<Record<string, string>>({});
  const [prompt, setPrompt] = useState("");
  const [streamingTurnId, setStreamingTurnId] = useState<string | null>(null);
  const cancelledTurnIds = useRef(new Set<string>());
  const promptSubmissionInFlight = useRef(false);
  const [submittingPrompt, setSubmittingPrompt] = useState(false);
  const [starting, setStarting] = useState(false);
  const [authLabel, setAuthLabel] = useState("");
  const [capabilities, setCapabilities] = useState<CopilotCapabilities | null>(null);

  useEffect(() => {
    if (!shown || turns.some((turn) => turn.conversation_id !== shown.id)) return;
    onTurnsChange(shown.id, turns);
  }, [onTurnsChange, shown, turns]);

  const loadConversation = useCallback(async (conversation: AskConversation) => {
    setShown(conversation);
    setTurns(await listAskTurns(conversation.id));
  }, []);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      if (readOnly || !open) {
        const [current, history] = await Promise.all([
          currentConversation(round.id),
          listPreviousChats(round.id),
        ]);
        setActive(current);
        setPrevious(history);
        setOptionValues({});
        setCapabilities(null);
        setAuthLabel(current?.provider_session_label ?? "");
        const transcript = current ?? history[0] ?? null;
        if (transcript) await loadConversation(transcript);
        else {
          setShown(null);
          setTurns([]);
        }
        setError(null);
        return;
      }
      const capabilities = await copilotCapabilities();
      const options = capabilitySessionOptions(capabilities.option_groups);
      const [current, history] = await Promise.all([
        activeConversation(round.id, options),
        listPreviousChats(round.id),
      ]);
      setActive(current);
      setPrevious(history);
      setCapabilities(capabilities);
      setAuthLabel(current.provider_session_label ?? "");
      setOptionValues(Object.fromEntries(current.options.filter((option) => option.selected).map((option) => [option.key, option.selected as string])));
      await loadConversation(current);
      setError(null);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setLoading(false);
    }
  }, [loadConversation, open, readOnly, round.id]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const clear = async () => {
    try {
      if (readOnly || !active) return;
      const next = await clearCopilotChat(round.id, active.id);
      const history = await listPreviousChats(round.id);
      setSessionId(null);
      setAuthLabel("");
      setActive(next);
      setPrevious(history);
      await loadConversation(next);
      setError(null);
    } catch (problem) {
      setError(toCommandError(problem));
    }
  };

  const startSession = async (requestedOptions = optionValues) => {
    if (readOnly || !active) return;
    setStarting(true);
    setError(null);
    try {
      const session = await startCopilotSession(round.id, active.id, requestedOptions);
      setSessionId(session.sessionId);
      setOptionValues(session.activeOptions);
      setAuthLabel(`${session.authSource === "existing_cli_sign_in_read_only" ? "existing Copilot CLI sign-in" : "app OAuth"}${session.account ? ` · ${session.account}` : ""}`);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setStarting(false);
    }
  };

  const pollUntilDone = async (turnId: string, conversation: AskConversation) => {
    try {
      for (;;) {
        if (cancelledTurnIds.current.has(turnId)) break;
        const result = await pollCopilotPrompt(turnId);
        if (cancelledTurnIds.current.has(turnId)) break;
        setTurns((current) => {
          const without = current.filter((turn) => turn.id !== result.turn.id);
          return [...without, result.turn].sort((a, b) => a.created_at.localeCompare(b.created_at));
        });
        if (result.update.state !== "chunk") break;
      }
    } catch (problem) {
      if (cancelledTurnIds.current.has(turnId)) {
        setError(null);
      } else {
        setError(toCommandError(problem));
        await loadConversation(conversation);
      }
    } finally {
      cancelledTurnIds.current.delete(turnId);
      setStreamingTurnId(null);
    }
  };

  const send = async (event: React.FormEvent, retry?: AskTurn) => {
    event.preventDefault();
    if (readOnly || !active || !sessionId) return;
    const text = retry?.prompt ?? prompt.trim();
    if (!text || promptSubmissionInFlight.current) return;
    promptSubmissionInFlight.current = true;
    setSubmittingPrompt(true);
    setError(null);
    try {
      const turn = await sendCopilotPrompt(
        round.id,
        active.id,
        text,
        retry?.anchor ?? pendingAnchor,
        optionValues,
      );
      setTurns((current) => [...current.filter((item) => item.id !== turn.id), turn]);
      setPrompt("");
      onAnchorConsumed();
      setStreamingTurnId(turn.id);
      void pollUntilDone(turn.id, active);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      promptSubmissionInFlight.current = false;
      setSubmittingPrompt(false);
    }
  };

  const retryInFreshChat = async (turn: AskTurn) => {
    if (
      readOnly
      || !active
      || shown?.id !== active.id
      || shown.session_state === "history_only"
      || turn.conversation_id !== active.id
      || starting
      || streamingTurnId
      || promptSubmissionInFlight.current
    ) return;
    promptSubmissionInFlight.current = true;
    setSubmittingPrompt(true);
    setStarting(true);
    setError(null);
    try {
      const next = await clearCopilotChat(round.id, active.id);
      const history = await listPreviousChats(round.id);
      const retryOptions = Object.fromEntries(
        next.options
          .filter((option) => option.selected)
          .map((option) => [option.key, option.selected as string]),
      );
      const session = await startCopilotSession(round.id, next.id, retryOptions);
      const retried = await sendCopilotPrompt(
        round.id,
        next.id,
        turn.prompt,
        turn.anchor ?? null,
        session.activeOptions,
      );
      setActive(next);
      setShown(next);
      setPrevious(history);
      setTurns([retried]);
      setSessionId(session.sessionId);
      setOptionValues(session.activeOptions);
      setAuthLabel(`${session.authSource === "existing_cli_sign_in_read_only" ? "existing Copilot CLI sign-in" : "app OAuth"}${session.account ? ` · ${session.account}` : ""}`);
      setStreamingTurnId(retried.id);
      void pollUntilDone(retried.id, next);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      promptSubmissionInFlight.current = false;
      setSubmittingPrompt(false);
      setStarting(false);
    }
  };

  const providerLost = !sessionId && turns.length > 0 && shown?.id === active?.id;
  const historyOnly = shown?.id !== active?.id || shown?.session_state === "history_only" || providerLost || readOnly;
  const canRetryTurn = (turn: AskTurn) =>
    !readOnly
    && Boolean(active)
    && shown?.id === active?.id
    && shown?.session_state !== "history_only"
    && turn.conversation_id === active?.id;
  const retryTitle = (turn: AskTurn) => {
    if (readOnly) return readOnlyReason || "This historical round is read-only.";
    if (!canRetryTurn(turn)) {
      return "Archived chats are permanently read-only. Return to Current chat to continue.";
    }
    return historyOnly || !sessionId
      ? "Archives the current transcript, starts a fresh session, and sends a new prompt only after this click."
      : "Creates a new prompt turn; the original request is never replayed automatically.";
  };
  const currentConversationShown = shown?.id === active?.id;
  const displayedOptions = currentConversationShown && capabilities
    ? capabilitySessionOptions(capabilities.option_groups)
    : shown?.options ?? [];
  const unavailableSelections = !sessionId && currentConversationShown && capabilities
    ? unavailableCopilotOptionSelections(capabilities.option_groups, optionValues)
    : [];
  const availableOptionValues = capabilities
    ? availableCopilotOptionValues(capabilities.option_groups, optionValues)
    : optionValues;
  const inputReason = readOnly
    ? readOnlyReason
    : historyOnly
      ? shown?.history_only_reason ?? "This chat's session has ended"
      : sessionId
        ? pendingAnchor
          ? `Ask about ${pendingAnchor.workspace_relative_path}:${pendingAnchor.start_line}–${pendingAnchor.end_line}`
          : "Ask a follow-up…"
        : "Start a Copilot session before asking";

  return (
    <aside
      className={`chat ${open ? "is-open" : ""}`}
      id="round-chat"
      aria-label="Round chat"
      aria-hidden={!open}
    >
      <header>
        <div><b>Chat</b><small> · {shortSha(round.id)}</small></div>
        <div className="chat-header-actions">
          <span className="session-state">{historyOnly ? "history only" : authLabel || "ready to start"}</span>
          <button className="chat-close" aria-label="Close chat" onClick={onClose}>×</button>
        </div>
      </header>
      <div className="chat-actions">
        {previous.length > 0 && (
          <select
            aria-label="Previous chats"
            value={shown?.id ?? ""}
            onChange={(event) => {
              const conversation = [active, ...previous].find((item) => item?.id === event.target.value);
              if (conversation) void loadConversation(conversation);
            }}
          >
            {active && <option value={active.id}>Current chat</option>}
            {previous.map((conversation, index) => (
              <option value={conversation.id} key={conversation.id}>Previous chat {previous.length - index}</option>
            ))}
          </select>
        )}
        <button
          disabled={readOnly || shown?.id !== active?.id || loading}
          title={readOnly ? readOnlyReason : "Archive this transcript and open an empty chat. Sends no prompt."}
          onClick={() => void clear()}
        >
          Clear chat
        </button>
        {!sessionId && !historyOnly && (
          <button
            className="primary"
            disabled={starting || !active}
            onClick={() => void startSession(unavailableSelections.length ? availableOptionValues : optionValues)}
          >
            {starting
              ? "Starting…"
              : unavailableSelections.length
                ? "Reset unavailable options and start Copilot"
                : "Start Copilot"}
          </button>
        )}
      </div>
      {unavailableSelections.length > 0 && (
        <div className="stale-options" role="alert">
          <b>Unavailable saved Copilot options</b>
          {unavailableSelections.map((selection) => (
            <p key={selection.key}>
              <code>{selection.key}={selection.value}</code> · {selection.reason}
            </p>
          ))}
          <small>
            These saved values will not be sent. Resetting starts with the current advertised
            options and sends zero prompts.
          </small>
        </div>
      )}
      {displayedOptions.length ? (
        <div className="options">
          {displayedOptions.map((option) => (
            <label key={option.key}>
              {option.label}
              {option.supported ? (
                <select
                  disabled={historyOnly}
                  value={optionValues[option.key] ?? option.selected ?? ""}
                  onChange={(event) => {
                    const value = event.target.value;
                    const conversationId = active?.id;
                    if (!conversationId) return;
                    if (!sessionId) {
                      setOptionValues((current) => ({ ...current, [option.key]: value }));
                      return;
                    }
                    void changeCopilotOption(round.id, conversationId, option.key, value)
                      .then((result) => {
                        if (result.effect === "requires_fresh_session") {
                          setOptionValues((current) => ({ ...current, [option.key]: value }));
                          setError({
                            code: "copilot_fresh_session_required",
                            message: `${option.label} requires a fresh Copilot session.`,
                            data_safety: "The current session and transcript were preserved.",
                            next_step: "Choose Clear chat, then select the option before starting the new session.",
                          });
                        } else {
                          setOptionValues(result.active_option_stamp);
                        }
                      })
                      .catch((problem) => setError(toCommandError(problem)));
                  }}
                >
                  {option.values.map((value) => (
                    <option key={value}>{value}</option>
                  ))}
                </select>
              ) : (
                <span className="option-unavailable">Unavailable</span>
              )}
              {!option.supported && <small>{option.unavailable_reason}</small>}
            </label>
          ))}
        </div>
      ) : (
        <p className="session">Copilot adapter not connected · opening and history issue zero prompts.</p>
      )}
      <div className="messages">
        {loading && <p>Loading saved transcript…</p>}
        {!loading && !turns.length && <p>Select code and <code>/ask</code> to start.</p>}
        {turns.map((turn) => (
          <article key={turn.id} className="chat-turn ask-thread">
            <small className="ask-label">You · {turn.anchor ? `${turn.anchor.workspace_relative_path}:${turn.anchor.start_line}–${turn.anchor.end_line} · ${turn.anchor.side}` : "round follow-up"}</small>
            {turn.anchor?.selected_code && <pre className="anchor-snippet">{turn.anchor.selected_code}</pre>}
            <p>{turn.prompt}</p>
            <small>Copilot · {turn.state}{Object.keys(turn.option_values).length ? ` · ${Object.entries(turn.option_values).map(([key, value]) => `${key}: ${value}`).join(" · ")}` : ""}</small>
            {turn.response_text && <p>{turn.response_text}</p>}
            {turn.failure_reason && (
              <p className="danger-text">
                {turn.failure_reason}{" "}
                <button
                  disabled={!canRetryTurn(turn) || starting || submittingPrompt || Boolean(streamingTurnId)}
                  title={retryTitle(turn)}
                  onClick={(event) => {
                    if (historyOnly || !sessionId) void retryInFreshChat(turn);
                    else void send(event, turn);
                  }}
                >
                  Retry as new prompt
                </button>
              </p>
            )}
            {(turn.state === "cancelled" || turn.state === "interrupted") && !turn.failure_reason && (
              <button
                disabled={!canRetryTurn(turn) || starting || submittingPrompt || Boolean(streamingTurnId)}
                title={retryTitle(turn)}
                onClick={(event) => {
                  if (historyOnly || !sessionId) void retryInFreshChat(turn);
                  else void send(event, turn);
                }}
              >
                Retry as new prompt
              </button>
            )}
          </article>
        ))}
        {error && <ErrorPanel error={error} />}
      </div>
      <form onSubmit={(event) => void send(event)}>
        <input
          disabled={historyOnly || !sessionId || submittingPrompt || Boolean(streamingTurnId)}
          aria-label="Ask a follow-up"
          placeholder={inputReason}
          value={prompt}
          onChange={(event) => setPrompt(event.target.value)}
        />
        {streamingTurnId ? (
          <button type="button" onClick={() => {
            cancelledTurnIds.current.add(streamingTurnId);
            void cancelCopilotPrompt(streamingTurnId).then((turn) => {
              setTurns((current) => current.map((item) => item.id === turn.id ? turn : item));
              setStreamingTurnId(null);
              setError(null);
            }).catch((problem) => {
              cancelledTurnIds.current.delete(streamingTurnId);
              setError(toCommandError(problem));
            });
          }}>Cancel</button>
        ) : (
          <button disabled={historyOnly || !sessionId || submittingPrompt || !prompt.trim()} title={inputReason}>Send</button>
        )}
      </form>
    </aside>
  );
}

function FormalFeedbackDrawer({
  round,
  initialAnchor,
  initialThreadId,
  initialDraft,
  readOnly,
  readOnlyReason,
  onCommentsChange,
  onClose,
  onReproduce,
}: {
  round: ReviewRound;
  initialAnchor: Anchor | null;
  initialThreadId?: string;
  initialDraft: string;
  readOnly: boolean;
  readOnlyReason: string;
  onCommentsChange: (comments: FormalComment[]) => void;
  onClose: () => void;
  onReproduce: () => void;
}) {
  const [comments, setComments] = useState<FormalComment[]>([]);
  const [history, setHistory] = useState<DeliveryHistoryEntry[]>([]);
  const [draft, setDraft] = useState(initialDraft);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editBody, setEditBody] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<CommandError | null>(null);
  const [copied, setCopied] = useState(false);
  const [copiedDeliveryId, setCopiedDeliveryId] = useState<string | null>(null);
  const [routes, setRoutes] = useState<AgentRoute[]>([]);
  const [routeId, setRouteId] = useState(round.origin_route_id ?? "");
  const [decision, setDecision] = useState<"approve" | "request_changes" | null>(null);
  const [prepared, setPrepared] = useState<PreparedFeedbackPrompt | null>(null);
  const [working, setWorking] = useState(false);
  const [deliveryMessage, setDeliveryMessage] = useState("");
  const dialog = useDialogFocus(onClose);

  const refreshComments = useCallback(async () => {
    try {
      const [savedComments, savedDecision, savedHistory] = await Promise.all([
        listFormalComments(round.id),
        getRoundDecision(round.id),
        listFeedbackDeliveryHistory(round.id),
      ]);
      setComments(savedComments);
      onCommentsChange(savedComments);
      setDecision(savedDecision);
      setHistory(savedHistory);
      setError(null);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setLoading(false);
    }
  }, [onCommentsChange, round.id]);

  useEffect(() => {
    void refreshComments();
    listAgentRoutes()
      .then((items) => {
        setRoutes(items);
      })
      .catch((problem) => setError(toCommandError(problem)));
  }, [refreshComments]);

  const addComment = async (event: React.FormEvent) => {
    event.preventDefault();
    if (readOnly || !draft.trim()) return;
    try {
      await createFormalComment(round.id, draft.trim(), initialAnchor, initialThreadId);
      setDraft("");
      await refreshComments();
    } catch (problem) {
      setError(toCommandError(problem));
    }
  };

  const copyText = async (text: string, deliveryId?: string) => {
    try {
      await navigator.clipboard.writeText(text);
      if (deliveryId) setCopiedDeliveryId(deliveryId);
      else setCopied(true);
    } catch {
      setError({
        code: "clipboard_unavailable",
        message: "Review Queue could not copy the feedback prompt.",
        data_safety: "Your formal drafts remain saved and nothing was sent.",
        next_step: "Select the draft text and copy it manually.",
      });
    }
  };

  const prepare = async () => {
    if (readOnly) return;
    setWorking(true);
    try {
      setPrepared(await prepareFeedbackHandoff(
        round.id,
        routeId || null,
      ));
      setDeliveryMessage("");
      setCopied(false);
      setError(null);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setWorking(false);
    }
  };

  const confirmSubmitted = async (deliveryId: string) => {
    if (readOnly) return;
    setWorking(true);
    try {
      await confirmManualFeedbackSubmission(deliveryId);
      setDeliveryMessage("Manual submission recorded. Review Queue did not contact or inject into the agent session.");
      if (prepared?.delivery_id === deliveryId) setPrepared(null);
      setCopied(false);
      await refreshComments();
      setError(null);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setWorking(false);
    }
  };

  const prepareDisabledReason = !comments.length
    ? "Add at least one formal comment before preparing feedback."
    : !decision
      ? "Record Approve or Request changes before preparing feedback."
      : "";
  const selectedRoute = routes.find((route) => route.id === routeId) ?? null;

  return (
    <div className="drawer-backdrop">
      <aside {...dialog} className="feedback" role="dialog" aria-modal="true" aria-labelledby="feedback-title">
        <header><h2 id="feedback-title">Formal feedback</h2><button onClick={onClose} aria-label="Close formal feedback">×</button></header>
        <p><b>{comments.length} comments</b> · your <code>/ask</code> chat is never sent</p>
        <p>
          <b>Decision</b> · {decision ? decision.replace("_", " ") : "not recorded"}
        </p>
        {readOnly && <p className="notice">{readOnlyReason || "This historical round is read-only."} Saved comments and handoffs remain available to inspect and copy.</p>}
        <label>
          Originating session
          <select value={routeId} onChange={(event) => { setRouteId(event.target.value); setPrepared(null); setCopied(false); }}>
            <option value="">Closed or unavailable — reproduce first</option>
            {routes.map((route) => <option key={route.id} value={route.id}>{route.agent_id} · {route.status} · {route.session_id ?? "no session"}</option>)}
          </select>
        </label>
        {selectedRoute && <AgentRouteDetails route={selectedRoute} />}
        <p className="safe-copy">Route status is informational. Review Queue never queues, interrupts, types, or injects a prompt into this session.</p>
        {loading && <p>Loading saved drafts…</p>}
        {comments.map((comment) => (
          <article className="formal-comment" key={comment.id}>
            {editingId === comment.id ? (
              <form
                onSubmit={async (event) => {
                  event.preventDefault();
                  if (!editBody.trim() || readOnly) return;
                  try {
                    await editFormalComment(comment.id, editBody.trim(), comment.anchor ?? null);
                    setEditingId(null);
                    setEditBody("");
                    await refreshComments();
                  } catch (problem) {
                    setError(toCommandError(problem));
                  }
                }}
              >
                <label>
                  Edit formal comment
                  <textarea value={editBody} onChange={(event) => setEditBody(event.target.value)} />
                </label>
                <div className="inline-actions">
                  <button type="button" onClick={() => { setEditingId(null); setEditBody(""); }}>Cancel</button>
                  <button disabled={!editBody.trim()}>Save as revision {comment.revision + 1}</button>
                </div>
              </form>
            ) : (
              <p>{comment.body}</p>
            )}
            {comment.anchor && <FormalAnchorDetails anchor={comment.anchor} />}
            <small>
              revision {comment.revision}
              {comment.delivered_revision === comment.revision ? " · delivered" : comment.delivered_revision ? " · delivered · edited" : " · not delivered"}
            </small>
            <button onClick={() => void copyText(formalCommentForCopy(comment))}>Copy comment</button>
            {!readOnly && editingId !== comment.id && (
              <div className="inline-actions">
                <button onClick={() => { setEditingId(comment.id); setEditBody(comment.body); }}>Edit</button>
                <button
                  aria-label={`Resolve comment ${comment.body}`}
                  onClick={async () => {
                    try {
                      await deleteFormalComment(comment.id);
                      await refreshComments();
                    } catch (problem) {
                      setError(toCommandError(problem));
                    }
                  }}
                >
                  Resolve draft
                </button>
              </div>
            )}
          </article>
        ))}
        {!readOnly && (
          <form onSubmit={addComment}>
            <label>
              {initialAnchor
                ? `Comment on ${initialAnchor.workspace_relative_path}:${initialAnchor.start_line}–${initialAnchor.end_line}`
                : "Add general comment"}
              <textarea value={draft} onChange={(event) => setDraft(event.target.value)} placeholder={initialAnchor ? "Feedback anchored to this hunk" : "Feedback about the change as a whole"} />
            </label>
            {initialAnchor && <FormalAnchorDetails anchor={initialAnchor} />}
            <button disabled={!draft.trim()}>Add comment</button>
          </form>
        )}
        <section className="delivery-history">
          <h3>Manual handoff history</h3>
          {history.length === 0 && <p className="muted">No immutable handoff has been prepared.</p>}
          {history.map((entry) => (
            <article key={entry.delivery.id}>
              <p>
                <b>{entry.delivery.payload.decision.replace("_", " ")}</b> · {entry.delivery.payload.comments.length} comment revisions
              </p>
              <small>
                Prepared {new Date(entry.created_at).toLocaleString()} · {entry.outcome?.replaceAll("_", " ") ?? "awaiting manual confirmation"}
                {entry.delivered_at ? ` · confirmed ${new Date(entry.delivered_at).toLocaleString()}` : ""}
              </small>
              <button onClick={() => void copyText(feedbackPromptFromHistory(entry), entry.delivery.id)}>
                {copiedDeliveryId === entry.delivery.id ? "Copied" : "Copy historical prompt"}
              </button>
              {!readOnly && !entry.outcome && (
                <button disabled={working} onClick={() => void confirmSubmitted(entry.delivery.id)}>
                  {working ? "Recording…" : "I submitted this manually"}
                </button>
              )}
            </article>
          ))}
        </section>
        {error && <ErrorPanel error={error} />}
        {prepared && (
          <section className="prepared-feedback">
            <p><b>Immutable prepared prompt</b> · <code>{prepared.idempotency_key}</code></p>
            <textarea readOnly value={prepared.prompt} aria-label="Immutable prepared feedback prompt" />
            <p className="safe-copy">{prepared.guidance}</p>
            {prepared.reproduction_required && (
              <button disabled={readOnly} onClick={onReproduce}>Preview and confirm reproduction…</button>
            )}
          </section>
        )}
        <div className="drawer-actions">
          {!prepared ? (
            <button
              disabled={readOnly || Boolean(prepareDisabledReason) || working}
              title={readOnly ? readOnlyReason : prepareDisabledReason}
              onClick={() => void prepare()}
            >
              {working ? "Preparing…" : "Prepare immutable prompt"}
            </button>
          ) : (
            <>
              <button disabled={working} onClick={() => void copyText(prepared.prompt)}>{copied ? "Copied" : "Copy prepared prompt"}</button>
              <button disabled={working || readOnly} title={readOnly ? readOnlyReason : ""} onClick={() => void confirmSubmitted(prepared.delivery_id)}>
                {working ? "Recording…" : "I submitted it manually"}
              </button>
            </>
          )}
        </div>
        <p className="safe-copy">{deliveryMessage || "Preparing and copying do not contact an agent. Only you can submit the prompt."}</p>
      </aside>
    </div>
  );
}

function FormalAnchorDetails({ anchor }: { anchor: Anchor }) {
  return (
    <div className="formal-anchor">
      <small>
        <code>{anchor.workspace_relative_path}:{anchor.start_line}–{anchor.end_line}</code> · {anchor.side} · blob {shortSha(anchor.blob_sha)}
      </small>
      {anchor.selected_code && <pre>{anchor.selected_code}</pre>}
    </div>
  );
}

function AgentRouteDetails({ route }: { route: AgentRoute }) {
  const provenance = route.provenance;
  return (
    <section className="route-details">
      <p>
        <b>Route</b> {route.adapter_kind}{provenance?.adapter_version ? ` ${provenance.adapter_version}` : ""} · {route.agent_id} · {route.status}
        {provenance?.schema_version ? ` · provenance v${provenance.schema_version}` : ""}
      </p>
      <p><b>Heartbeat</b> {new Date(route.last_heartbeat).toLocaleString()}</p>
      {(route.session_id || route.endpoint) && <p><b>Session</b> {route.session_id ?? "no session"}{route.endpoint ? ` · ${route.endpoint}` : ""}</p>}
      {provenance?.machine_id && <p><b>Machine</b> <code>{provenance.machine_id}</code></p>}
      {provenance?.original_cwd && <p><b>Original cwd</b> <code>{provenance.original_cwd}</code></p>}
      {(provenance?.cmux_workspace || provenance?.cmux_surface) && (
        <p><b>cmux</b> {provenance.cmux_workspace ?? "unknown workspace"} · {provenance.cmux_surface ?? "unknown surface"}</p>
      )}
      {(provenance?.provider || provenance?.model) && (
        <p><b>Provider</b> {[provenance.provider, provenance.provider_version, provenance.model].filter(Boolean).join(" · ")}</p>
      )}
      {(provenance?.mode || provenance?.thinking || provenance?.context) && (
        <p><b>Session configuration</b> {[provenance.mode, provenance.thinking, provenance.context].filter(Boolean).join(" · ")}</p>
      )}
      {provenance?.reconnect_recipe && <p><b>Reconnect</b> {provenance.reconnect_recipe}</p>}
      {(provenance?.provider_resume_handle || provenance?.transcript_reference) && (
        <p><b>Resume / transcript</b> {[provenance.provider_resume_handle, provenance.transcript_reference].filter(Boolean).join(" · ")}</p>
      )}
      {provenance?.last_turn && (
        <p>
          <b>Last turn</b>{" "}
          {[
            provenance.last_turn.turn_id,
            provenance.last_turn.status,
            provenance.last_turn.completed_at
              ? new Date(provenance.last_turn.completed_at).toLocaleString()
              : provenance.last_turn.started_at
                ? `started ${new Date(provenance.last_turn.started_at).toLocaleString()}`
                : null,
          ].filter(Boolean).join(" · ") || "recorded"}
        </p>
      )}
    </section>
  );
}

function formalCommentForCopy(comment: FormalComment) {
  const location = comment.anchor
    ? `${comment.anchor.workspace_relative_path}:${comment.anchor.start_line}-${comment.anchor.end_line} (${comment.anchor.side}): `
    : "";
  return `${location}${comment.body}`;
}

function feedbackPromptFromHistory(entry: DeliveryHistoryEntry) {
  const decision = entry.delivery.payload.decision === "approve" ? "Approve" : "Request changes";
  const comments = entry.delivery.payload.comments.map((comment, index) => {
    return `${index + 1}. ${formalCommentForCopy(comment)}`;
  });
  return [
    `Review round: ${entry.delivery.payload.round_id}`,
    `Feedback ID: ${entry.delivery.idempotency_key}`,
    `Decision: ${decision}`,
    "",
    "Formal feedback:",
    "",
    ...comments,
  ].join("\n");
}

function PinnedFilePane({
  round,
  selected,
  githubFile,
}: {
  round: ReviewRound;
  selected: { repository: RepositoryDiff; file: DiffFile; path: string };
  githubFile?: GithubMaterializedFile;
}) {
  const [left, setLeft] = useState<PinnedFileContent | null>(null);
  const [right, setRight] = useState<PinnedFileContent | null>(null);
  const [error, setError] = useState<CommandError | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    if (githubFile) {
      setLeft(githubPinnedFile(githubFile, "LEFT"));
      setRight(githubPinnedFile(githubFile, "RIGHT"));
      setError(null);
      setLoading(false);
      return () => {
        cancelled = true;
      };
    }
    const requests: Promise<PinnedFileContent | null>[] = [
      selected.file.old_path
        ? materializeRoundFile(round.id, selected.file.repository_id, selected.file.old_path, "LEFT")
        : Promise.resolve(null),
      selected.file.new_path
        ? materializeRoundFile(round.id, selected.file.repository_id, selected.file.new_path, "RIGHT")
        : Promise.resolve(null),
    ];
    Promise.all(requests)
      .then(([base, head]) => {
        if (cancelled) return;
        setLeft(base);
        setRight(head);
        setError(null);
      })
      .catch((problem) => {
        if (!cancelled) setError(toCommandError(problem));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [githubFile, round.id, selected.file.repository_id, selected.file.old_path, selected.file.new_path]);

  if (loading) return <p className="loading-state">Loading complete pinned file…</p>;
  if (error) return <ErrorPanel error={error} />;
  return <FullFileView file={right ?? left} />;
}

function FullFileView({
  file,
  empty = "The pinned file is unavailable on this side.",
}: {
  file: PinnedFileContent | null;
  empty?: string;
}) {
  if (!file) return <div className="full-file empty-state">{empty}</div>;
  if (file.is_binary) return <div className="full-file binary-state">Binary blob <code>{shortSha(file.blob_sha)}</code></div>;
  return (
    <div className="full-file code" aria-label={`${file.side} complete file`}>
      <div className="full-file-head">{file.side} · <code>{shortSha(file.blob_sha)}</code></div>
      {(file.content ?? "").split("\n").map((line, index) => (
        <div className="code-line" key={index}><span>{index + 1}</span><code>{line}</code></div>
      ))}
    </div>
  );
}

function DiffFileView({
  file,
  fileIndex,
  continuous = false,
  activeHunk,
  onActiveHunkChange,
  layout,
  repositoryRoot,
  importedComments,
  formalComments,
  askTurns,
  readOnly,
  onComment,
  onAsk,
  onOpenAskTurn,
  onConvertAskTurn,
}: {
  file: DiffFile;
  fileIndex: number;
  continuous?: boolean;
  activeHunk: number | null;
  onActiveHunkChange: (hunkIndex: number) => void;
  layout: "unified" | "split";
  repositoryRoot: string;
  importedComments: ImportedComment[];
  formalComments: FormalComment[];
  askTurns: AskTurn[];
  readOnly: boolean;
  onComment: (anchor: Anchor, threadId?: string) => void;
  onAsk: (anchor: Anchor) => void;
  onOpenAskTurn: (anchor: Anchor) => void;
  onConvertAskTurn: (turn: AskTurn) => void;
}) {
  if (file.is_binary) {
    return <div className="code binary-state"><b>Binary file changed</b><p>The pinned Git patch is retained, but binary content is not rendered as text.</p></div>;
  }
  return (
    <div className={continuous ? "code continuous-code" : "code"} role="region" aria-label="Code diff" tabIndex={0}>
      {!continuous && (
        <nav className="hunk-navigation" aria-label="Hunk navigation">
          <button disabled={activeHunk === null || activeHunk <= 0} onClick={() => onActiveHunkChange(Math.max(0, (activeHunk ?? 0) - 1))}>Previous hunk</button>
          <span>{file.hunks.length && activeHunk !== null ? `${activeHunk + 1} / ${file.hunks.length}` : "No hunks"}</span>
          <button disabled={activeHunk === null || activeHunk >= file.hunks.length - 1} onClick={() => onActiveHunkChange(Math.min(file.hunks.length - 1, (activeHunk ?? 0) + 1))}>Next hunk</button>
          <span className="muted">Use Full file to expand context.</span>
        </nav>
      )}
      {file.hunks.map((hunk, index) => (
        <div
          id={`review-queue-hunk-${fileIndex}-${index}`}
          className={index === activeHunk ? "active-hunk" : ""}
          key={`${hunk.old_start}:${hunk.new_start}:${index}`}
        >
          <DiffHunkView
            file={file}
            hunk={hunk}
            layout={layout}
            repositoryRoot={repositoryRoot}
            importedComments={importedComments}
            formalComments={formalComments}
            askTurns={askTurns}
            readOnly={readOnly}
            onComment={onComment}
            onAsk={onAsk}
            onOpenAskTurn={onOpenAskTurn}
            onConvertAskTurn={onConvertAskTurn}
          />
        </div>
      ))}
    </div>
  );
}

function pairSplitLines(lines: DiffLine[], oldStart: number, newStart: number) {
  type Indexed = { line: DiffLine; index: number };
  type Numbered = Indexed & { number: number };
  const rows: Array<{ left: Numbered | null; right: Numbered | null }> = [];
  let oldLine = oldStart;
  let newLine = newStart;
  let deletions: Indexed[] = [];
  let additions: Indexed[] = [];
  const flush = () => {
    const pairCount = Math.max(deletions.length, additions.length);
    for (let i = 0; i < pairCount; i++) {
      const del = deletions[i];
      const add = additions[i];
      rows.push({
        left: del ? { ...del, number: oldLine++ } : null,
        right: add ? { ...add, number: newLine++ } : null,
      });
    }
    deletions = [];
    additions = [];
  };
  for (const [index, line] of lines.entries()) {
    if (line.type === "deletion") deletions.push({ line, index });
    else if (line.type === "addition") additions.push({ line, index });
    else {
      flush();
      rows.push({
        left: { line, index, number: oldLine++ },
        right: { line, index, number: newLine++ },
      });
    }
  }
  flush();
  return rows;
}

function DiffHunkView({
  file,
  hunk,
  layout,
  repositoryRoot,
  importedComments,
  formalComments,
  askTurns,
  readOnly,
  onComment,
  onAsk,
  onOpenAskTurn,
  onConvertAskTurn,
}: {
  file: DiffFile;
  hunk: DiffHunk;
  layout: "unified" | "split";
  repositoryRoot: string;
  importedComments: ImportedComment[];
  formalComments: FormalComment[];
  askTurns: AskTurn[];
  readOnly: boolean;
  onComment: (anchor: Anchor, threadId?: string) => void;
  onAsk: (anchor: Anchor) => void;
  onOpenAskTurn: (anchor: Anchor) => void;
  onConvertAskTurn: (turn: AskTurn) => void;
}) {
  const [selection, setSelection] = useState<{
    side: "LEFT" | "RIGHT";
    start: number;
    end: number;
  } | null>(null);
  const [keyboardLine, setKeyboardLine] = useState<{
    side: "LEFT" | "RIGHT";
    index: number;
  } | null>(null);
  let oldLine = hunk.old_start;
  let newLine = hunk.new_start;
  const numberedLines = hunk.lines.map((line, index) => ({
    line,
    index,
    oldNumber: line.type === "addition" ? null : oldLine++,
    newNumber: line.type === "deletion" ? null : newLine++,
  }));
  const defaultSide = file.new_path && file.new_blob_sha ? "RIGHT" : "LEFT";
  const selectedSide = selection?.side ?? defaultSide;
  const right = selectedSide === "RIGHT";
  const path = (right ? file.new_path : file.old_path) ?? "(unknown path)";
  const relevantKinds = right ? new Set(["context", "addition"]) : new Set(["context", "deletion"]);
  const selectedLines = numberedLines.filter(({ line, index }) =>
    relevantKinds.has(line.type)
      && (!selection || (index >= selection.start && index <= selection.end)),
  );
  const selectedNumbers = selectedLines
    .map(({ oldNumber, newNumber }) => right ? newNumber : oldNumber)
    .filter((line): line is number => line !== null);
  const startLine = selectedNumbers[0] ?? (right ? hunk.new_start : hunk.old_start);
  const endLine = selectedNumbers.at(-1) ?? startLine;
  const anchor: Anchor | null = (right ? file.new_blob_sha : file.old_blob_sha)
    ? {
        repository_id: file.repository_id,
        workspace_relative_path: repositoryRoot === "." ? path : `${repositoryRoot}/${path}`,
        side: right ? "RIGHT" : "LEFT",
        start_line: startLine,
        end_line: endLine,
        blob_sha: (right ? file.new_blob_sha : file.old_blob_sha) as string,
        selected_code: selectedLines.map(({ line }) => line.content).join("\n") || hunk.header,
      }
    : null;
  const workspacePathForSide = (side: "LEFT" | "RIGHT") => {
    const sidePath = (side === "RIGHT" ? file.new_path : file.old_path) ?? path;
    return repositoryRoot === "." ? sidePath : `${repositoryRoot}/${sidePath}`;
  };
  const anchorEndsAt = (
    itemAnchor: Anchor | null | undefined,
    side: "LEFT" | "RIGHT",
    line: number,
  ) => Boolean(
    itemAnchor
      && itemAnchor.repository_id === file.repository_id
      && itemAnchor.workspace_relative_path === workspacePathForSide(side)
      && itemAnchor.side.toUpperCase() === side
      && itemAnchor.end_line === line,
  );
  const renderInlineThreads = (side: "LEFT" | "RIGHT", line: number) => {
    const importedAtLine = importedComments.filter((comment) =>
      anchorEndsAt(comment.anchor, side, line));
    const formalAtLine = formalComments.filter((comment) =>
      anchorEndsAt(comment.anchor, side, line));
    const asksAtLine = askTurns.filter((turn) =>
      anchorEndsAt(turn.anchor, side, line));
    if (!importedAtLine.length && !formalAtLine.length && !asksAtLine.length) return null;
    return (
      <div className="inline-thread-stack" aria-label={`Threads at ${workspacePathForSide(side)} line ${line}`}>
        {importedAtLine.map((comment) => (
          <article
            className={`imported-thread-inline ${comment.upstream_resolved ? "resolved-upstream" : ""}`}
            key={`imported:${comment.id}`}
          >
            <small>{comment.upstream_resolved ? "Resolved on GitHub" : "Imported review thread · read-only"}</small>
            <p><b>{comment.upstream_author}</b> · <time>{new Date(comment.upstream_created_at).toLocaleString()}</time></p>
            <p>{comment.body}</p>
            <div className="inline-thread-actions">
              <a href={comment.source_url} target="_blank" rel="noreferrer">Open upstream</a>
              <button
                disabled={readOnly || !comment.anchor}
                title={readOnly ? "This round is read-only" : "Draft a formal reply in this imported GitHub thread"}
                onClick={() => comment.anchor && onComment(comment.anchor, comment.thread_id)}
              >Reply formally</button>
            </div>
          </article>
        ))}
        {formalAtLine.map((comment) => (
          <article className="formal-comment inline-formal-comment" key={`formal:${comment.id}`}>
            <small>Formal comment · revision {comment.revision}{comment.delivered_revision ? ` · delivered r${comment.delivered_revision}` : ""}</small>
            <p>{comment.body}</p>
          </article>
        ))}
        {asksAtLine.map((turn) => (
          <article className="ask-thread inline-ask-thread" key={`ask:${turn.id}`}>
            <small className="ask-label">
              /ask · {new Date(turn.created_at).toLocaleString()} · Copilot {turn.state}
              {Object.keys(turn.option_values).length
                ? ` · ${Object.entries(turn.option_values).map(([key, value]) => `${key}: ${value}`).join(" · ")}`
                : ""}
            </small>
            {turn.anchor?.selected_code && <pre className="anchor-snippet">{turn.anchor.selected_code}</pre>}
            <p><b>You</b> · {turn.prompt}</p>
            {turn.response_text && <p><b>Copilot</b> · {turn.response_text}</p>}
            {turn.failure_reason && <p className="danger-text">{turn.failure_reason}</p>}
            <div className="inline-thread-actions">
              {turn.anchor && (
                <>
                  <button disabled={readOnly} onClick={() => onOpenAskTurn(turn.anchor as Anchor)}>Reply to Copilot</button>
                  <button onClick={() => onOpenAskTurn(turn.anchor as Anchor)}>Open chat sheet</button>
                </>
              )}
              <button
                disabled={readOnly || !turn.response_text}
                title={!turn.response_text ? "A Copilot response is required before converting it" : ""}
                onClick={() => onConvertAskTurn(turn)}
              >Convert to comment</button>
            </div>
          </article>
        ))}
      </div>
    );
  };
  const selectLine = (
    side: "LEFT" | "RIGHT",
    index: number,
    extend: boolean,
  ) => {
    setSelection((current) => extend && current?.side === side
      ? {
          side,
          start: Math.min(current.start, index),
          end: Math.max(current.end, index),
        }
      : { side, start: index, end: index });
  };
  const splitRows = layout === "split"
    ? pairSplitLines(hunk.lines, hunk.old_start, hunk.new_start)
    : [];
  const initialSplitTarget = defaultSide === "RIGHT"
    ? splitRows.find((row) => row.right)?.right ?? splitRows.find((row) => row.left)?.left ?? null
    : splitRows.find((row) => row.left)?.left ?? splitRows.find((row) => row.right)?.right ?? null;
  const initialSplitSide = splitRows.some((row) => row.right === initialSplitTarget)
    ? "RIGHT"
    : "LEFT";
  const isKeyboardTabStop = (
    side: "LEFT" | "RIGHT",
    index: number,
    initial: boolean,
  ) => keyboardLine
    ? keyboardLine.side === side && keyboardLine.index === index
    : selection
      ? selection.side === side && selection.start === index
      : initial;
  const moveKeyboardLine = (
    event: React.KeyboardEvent<HTMLDivElement>,
  ) => {
    if (!["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(event.key)) return false;
    const lines = Array.from(
      event.currentTarget.closest(".diff-hunk")?.querySelectorAll<HTMLElement>("[data-selectable-diff-line='true']")
        ?? [],
    );
    const current = lines.indexOf(event.currentTarget);
    const delta = event.key === "ArrowUp" || event.key === "ArrowLeft" ? -1 : 1;
    const next = lines[Math.max(0, Math.min(current + delta, lines.length - 1))];
    if (!next || next === event.currentTarget) return true;
    event.preventDefault();
    setKeyboardLine({
      side: next.dataset.diffSide as "LEFT" | "RIGHT",
      index: Number(next.dataset.diffIndex),
    });
    next.focus();
    return true;
  };
  const splitCell = (
    side: "LEFT" | "RIGHT",
    entry: ReturnType<typeof pairSplitLines>[number]["left"],
  ) => {
    if (!entry) {
      return <div className="code-line split-diff-cell split-diff-empty" aria-hidden="true"><span /><code /></div>;
    }
    const sideAvailable = side === "RIGHT" ? Boolean(file.new_blob_sha) : Boolean(file.old_blob_sha);
    const sidePath = (side === "RIGHT" ? file.new_path : file.old_path) ?? path;
    const selected = selection?.side === side
      && entry.index >= selection.start
      && entry.index <= selection.end;
    const prefix = entry.line.type === "addition" ? "+" : entry.line.type === "deletion" ? "-" : " ";
    return (
      <div
        className={`code-line split-diff-cell ${entry.line.type} ${selected ? "selected-code-line" : ""}`}
        role={sideAvailable ? "button" : undefined}
        tabIndex={sideAvailable
          ? (isKeyboardTabStop(
              side,
              entry.index,
              side === initialSplitSide && entry === initialSplitTarget,
            ) ? 0 : -1)
          : undefined}
        data-selectable-diff-line={sideAvailable ? "true" : undefined}
        data-diff-side={sideAvailable ? side : undefined}
        data-diff-index={sideAvailable ? entry.index : undefined}
        aria-label={sideAvailable ? `Select ${sidePath} ${side.toLowerCase()} line ${entry.number}` : undefined}
        onFocus={sideAvailable ? () => setKeyboardLine({ side, index: entry.index }) : undefined}
        onClick={sideAvailable ? (event) => selectLine(side, entry.index, event.shiftKey) : undefined}
        onKeyDown={sideAvailable ? (event) => {
          if (moveKeyboardLine(event)) return;
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            selectLine(side, entry.index, event.shiftKey);
          }
        } : undefined}
      >
        <span>{entry.number}</span>
        <code>{prefix}{entry.line.content}</code>
      </div>
    );
  };
  return (
    <section className="diff-hunk">
      <div className="hunk-header">
        <span>@@ -{hunk.old_start},{hunk.old_lines} +{hunk.new_start},{hunk.new_lines} @@ {hunk.header}</span>
        <span>
          <button
            disabled={readOnly || !anchor}
            title={readOnly ? "This round is read-only" : !anchor ? "The pinned blob is unavailable" : "Ask Copilot about this hunk"}
            onClick={() => anchor && onAsk(anchor)}
          >/ask</button>{" "}
          <button
            disabled={readOnly || !anchor}
            title={readOnly ? "This round is read-only" : !anchor ? "The pinned blob is unavailable" : "Add a formal comment anchored to this hunk"}
            onClick={() => anchor && onComment(anchor)}
          >＋ Comment</button>
        </span>
      </div>
      {layout === "unified"
        ? numberedLines.map(({ line, index, oldNumber, newNumber }) => {
            const lineSide = line.type === "deletion"
              ? "LEFT"
              : line.type === "addition"
                ? "RIGHT"
                : defaultSide;
            const linePath = (lineSide === "RIGHT" ? file.new_path : file.old_path) ?? path;
            const selected = selection?.side === lineSide
              && index >= selection.start
              && index <= selection.end;
            return (
              <Fragment key={index}>
                <div
                  className={`code-line ${line.type} ${selected ? "selected-code-line" : ""}`}
                  role="button"
                  tabIndex={isKeyboardTabStop(lineSide, index, index === 0) ? 0 : -1}
                  data-selectable-diff-line="true"
                  data-diff-side={lineSide}
                  data-diff-index={index}
                  aria-label={`Select ${linePath} ${lineSide.toLowerCase()} line ${lineSide === "RIGHT" ? newNumber ?? oldNumber : oldNumber ?? newNumber}`}
                  onFocus={() => setKeyboardLine({ side: lineSide, index })}
                  onClick={(event) => {
                    selectLine(lineSide, index, event.shiftKey);
                  }}
                  onKeyDown={(event) => {
                    if (moveKeyboardLine(event)) return;
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      selectLine(lineSide, index, event.shiftKey);
                    }
                  }}
                >
                  <span>{oldNumber ?? ""}</span><span>{newNumber ?? ""}</span>
                  <code>{line.type === "addition" ? "+" : line.type === "deletion" ? "-" : " "}{line.content}</code>
                </div>
                {oldNumber !== null && line.type !== "addition" && renderInlineThreads("LEFT", oldNumber)}
                {newNumber !== null && line.type !== "deletion" && renderInlineThreads("RIGHT", newNumber)}
              </Fragment>
            );
          })
        : splitRows.map((row, index) => (
            <Fragment key={index}>
              <div className="split-diff-row">
                {splitCell("LEFT", row.left)}
                {splitCell("RIGHT", row.right)}
              </div>
              {row.left && renderInlineThreads("LEFT", row.left.number)}
              {row.right && renderInlineThreads("RIGHT", row.right.number)}
            </Fragment>
          ))}
    </section>
  );
}

function importedDiscussionLabel(comment: ImportedComment) {
  if (comment.upstream_resolved) return "Resolved on GitHub";
  if (comment.kind === "review_summary") {
    return `Review summary${comment.upstream_review_state ? ` · ${comment.upstream_review_state.replaceAll("_", " ")}` : ""}`;
  }
  return comment.kind === "review_thread_comment"
    ? "Review thread"
    : "Pull request discussion";
}

function ReviewBriefView({ brief }: { brief: ReviewBrief }) {
  const fields = [
    ["What", brief.what],
    ["Why", brief.why],
    ["Approach / Alternatives", brief.approach_alternatives],
    ["Testing", brief.testing],
  ].filter(([, value]) => value);
  return (
    <details className="brief">
      <summary>Review brief</summary>
      <div><b>{brief.title}</b>{fields.map(([label, value]) => <p key={label}><strong>{label}</strong>{value}</p>)}</div>
    </details>
  );
}

function SubmitLocalDialog({
  onClose,
  onSubmitted,
}: {
  onClose: () => void;
  onSubmitted: (round: ReviewRound) => Promise<void>;
}) {
  const [workspacePath, setWorkspacePath] = useState("");
  const [topic, setTopic] = useState("");
  const [brief, setBrief] = useState(emptyBrief);
  const [submitting, setSubmitting] = useState(false);
  const [preflight, setPreflight] = useState<LocalPreflight | null>(null);
  const [participating, setParticipating] = useState<Set<string>>(new Set());
  const [preflightFresh, setPreflightFresh] = useState(false);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<CommandError | null>(null);
  const [routes, setRoutes] = useState<AgentRoute[]>([]);
  const [routesLoading, setRoutesLoading] = useState(true);
  const [routeLoadError, setRouteLoadError] = useState<CommandError | null>(null);
  const [originRouteId, setOriginRouteId] = useState<string | null>(null);
  const [routeSelectionTouched, setRouteSelectionTouched] = useState(false);
  const dialog = useDialogFocus(onClose);

  const invalidatePreflight = () => setPreflightFresh(false);
  const update = (field: keyof ReviewBrief, value: string) => {
    setBrief((current) => ({ ...current, [field]: value }));
    invalidatePreflight();
  };

  useEffect(() => {
    let cancelled = false;
    setRoutesLoading(true);
    listAgentRoutes()
      .then((items) => {
        if (cancelled) return;
        setRoutes(items);
        setRouteLoadError(null);
      })
      .catch((problem) => {
        if (!cancelled) setRouteLoadError(toCommandError(problem));
      })
      .finally(() => {
        if (!cancelled) setRoutesLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (routeSelectionTouched || originRouteId || !workspacePath.trim()) return;
    const workspace = workspacePath.trim().replace(/\/+$/, "");
    if (!workspace) return;
    const matchingActiveRoutes = routes.filter((route) => {
      const cwd = route.provenance?.original_cwd?.replace(/\/+$/, "");
      return route.status.toLowerCase() === "active" && Boolean(cwd) && (cwd === workspace || cwd?.startsWith(`${workspace}/`));
    });
    // Do not guess between sessions. A single live route whose saved cwd is
    // inside the workspace is the only safe automatic provenance choice.
    if (matchingActiveRoutes.length === 1) {
      setOriginRouteId(matchingActiveRoutes[0].id);
      invalidatePreflight();
    }
  }, [originRouteId, routeSelectionTouched, routes, workspacePath]);

  const selectedRoute = routes.find((route) => route.id === originRouteId) ?? null;
  const updateWorkspacePath = (value: string) => {
    setWorkspacePath(value);
    setPreflight(null);
    setParticipating(new Set());
    // An automatic selection is only safe for the workspace it matched.
    // Preserve a deliberate manual choice, but re-evaluate any inferred one.
    if (!routeSelectionTouched) setOriginRouteId(null);
    invalidatePreflight();
  };
  const selectOriginRoute = (value: string) => {
    setRouteSelectionTouched(true);
    setOriginRouteId(value || null);
    invalidatePreflight();
  };

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setSubmitting(true);
    setError(null);
    const request: LocalSubmissionRequest = {
      workspacePath,
      topic,
      brief,
      originRouteId,
      participatingRepositoryIds: [...participating],
      preflightToken: preflightFresh ? preflight?.preflightToken ?? null : null,
    };
    try {
      const outcome = await submitLocal(request);
      await onSubmitted(outcome.round);
    } catch (problem) {
      setError(toCommandError(problem));
      setSubmitting(false);
    }
  };

  const preview = async () => {
    setChecking(true);
    setError(null);
    try {
      const next = await preflightLocal({
        workspacePath,
        topic,
        brief,
        originRouteId,
        participatingRepositoryIds: participating.size ? [...participating] : [],
        preflightToken: null,
      });
      setPreflight(next);
      setParticipating(new Set(next.participatingRepositoryIds));
      setOriginRouteId(next.originRouteId ?? null);
      setPreflightFresh(true);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setChecking(false);
    }
  };

  return (
    <div className="modal-backdrop">
      <section {...dialog} className="modal" role="dialog" aria-modal="true" aria-labelledby="submit-title">
        <header><h2 id="submit-title">Submit local review</h2><button onClick={onClose} aria-label="Close">×</button></header>
        <form className="form" onSubmit={submit}>
          <label>Workspace path<input value={workspacePath} onChange={(event) => updateWorkspacePath(event.target.value)} required autoFocus data-dialog-initial-focus /></label>
          <label>Topic (stable)<input value={topic} onChange={(event) => { setTopic(event.target.value); invalidatePreflight(); }} required /></label>
          <label>Title<input value={brief.title} onChange={(event) => update("title", event.target.value)} required /></label>
          <label>What<textarea value={brief.what} onChange={(event) => update("what", event.target.value)} /></label>
          <label>Why<textarea value={brief.why} onChange={(event) => update("why", event.target.value)} /></label>
          <label>Approach / Alternatives<textarea value={brief.approach_alternatives} onChange={(event) => update("approach_alternatives", event.target.value)} /></label>
          <label>Testing<textarea value={brief.testing} onChange={(event) => update("testing", event.target.value)} /></label>
          <label>
            Originating session (optional)
            <select
              aria-label="Originating session"
              value={originRouteId ?? ""}
              onChange={(event) => selectOriginRoute(event.target.value)}
              disabled={routesLoading}
            >
              <option value="">No originating session selected</option>
              {routes.map((route) => (
                <option key={route.id} value={route.id}>
                  {route.agent_id} · {route.status} · {route.session_id ?? "no session"}
                </option>
              ))}
            </select>
          </label>
          {routesLoading && <p className="notice">Loading saved agent-session provenance…</p>}
          {routeLoadError && <p className="notice">Agent-session provenance is unavailable. You can still capture without selecting a session.</p>}
          {selectedRoute && <AgentRouteDetails route={selectedRoute} />}
          <p className="safe-copy">This records provenance only. Review Queue never queues, interrupts, types, or injects a prompt into an agent session.</p>
          <div>
            <button type="button" onClick={() => void preview()} disabled={checking || !workspacePath || !topic || !brief.title}>
              {checking ? "Checking…" : "Preview repositories"}
            </button>
          </div>
          {preflight && (
            <section className="preflight" aria-label="Detected repositories">
              <b>Repository participation</b>
              {preflight.repositories.map((repo) => (
                <label key={repo.repositoryId}>
                  <span>
                    <input
                      type="checkbox"
                      checked={participating.has(repo.repositoryId)}
                      onChange={(event) => {
                        setParticipating((current) => {
                          const next = new Set(current);
                          if (event.target.checked) next.add(repo.repositoryId);
                          else next.delete(repo.repositoryId);
                          return next;
                        });
                        invalidatePreflight();
                      }}
                    />
                    {repo.repositoryId}
                  </span>
                  <span>{repo.branch} · {repo.hasChanges ? "changes will be committed" : "pins existing HEAD"}</span>
                </label>
              ))}
              {!participating.size && <p className="danger-text">Select at least one repository.</p>}
              {!preflightFresh && <p className="notice">Selection or form changed. Preview again before capture.</p>}
            </section>
          )}
          <p className="notice">Capture commits outstanding work per repository with a topic-tagged message. Source files are never deleted or rewritten.</p>
          {error && <ErrorPanel error={error} />}
          <footer>
            <button type="button" onClick={onClose}>Cancel</button>
            <button
              className="primary"
              disabled={submitting || !preflightFresh || !participating.size}
              title={!participating.size ? "Select at least one repository." : !preflightFresh ? "Preview the exact form and repository selection before capture." : ""}
            >
              {submitting ? "Capturing…" : "Capture snapshot"}
            </button>
          </footer>
        </form>
      </section>
    </div>
  );
}

function DetailsDialog({
  round,
  onClose,
  onReproduce,
  onSaved,
}: {
  round: ReviewRound;
  onClose: () => void;
  onReproduce: () => void;
  onSaved: (brief: ReviewBrief) => Promise<void>;
}) {
  const [editing, setEditing] = useState(false);
  const [brief, setBrief] = useState(round.brief);
  const [error, setError] = useState<CommandError | null>(null);
  const [originRoute, setOriginRoute] = useState<AgentRoute | null>(null);
  const dialog = useDialogFocus(onClose);
  useEffect(() => {
    if (!round.origin_route_id) {
      setOriginRoute(null);
      return;
    }
    listAgentRoutes()
      .then((routes) => setOriginRoute(routes.find((route) => route.id === round.origin_route_id) ?? null))
      .catch((problem) => setError(toCommandError(problem)));
  }, [round.origin_route_id]);
  return (
    <div className="modal-backdrop">
      <section {...dialog} className="modal" role="dialog" aria-modal="true" aria-labelledby="details-title">
        <header><h2 id="details-title">Review round details</h2><button onClick={onClose} aria-label="Close">×</button></header>
        <div className="detail-grid">
          <p><b>Provenance</b>{round.collection} · topic {round.manifest.topic}</p>
          <p><b>Workspace</b>{round.manifest.workspace_root}</p>
          {originRoute && <AgentRouteDetails route={originRoute} />}
          {round.source_metadata?.kind === "machine" && (
            <>
              <p><b>Machine</b>{round.source_metadata.machine_name} · <code>{round.source_metadata.machine_id}</code></p>
              <p><b>Remote path</b><code>{round.source_metadata.remote_workspace_path}</code></p>
              <p><b>Cached</b>{new Date(round.source_metadata.cached_at).toLocaleString()} · cursor {round.source_metadata.cursor}</p>
            </>
          )}
          {round.source_metadata?.kind === "github" && (
            <p><b>GitHub source</b><code>{githubSourceSummary(round.source_metadata)}</code></p>
          )}
          <p><b>Snapshot</b><code>{round.manifest_hash}</code></p>
          {!editing ? (
            <section>
              <b>Review brief</b>
              <p>{round.brief.title}</p>
              <button onClick={() => setEditing(true)} disabled={!isActive(round)}>Edit brief</button>
            </section>
          ) : (
            <form
              className="form embedded-form"
              onSubmit={async (event) => {
                event.preventDefault();
                try {
                  await onSaved(brief);
                  setEditing(false);
                  setError(null);
                } catch (problem) {
                  setError(toCommandError(problem));
                }
              }}
            >
              <label>Title<input value={brief.title} required onChange={(event) => setBrief({ ...brief, title: event.target.value })} /></label>
              <label>What<textarea value={brief.what} onChange={(event) => setBrief({ ...brief, what: event.target.value })} /></label>
              <label>Why<textarea value={brief.why} onChange={(event) => setBrief({ ...brief, why: event.target.value })} /></label>
              <label>Approach / Alternatives<textarea value={brief.approach_alternatives} onChange={(event) => setBrief({ ...brief, approach_alternatives: event.target.value })} /></label>
              <label>Testing<textarea value={brief.testing} onChange={(event) => setBrief({ ...brief, testing: event.target.value })} /></label>
              <div className="dialog-actions"><button type="button" onClick={() => { setBrief(round.brief); setEditing(false); }}>Cancel</button><button className="primary">Save brief</button></div>
            </form>
          )}
          {round.manifest.repositories.map((repo) => <p key={repo.repository_id}><b>{repo.root}</b>{repo.branch} · {shortSha(repo.base_sha)} → {shortSha(repo.head_sha)}</p>)}
          <button onClick={onReproduce}>Reproduce…</button>
          {error && <ErrorPanel error={error} />}
        </div>
      </section>
    </div>
  );
}

function ReproductionDialog({
  round,
  onClose,
}: {
  round: ReviewRound;
  onClose: () => void;
}) {
  const [destination, setDestination] = useState(`${round.manifest.workspace_root}-review-${shortSha(round.id)}`);
  const [preview, setPreview] = useState<ReproductionPreview | null>(null);
  const [working, setWorking] = useState(false);
  const [completed, setCompleted] = useState(false);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<CommandError | null>(null);
  const dialog = useDialogFocus(onClose);

  const inspect = async () => {
    setWorking(true);
    try {
      setPreview(await previewRoundReproduction(round.id, destination));
      setCompleted(false);
      setError(null);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setWorking(false);
    }
  };

  const materialize = async () => {
    setWorking(true);
    try {
      await materializeRoundReproduction(round.id, destination);
      setCompleted(true);
      setError(null);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setWorking(false);
    }
  };

  const copyBundle = async () => {
    if (!preview) return;
    try {
      await navigator.clipboard.writeText(preview.command_bundle);
      setCopied(true);
    } catch {
      setError({
        code: "clipboard_unavailable",
        message: "Review Queue could not copy the reproduction commands.",
        data_safety: "The preview is unchanged and no source repository was modified.",
        next_step: "Select the command bundle in this dialog and copy it manually.",
      });
    }
  };

  return (
    <div className="modal-backdrop">
      <section {...dialog} className="modal reproduction-dialog" role="dialog" aria-modal="true" aria-labelledby="reproduce-title">
        <header><h2 id="reproduce-title">Reproduce review round</h2><button onClick={onClose} aria-label="Close">×</button></header>
        <div className="detail-grid">
          <label>Clean destination<input value={destination} onChange={(event) => { setDestination(event.target.value); setPreview(null); setCompleted(false); }} autoFocus /></label>
          <p className="safe-copy">Preview creates nothing. Confirmed materialization makes detached clones at the saved commits and never changes the source workspace or sends feedback.</p>
          {!preview && <button onClick={() => void inspect()} disabled={working || !destination}>{working ? "Inspecting…" : "Preview reproduction"}</button>}
          {preview && (
            <>
              <p><b>Destination</b><code>{preview.destination}</code></p>
              {preview.repositories.map((repository) => (
                <p key={repository.repository_id}>
                  <b>{repository.repository_id}</b>
                  <span><code>{shortSha(repository.head_sha)}</code> → {repository.destination}</span>
                </p>
              ))}
              <label>Copyable environment setup bundle<textarea readOnly value={preview.command_bundle} /></label>
              <p><b>Fresh-agent working directory</b><code>{preview.agent_working_directory}</code></p>
              <p className="safe-copy">{preview.launch_guidance}</p>
              <div className="dialog-actions">
                <button onClick={() => void copyBundle()}>{copied ? "Copied" : "Copy commands"}</button>
                <button className="primary" disabled={working || completed} onClick={() => void materialize()}>
                  {working ? "Reproducing…" : completed ? "Reproduced" : "Confirm and reproduce"}
                </button>
              </div>
            </>
          )}
          {completed && <p className="safe-copy">The saved commits were reproduced. Start a fresh agent in the displayed working directory and submit the prepared prompt manually.</p>}
          {error && <ErrorPanel error={error} />}
        </div>
      </section>
    </div>
  );
}

function SettingsDialog({
  initialHealth,
  onHealth,
  onClose,
}: {
  initialHealth: ConnectionHealth | null;
  onHealth: (health: ConnectionHealth) => void;
  onClose: () => void;
}) {
  const [health, setHealth] = useState(initialHealth);
  const [deviceFlow, setDeviceFlow] = useState<DeviceFlowPublicState | null>(initialHealth?.pendingDeviceFlow ?? null);
  const [clientId, setClientId] = useState(initialHealth?.publicClientId ?? "");
  const [confirmClientChange, setConfirmClientChange] = useState(false);
  const [error, setError] = useState<CommandError | null>(null);
  const [working, setWorking] = useState(false);
  const [update, setUpdate] = useState<UpdateCheck | null>(null);
  const [appVersion, setAppVersion] = useState("");
  const [installedVersion, setInstalledVersion] = useState("");
  const [diagnosticsPath, setDiagnosticsPath] = useState("");
  const [codeCopied, setCodeCopied] = useState(false);
  const [deviceFlowMessage, setDeviceFlowMessage] = useState("");
  const [devicePollDelay, setDevicePollDelay] = useState(5);
  const [, tick] = useState(0);
  const dialog = useDialogFocus(onClose);

  useEffect(() => {
    if (health || !initialHealth) return;
    setHealth(initialHealth);
    setDeviceFlow(initialHealth.pendingDeviceFlow ?? null);
    setClientId(initialHealth.publicClientId ?? "");
  }, [health, initialHealth]);

  useEffect(() => {
    if (!deviceFlow || deviceFlow.phase === "expired") return;
    const timer = window.setInterval(() => tick((value) => value + 1), 1000);
    return () => window.clearInterval(timer);
  }, [deviceFlow]);

  useEffect(() => {
    let cancelled = false;
    void applicationVersion()
      .then((version) => {
        if (!cancelled) setAppVersion(version);
      })
      .catch(() => {
        if (!cancelled) setAppVersion("");
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const acceptHealth = (next: ConnectionHealth) => {
    setHealth(next);
    setDeviceFlow(next.pendingDeviceFlow ?? null);
    setClientId(next.publicClientId ?? "");
    onHealth(next);
  };

  const pollDeviceFlow = async () => {
    setWorking(true);
    try {
      const result = await completeDeviceFlow();
      setDeviceFlowMessage(result.message);
      setDevicePollDelay(result.retryAfterSeconds ?? 5);
      if (result.phase === "connected") {
        acceptHealth(await retryConnection());
        setError(null);
      } else if (result.phase !== "pending" && result.phase !== "slow_down") {
        acceptHealth(await retryConnection());
        setError({
          code: `device_flow_${result.phase}`,
          message: result.message,
          data_safety: "No review data changed and no credential was retained for this attempt.",
          next_step: result.phase === "account_mismatch"
            ? "Disconnect the capability and start a new flow with the intended GitHub account."
            : "Start a fresh connection when you are ready.",
        });
      }
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setWorking(false);
    }
  };

  useEffect(() => {
    if (!deviceFlow || deviceFlow.phase !== "pending" || working) return;
    const timer = window.setTimeout(() => void pollDeviceFlow(), devicePollDelay * 1000);
    return () => window.clearTimeout(timer);
  }, [deviceFlow, devicePollDelay, working]);

  const run = async (operation: () => Promise<ConnectionHealth>) => {
    setWorking(true);
    try {
      acceptHealth(await operation());
      setError(null);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setWorking(false);
    }
  };

  const connect = async (capability: string) => {
    setWorking(true);
    try {
      const pending = await startDeviceFlow(capability);
      setDeviceFlow(pending);
      setDeviceFlowMessage("Waiting for browser approval. Review Queue will check automatically.");
      setDevicePollDelay(5);
      setError(null);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setWorking(false);
    }
  };

  const saveClientId = async () => {
    setWorking(true);
    try {
      await setPublicClientId(clientId, confirmClientChange);
      setConfirmClientChange(false);
      acceptHealth(await retryConnection());
      setError(null);
    } catch (problem) {
      const commandError = toCommandError(problem);
      setError(commandError);
      if (commandError.code === "public_client_id_change_confirmation_required") {
        setConfirmClientChange(true);
      }
    } finally {
      setWorking(false);
    }
  };

  const runPublicOperation = async (operation: () => Promise<void>) => {
    setWorking(true);
    try {
      await operation();
      setError(null);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setWorking(false);
    }
  };

  const copyDeviceCode = async () => {
    if (!deviceFlow) return;
    try {
      await navigator.clipboard.writeText(deviceFlow.userCode);
      setCodeCopied(true);
      setError(null);
    } catch {
      setError({
        code: "clipboard_unavailable",
        message: "Review Queue could not copy the public Device Flow code.",
        data_safety: "No credential was created or changed.",
        next_step: "Select the displayed public code and copy it manually.",
      });
    }
  };

  const secondsRemaining = deviceFlow
    ? Math.max(0, deviceFlow.expiresAtUnixSeconds - Math.floor(Date.now() / 1000))
    : 0;

  return (
    <div className="modal-backdrop">
      <section {...dialog} className="modal settings-dialog" role="dialog" aria-modal="true" aria-labelledby="settings-title">
        <header><h2 id="settings-title">Application settings</h2><button onClick={onClose} aria-label="Close">×</button></header>
        <div className="detail-grid">
          <ConnectionRow
            label="Copilot /ask"
            status={health?.copilot}
            working={working}
            onConnect={() => void connect("copilot_app")}
            onDisconnect={(source) => void run(() => disconnectCapability("copilot_app", source))}
            onUseExisting={health?.cli.signedIn && health?.copilot.source !== "existing_copilot_cli"
              ? () => void run(() => selectExistingCopilotCli())
              : undefined}
          />
          <ConnectionRow label="PR read" status={health?.prRead} working={working} onConnect={() => void connect("pr_read")} onDisconnect={(source) => void run(() => disconnectCapability("pr_read", source))} />
          <ConnectionRow label="PR publish" status={health?.prPublish} working={working} onConnect={() => void connect("pr_publish")} onDisconnect={(source) => void run(() => disconnectCapability("pr_publish", source))} />
          {deviceFlow && (
            <section className="device-flow" aria-label="GitHub Device Flow">
              <b>{deviceFlow.phase === "expired" || secondsRemaining === 0 ? "Device code expired" : "Connect in your browser"}</b>
              <p>Public code <code>{deviceFlow.userCode}</code> · expires in {secondsRemaining}s</p>
              {deviceFlowMessage && <p className="muted">{deviceFlowMessage}</p>}
              <a href={deviceFlow.verificationUri} target="_blank" rel="noreferrer">Open {deviceFlow.verificationUri}</a>
              <div className="dialog-actions">
                <button disabled={working || secondsRemaining === 0} onClick={() => void copyDeviceCode()}>{codeCopied ? "Code copied" : "Copy code"}</button>
                <button disabled={working || secondsRemaining === 0} onClick={() => void pollDeviceFlow()}>Check now</button>
                <button disabled={working || !deviceFlow.canCancel} onClick={() => void run(() => cancelDeviceFlow())}>Cancel</button>
                {secondsRemaining === 0 && <button onClick={() => void connect(deviceFlow.capability)}>Start fresh flow</button>}
              </div>
            </section>
          )}
          <section>
            <b>GitHub OAuth public client</b>
            <label>Advanced public Client ID<input value={clientId} onChange={(event) => setClientId(event.target.value)} placeholder="Public Client ID — never a secret" /></label>
            <button disabled={working || !clientId.trim()} className={confirmClientChange ? "danger" : ""} onClick={() => void saveClientId()}>
              {confirmClientChange ? "Confirm disconnect and save" : "Save Client ID"}
            </button>
            <p className="muted">Changing it disconnects only this app's connections and cancels pending Device Flow. Snapshots and comments remain.</p>
          </section>
          <section>
            <b>Keychain</b>
            <p>{health?.keychain.available ? `healthy · ${health.keychain.service}` : health?.keychain.recoveryInstructions ?? "checking…"}</p>
            {!health?.keychain.available && (
              <button onClick={() => void openKeychainAccess().catch((problem) => setError(toCommandError(problem)))}>Open Keychain Access</button>
            )}
            <button onClick={() => void run(() => retryConnection())} disabled={working}>Retry connection</button>
          </section>
          <section>
            <b>Privacy and diagnostics</b>
            <p>Credentials remain in capability-specific Keychain items. SQLite, the CLI socket, connected daemons, and diagnostics are token-free.</p>
            <button disabled={working} onClick={() => void runPublicOperation(async () => {
              const result = await exportRedactedDiagnostics(true);
              setDiagnosticsPath(result.path);
            })}>Open redacted diagnostics</button>
            {diagnosticsPath && <p className="safe-copy">Exported metadata-only support artifact: <code>{diagnosticsPath}</code></p>}
          </section>
          <section>
            <b>Updates</b>
            {appVersion && <p>Review Queue {appVersion}</p>}
            <p>Checks are read-only. Install and relaunch each require a separate click.</p>
            <div className="dialog-actions">
              <button disabled={working} onClick={() => void runPublicOperation(async () => {
                setUpdate(await checkForUpdate());
                setInstalledVersion("");
              })}>{working ? "Working…" : "Check for updates"}</button>
              {update?.available && update.version && !installedVersion && (
                <button className="primary" disabled={working} onClick={() => void runPublicOperation(async () => {
                  const result = await installUpdate(update.version ?? "");
                  setInstalledVersion(result.version);
                })}>Confirm install {update.version}</button>
              )}
              {installedVersion && (
                <button className="primary" disabled={working} onClick={() => void runPublicOperation(() => relaunchAfterUpdate())}>
                  Confirm relaunch
                </button>
              )}
            </div>
            {update && !update.available && <p className="safe-copy">Review Queue {update.currentVersion} is current.</p>}
            {update?.available && <p className="safe-copy">{update.notes || `Review Queue ${update.version} is available.`}</p>}
            {installedVersion && <p className="safe-copy">Review Queue {installedVersion} is installed. Relaunch when ready.</p>}
          </section>
          {error && <ErrorPanel error={error} />}
        </div>
      </section>
    </div>
  );
}

function ConnectionRow({
  label,
  status,
  working,
  onConnect,
  onDisconnect,
  onUseExisting,
}: {
  label: string;
  status?: ConnectionHealth["copilot"];
  working: boolean;
  onConnect: () => void;
  onDisconnect: (source: ConnectionHealth["copilot"]["source"]) => void;
  onUseExisting?: () => void;
}) {
  const connected = status?.state === "connected";
  return (
    <section className="connection-row">
      <div><b>{label}</b><p>{status?.explanation ?? "Checking connection…"}</p></div>
      <span className={connected ? "status good" : "status"}>{connected ? `✓ ${status?.account ?? status?.source.replaceAll("_", " ")}` : status?.state.replaceAll("_", " ") ?? "checking…"}</span>
      <div className="dialog-actions">
        {connected ? (
          <button disabled={working} onClick={() => status && onDisconnect(status.source)}>
            {status?.source === "existing_copilot_cli" ? "Stop using existing sign-in" : "Disconnect"}
          </button>
        ) : (
          <button disabled={working || status?.state === "unavailable"} onClick={onConnect}>Connect app</button>
        )}
        {onUseExisting && (
          <button disabled={working || status?.state === "unavailable"} onClick={onUseExisting}>
            Use existing CLI sign-in
          </button>
        )}
      </div>
    </section>
  );
}

function GithubPublishDialog({
  attempt,
  onClose,
  onConfirm,
}: {
  attempt: GithubPublishAttempt;
  onClose: () => void;
  onConfirm: () => Promise<void>;
}) {
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<CommandError | null>(null);
  const target = attempt.preview.target;
  const completed = attempt.status === "completed";
  const replyWrites = attempt.replies ?? [];
  const dialog = useDialogFocus(onClose);
  return (
    <div className="modal-backdrop">
      <section {...dialog} className="modal confirm-dialog" role="alertdialog" aria-modal="true" aria-labelledby="publish-title">
        <header><h2 id="publish-title">Publish GitHub review?</h2><button aria-label="Close" onClick={onClose}>×</button></header>
        <div className="detail-grid">
          <p><b>Target</b> {target.owner}/{target.repository} #{target.pull_number} at <code>{shortSha(target.head_sha)}</code></p>
          <p><b>Review write</b> {attempt.preview.event.toUpperCase()} · {attempt.request.comments.length} comment{attempt.request.comments.length === 1 ? "" : "s"}</p>
          {attempt.request.comments.map((comment) => (
            <article className="formal-comment" key={comment.formal_comment_id}>
              <small>{comment.disposition.replaceAll("_", " ")}{comment.fallback_reference ? ` · ${comment.fallback_reference}` : ""}</small>
              <p>{comment.body}</p>
            </article>
          ))}
          <p><b>Threaded reply writes</b> {replyWrites.length}</p>
          {replyWrites.map((reply) => (
            <article className="formal-comment github-reply-preview" key={reply.id}>
              <small>reply to imported GitHub comment {reply.request.upstream_comment_id} · formal revision {reply.request.formal_revision}</small>
              <p>{reply.request.body}</p>
            </article>
          ))}
          <p className="safe-copy">
            This confirmation performs one GitHub review write plus {replyWrites.length} threaded reply write{replyWrites.length === 1 ? "" : "s"}.
            Only the formal decision, review comments, and replies shown above are included. `/ask` chats and imported comments are excluded.
          </p>
          {completed && <p className="status good">Published once as GitHub review {attempt.review_id}.</p>}
          {completed && replyWrites.length > 0 && (
            <p className="status good">
              {replyWrites.filter((reply) => reply.status === "completed").length} threaded repl{replyWrites.length === 1 ? "y" : "ies"} published.
            </p>
          )}
          {attempt.status === "unknown" && <p className="danger-text">The publish outcome is unknown. Inspect the pull request before trying anything else.</p>}
          {replyWrites.some((reply) => reply.status === "unknown") && (
            <p className="danger-text">A threaded reply has an unknown outcome. Inspect that GitHub thread before retrying.</p>
          )}
          {error && <ErrorPanel error={error} />}
          <div className="dialog-actions">
            <button onClick={onClose} autoFocus>{completed ? "Done" : "Cancel"}</button>
            {!completed && attempt.status === "prepared" && (
              <button className="primary" disabled={working} onClick={() => {
                setWorking(true);
                setError(null);
                void onConfirm().catch((problem) => setError(toCommandError(problem))).finally(() => setWorking(false));
              }}>{working ? "Publishing…" : `Publish ${attempt.preview.event.toUpperCase()}`}</button>
            )}
          </div>
        </div>
      </section>
    </div>
  );
}

function PurgeDialog({
  intent,
  onCancel,
  onConfirm,
}: {
  intent: PurgeIntent;
  onCancel: () => void;
  onConfirm: () => Promise<void>;
}) {
  const approving = intent.kind === "approve_local";
  const dialog = useDialogFocus(onCancel);
  return (
    <div className="modal-backdrop">
      <section {...dialog} className="modal confirm-dialog" role="alertdialog" aria-modal="true" aria-labelledby="purge-title">
        <header><h2 id="purge-title">{approving ? "Approve and purge this local round?" : "Delete this review round?"}</h2></header>
        <div className="detail-grid">
          <p>This removes the queue placement, manifest references, brief, chats, comments, decisions, and deliveries stored by Review Queue.</p>
          <p className="safe-copy">Source files, repositories, and submission commits are never touched.</p>
          <div className="dialog-actions"><button onClick={onCancel} autoFocus>Cancel</button><button className="danger" onClick={() => void onConfirm()}>{approving ? "Approve and purge" : "Delete permanently"}</button></div>
        </div>
      </section>
    </div>
  );
}

function ErrorPanel({ error, onRetry }: { error: CommandError; onRetry?: () => Promise<void> }) {
  return (
    <section className="error-panel" role="alert">
      <b>{error.message}</b><p>{error.data_safety}</p><p>Next: {error.next_step}</p>
      {onRetry && <button onClick={() => void onRetry()}>Retry</button>}
    </section>
  );
}

function ErrorBanner({ error, onDismiss }: { error: CommandError; onDismiss: () => void }) {
  return <div className="error-banner"><ErrorPanel error={error} /><button onClick={onDismiss}>Dismiss</button></div>;
}

function isActive(round: ReviewRound) {
  return round.lifecycle !== "completed" && !round.superseded_by;
}

function supportsCapability(round: ReviewRound, capability: SourceCapability) {
  return round.source_adapter.capabilities.capabilities.includes(capability);
}

function displayLifecycle(lifecycle: ReviewRound["lifecycle"]) {
  return lifecycle === "changes_requested" ? "changes requested" : lifecycle;
}

function shortSha(value: string) {
  return value.slice(0, 8);
}

function formatCacheAge(age?: number | null) {
  if (age == null) return "not cached";
  if (age < 60) return `${age}s ago`;
  if (age < 3600) return `${Math.floor(age / 60)}m ago`;
  return `${Math.floor(age / 3600)}h ago`;
}

function githubSourceSummary(source: Extract<NonNullable<ReviewRound["source_metadata"]>, { kind: "github" }>) {
  const value = source as Record<string, unknown>;
  const repository = [value.owner, value.repository].filter((part) => typeof part === "string").join("/");
  const pull = typeof value.pull_number === "number" ? `#${value.pull_number}` : "";
  const head = typeof value.head_sha === "string" ? `@ ${shortSha(value.head_sha)}` : "";
  return [repository || "GitHub pull request", pull, head].filter(Boolean).join(" ");
}

function githubFilesToDiff(round: ReviewRound, files: GithubMaterializedFile[]): MaterializedDiff {
  const repository = round.manifest.repositories[0];
  const repositoryId = repository?.repository_id ?? round.topic_identity;
  return {
    repositories: [{
      repository_id: repositoryId,
      root: repository?.root || ".",
      base_sha: repository?.base_sha ?? "",
      head_sha: repository?.head_sha ?? "",
      files: files.map((file) => ({
        repository_id: repositoryId,
        old_path: file.status === "added" ? null : file.path,
        new_path: file.status === "removed" ? null : file.path,
        old_blob_sha: file.status === "added" ? null : file.base_blob_sha,
        new_blob_sha: file.status === "removed" ? null : file.head_blob_sha,
        status: file.status === "added" ? "added" : file.status === "removed" ? "deleted" : "modified",
        is_binary: file.is_binary,
        patch: file.unified_diff,
        hunks: parseGithubHunks(repositoryId, file.unified_diff),
      })),
    }],
  };
}

function parseGithubHunks(repositoryId: string, patch: string): DiffHunk[] {
  const hunks: DiffHunk[] = [];
  let current: DiffHunk | null = null;
  const headerPattern = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)$/;
  for (const raw of patch.split("\n")) {
    const match = raw.match(headerPattern);
    if (match) {
      current = {
        repository_id: repositoryId,
        old_start: Number(match[1]),
        old_lines: Number(match[2] ?? 1),
        new_start: Number(match[3]),
        new_lines: Number(match[4] ?? 1),
        header: match[5].trim(),
        lines: [],
      };
      hunks.push(current);
    } else if (current && /^[ +\-]/.test(raw)) {
      current.lines.push({
        type: raw.startsWith("+") ? "addition" : raw.startsWith("-") ? "deletion" : "context",
        content: raw.slice(1),
      });
    }
  }
  return hunks;
}

function githubPinnedFile(file: GithubMaterializedFile, side: "LEFT" | "RIGHT"): PinnedFileContent | null {
  const content = side === "LEFT" ? file.base_content : file.head_content;
  const encoded = side === "LEFT" ? file.base_content_base64 : file.head_content_base64;
  const blob = side === "LEFT" ? file.base_blob_sha : file.head_blob_sha;
  if (blob === "0".repeat(40)) return null;
  if (file.is_binary) {
    return {
      repository_id: "",
      path: file.path,
      side,
      blob_sha: blob,
      is_binary: true,
      content: null,
    };
  }
  if (content != null) {
    return {
      repository_id: "",
      path: file.path,
      side,
      blob_sha: blob,
      is_binary: false,
      content,
    };
  }
  if (!encoded) return null;
  try {
    const bytes = Uint8Array.from(atob(encoded), (character) => character.charCodeAt(0));
    return {
      repository_id: "",
      path: file.path,
      side,
      blob_sha: blob,
      is_binary: false,
      content: new TextDecoder().decode(bytes),
    };
  } catch {
    return {
      repository_id: "",
      path: file.path,
      side,
      blob_sha: blob,
      is_binary: true,
      content: null,
    };
  }
}

function capabilitySessionOptions(groups: CopilotCapabilityGroup[]): SessionOption[] {
  return groups.map((group) => ({
    key: group.key,
    label: group.label,
    kind: "select",
    values: group.choices.map((choice) => choice.value),
    selected: group.selected ?? null,
    supported: group.supported,
    unavailable_reason: group.unsupported_reason ?? null,
  }));
}

interface UnavailableCopilotOptionSelection {
  key: string;
  value: string;
  reason: string;
}

function unavailableCopilotOptionSelections(
  groups: CopilotCapabilityGroup[],
  requested: Record<string, string>,
): UnavailableCopilotOptionSelection[] {
  return Object.entries(requested).flatMap(([key, value]) => {
    const group = groups.find((candidate) => candidate.key === key);
    if (!group) {
      return [{ key, value, reason: "The current Copilot runtime did not advertise this option." }];
    }
    if (!group.supported) {
      return [{
        key,
        value,
        reason: group.unsupported_reason ?? "The current Copilot runtime reports this option as unsupported.",
      }];
    }
    if (!group.choices.some((choice) => choice.value === value)) {
      return [{
        key,
        value,
        reason: `The current Copilot runtime did not advertise this ${group.label} value.`,
      }];
    }
    return [];
  });
}

function availableCopilotOptionValues(
  groups: CopilotCapabilityGroup[],
  requested: Record<string, string>,
): Record<string, string> {
  const unavailable = new Set(
    unavailableCopilotOptionSelections(groups, requested).map((selection) => selection.key),
  );
  return Object.fromEntries(
    Object.entries(requested).filter(([key]) => !unavailable.has(key)),
  );
}

function toCommandError(problem: unknown): CommandError {
  if (problem && typeof problem === "object") {
    const candidate = problem as Partial<CommandError> & {
      dataSafety?: string;
      nextStep?: string;
    };
    const dataSafety = candidate.data_safety ?? candidate.dataSafety;
    const nextStep = candidate.next_step ?? candidate.nextStep;
    if (candidate.message && dataSafety && nextStep) {
      return {
        code: candidate.code ?? "desktop_error",
        message: candidate.message,
        data_safety: dataSafety,
        next_step: nextStep,
      };
    }
  }
  return {
    code: "desktop_error",
    message: typeof problem === "string" ? problem : "Review Queue could not complete that action.",
    data_safety: "Your source files and saved review data are unchanged.",
    next_step: "Retry the action. If it persists, restart Review Queue.",
  };
}
