import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  activeConversation,
  addMachine,
  approveRemote,
  clearCopilotChat,
  cancelDeviceFlow,
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
  queueGithubPullRequest,
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
  CopilotCapabilityGroup,
  SessionOption,
  GithubMaterializedFile,
  GithubPublishAttempt,
  ImportedComment,
  PreparedFeedbackPrompt,
  UpdateCheck,
} from "./types";

type Modal = "submit" | "github" | "details" | "reproduce" | "settings" | "machine" | null;
type PurgeIntent = { round: ReviewRound; kind: "delete" | "approve_local" };

const emptyBrief = (): ReviewBrief => ({
  title: "",
  what: "",
  why: "",
  approach_alternatives: "",
  testing: "",
});

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
          round={selected}
          onBack={() => setSelected(null)}
          onDetails={() => setModal("details")}
          onReproduce={() => setModal("reproduce")}
          onRequestChanges={() => mutate(() => requestChanges(selected.id))}
          onApproveRemote={() => mutate(() => approveRemote(selected.id))}
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
          onBack={() => setActiveMachineId(null)}
          onChanged={refreshMachines}
          onOpen={async (round) => {
            await refresh();
            await openRound(round);
          }}
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
      {error && selected && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
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
      <button className={`machine ${activeMachineId ? "" : "current"}`} aria-current={activeMachineId ? undefined : "page"} onClick={onThisMac}>
        <span>●</span><b>this Mac</b><small>{activeCount} active</small>
      </button>
      {machines.map((status) => (
        <button
          className={`machine ${activeMachineId === status.machine.id ? "current" : ""}`}
          aria-current={activeMachineId === status.machine.id ? "page" : undefined}
          key={status.machine.id}
          onClick={() => onMachine(status.machine.id)}
          title={`${status.connection}; ${status.cachedItemCount} cached`}
        >
          <span className={status.connection === "connected" ? "health" : ""}>●</span>
          <b>{status.machine.config.name}</b>
          <small>{status.cachedItemCount} cached · {formatCacheAge(status.freshness.age_seconds)}</small>
        </button>
      ))}
      <button className="add-machine" onClick={onAdd}>
        ＋ <span>Add machine</span>
      </button>
    </aside>
  );
}

function MachineQueue({
  status,
  onBack,
  onChanged,
  onOpen,
  onError,
}: {
  status: MachineStatus | null;
  onBack: () => void;
  onChanged: () => Promise<MachineStatus[]>;
  onOpen: (round: ReviewRound) => Promise<void>;
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
        <section className="queue-column machine-items">
          <h2>{localStatus.machine.config.name.toUpperCase()} ({index?.index.items.length ?? localStatus.cachedItemCount})</h2>
          {!index && (
            <p className="empty-state">
              {localStatus.connection === "connected"
                ? `Choose Refresh machine queue to list rounds on ${localStatus.machine.config.name}.`
                : `Connect ${localStatus.machine.config.name} to refresh its queue.`}
            </p>
          )}
          {index?.index.items.length === 0 && <p className="empty-state">No rounds on {localStatus.machine.config.name}.</p>}
          {index && index.index.items.length > 0 && (
            <>
            {index.index.items.map((item) => (
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
          <label>Name<input required value={name} onChange={(event) => setName(event.target.value)} placeholder="buildbox" /></label>
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
  const dialog = useDialogFocus(onClose);
  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setWorking(true);
    setError(null);
    try {
      const result = await queueGithubPullRequest(url);
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
        <form className="form" onSubmit={(event) => void submit(event)}>
          {error && <ErrorPanel error={error} />}
          <label>GitHub pull request URL
            <input required type="url" value={url} onChange={(event) => setUrl(event.target.value)} placeholder="https://github.com/owner/repo/pull/42" />
          </label>
          <p className="notice">Adding resolves metadata and creates a local queue item. Complete file blobs and comments are pulled only when you explicitly open the review.</p>
          <footer><button type="button" onClick={onClose}>Cancel</button><button className="primary" disabled={working}>{working ? "Resolving…" : "Add to GitHub queue"}</button></footer>
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
  const [nextScope, setNextScope] = useState<"overall" | "local" | "github" | "machine">("overall");
  const local = props.rounds.filter((round) => round.collection === "local");
  const github = props.rounds.filter((round) => round.collection === "github");
  const activeLocal = local.filter(isActive).length;
  const activeGithub = github.filter(isActive).length;
  const next = [...props.rounds].filter((round) =>
    isActive(round) && (nextScope === "overall" || round.collection === nextScope),
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
          <select aria-label="Open next source" value={nextScope} onChange={(event) => setNextScope(event.target.value as typeof nextScope)}>
            <option value="overall">Overall</option>
            <option value="local">Local</option>
            <option value="github">GitHub</option>
            <option value="machine">Connected machine</option>
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
              if (readOnly || !event.altKey) return;
              if (event.key === "ArrowUp") onMove(round, Math.max(0, round.rank - 1));
              if (event.key === "ArrowDown") onMove(round, round.rank + 1);
              if (event.key === "Home") onMove(round, 0);
              if (event.key === "End") onMove(round, bottomRank);
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
              <span className="status">{displayLifecycle(round.lifecycle)}</span>
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
                  {round.collection === "github" && !readOnly && <button onClick={() => onRefreshGithub(round)}>Refresh remote PR</button>}
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
  onRequestChanges: () => void;
  onApproveRemote: () => void;
  onComplete: () => void;
  onPurge: (kind: "delete" | "approve_local") => void;
  onGithubRoundRefreshed: (round: ReviewRound) => Promise<void>;
}) {
  const [filter, setFilter] = useState("");
  const [diff, setDiff] = useState<MaterializedDiff | null>(null);
  const [diffError, setDiffError] = useState<CommandError | null>(null);
  const [selectedKey, setSelectedKey] = useState("");
  const [viewed, setViewed] = useState<Set<string>>(new Set());
  const [diffLoading, setDiffLoading] = useState(true);
  const [feedbackOpen, setFeedbackOpen] = useState(false);
  const [pendingAnchor, setPendingAnchor] = useState<Anchor | null>(null);
  const [pendingAskAnchor, setPendingAskAnchor] = useState<Anchor | null>(null);
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

  useEffect(() => {
    let cancelled = false;
    setDiffLoading(true);
    const load = round.collection === "github"
      ? Promise.all([
          openGithubPullRequest(round.id).then((opened) => {
            setGithubFiles(opened.files);
            return githubFilesToDiff(round, opened.files);
          }),
          listViewedFiles(round.id),
          refreshGithubComments(round.id).then((result) => {
            setImportedComments(result.imported);
            setStaleness(result.staleness);
            return result.imported;
          }),
        ]).then(([materialized, viewedFiles]) => [materialized, viewedFiles] as const)
      : Promise.all([materializeRoundDiff(round.id), listViewedFiles(round.id)]);
    load
      .then(([materialized, viewedFiles]) => {
        if (cancelled) return;
        setDiff(materialized);
        setViewed(new Set(viewedFiles.map((file) => fileKey(file.repositoryId, file.path))));
        setDiffError(null);
      })
      .catch((problem) => {
        if (!cancelled) setDiffError(toCommandError(problem));
      })
      .finally(() => {
        if (!cancelled) setDiffLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [round.id]);

  useEffect(() => {
    if (round.collection !== "github") {
      setGithubDecision(null);
      return;
    }
    getRoundDecision(round.id)
      .then(setGithubDecision)
      .catch((problem) => setDiffError(toCommandError(problem)));
  }, [round.collection, round.id]);

  const files = useMemo(
    () => (diff?.repositories ?? []).flatMap((repository) =>
      repository.files.map((file) => ({
        repository,
        file,
        path: file.new_path ?? file.old_path ?? "(unknown path)",
      }))),
    [diff],
  );
  const filteredFiles = files.filter(({ repository, path }) =>
    `${repository.root}/${path}`.toLowerCase().includes(filter.toLowerCase()),
  );
  const selected = files.find(({ file, path }) =>
    fileKey(file.repository_id, path) === selectedKey) ?? files[0];

  const toggleViewed = async () => {
    if (!selected || readOnly) return;
    const key = fileKey(selected.file.repository_id, selected.path);
    const next = !viewed.has(key);
    try {
      await setFileViewed(round.id, selected.file.repository_id, selected.path, next);
      setViewed((current) => {
        const updated = new Set(current);
        if (next) updated.add(key); else updated.delete(key);
        return updated;
      });
    } catch (problem) {
      setDiffError(toCommandError(problem));
    }
  };

  return (
    <main className="reviewer">
      <header className="review-header">
        <button className="back" onClick={onBack}>← Queue Home</button>
        <div>
          <b>{round.manifest.topic}</b>
          <span> · {round.manifest.repositories.length} repositories @ {shortSha(round.manifest_hash)}</span>
        </div>
        <button onClick={onDetails}>Details</button>
        {round.collection === "github" && (
          <>
            <button disabled={githubWorking} onClick={() => void githubAction(async () => {
              const result = await refreshGithubComments(round.id);
              setImportedComments(result.imported);
              setStaleness(result.staleness);
            }, setGithubWorking, setDiffError)}>Refresh comments</button>
            <button disabled={githubWorking} onClick={() => void githubAction(async () => {
              setStaleness(await checkGithubStaleness(round.id));
            }, setGithubWorking, setDiffError)}>Check head</button>
            <button
              disabled={githubWorking || readOnly || !githubDecision}
              title={!githubDecision ? "Record Approve or Request changes before publishing" : readOnly ? reason : "Preview the exact GitHub review request"}
              onClick={() => void githubAction(async () => {
              setPublishAttempt(await prepareGithubPublish(round.id));
            }, setGithubWorking, setDiffError)}
            >Publish review</button>
          </>
        )}
      </header>
      <ReviewBriefView brief={round.brief} />
      {round.collection === "github" && (
        <section className="upstream-discussion">
          {staleness && staleness.pinned_head_sha !== staleness.observed_head_sha && (
            <p className="danger-text">
              Head moved from {shortSha(staleness.pinned_head_sha)} to {shortSha(staleness.observed_head_sha)}.{" "}
              <button disabled={githubWorking} onClick={() => void githubAction(async () => {
                const result = await refreshGithubRound(round.id);
                await onGithubRoundRefreshed(result.round);
              }, setGithubWorking, setDiffError)}>Refresh into new round</button>
            </p>
          )}
          <details>
            <summary>Upstream discussion ({importedComments.length})</summary>
            {importedComments.length === 0 && <p className="muted">No imported PR discussion.</p>}
            {importedComments.map((comment) => (
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
          </details>
        </section>
      )}
      <div className="workspace snapshot-workspace">
        <aside className="files">
          <div className="pane-title">Repositories</div>
          <input aria-label="Filter repositories" placeholder="Filter paths" value={filter} onChange={(event) => setFilter(event.target.value)} />
          {filteredFiles.map(({ repository, file, path }) => {
            const key = fileKey(file.repository_id, path);
            return (
            <button
              className={`file ${selected && fileKey(selected.file.repository_id, selected.path) === key ? "selected-file" : ""}`}
              key={key}
              onClick={() => setSelectedKey(key)}
            >
              <span>{repository.root}/{path}</span>
              <em>{viewed.has(key) ? "✓" : file.status}</em>
            </button>
          )})}
        </aside>
        <section className="diff snapshot-pane" aria-label="Immutable diff">
          <div className="diff-head">
            <div>
              <b>{selected?.path ?? "Immutable review snapshot"}</b>
              <span>{selected?.file.status ?? round.collection}</span>
            </div>
            <div className="view-modes" role="group" aria-label="Diff view">
              {(["unified", "split", "file"] as const).map((mode) => (
                <button className={viewMode === mode ? "selected-mode" : ""} key={mode} onClick={() => setViewMode(mode)}>{mode === "file" ? "Full file" : mode}</button>
              ))}
            </div>
            <button
              className={selected && viewed.has(fileKey(selected.file.repository_id, selected.path)) ? "viewed" : ""}
              disabled={!selected || readOnly}
              title={readOnly ? reason : ""}
              onClick={() => void toggleViewed()}
            >
              {selected && viewed.has(fileKey(selected.file.repository_id, selected.path)) ? "✓ Viewed" : "Mark viewed"}
            </button>
          </div>
          {diffLoading && <p className="loading-state">Materializing pinned commits…</p>}
          {diffError && <ErrorPanel error={diffError} />}
          {!diffLoading && !diffError && selected && viewMode === "unified" && (
            <DiffFileView
              file={selected.file}
              repositoryRoot={selected.repository.root}
              importedComments={importedComments}
              readOnly={readOnly}
              onComment={(anchor) => {
                setPendingAnchor(anchor);
                setFeedbackOpen(true);
              }}
              onAsk={setPendingAskAnchor}
            />
          )}
          {!diffLoading && !diffError && selected && viewMode !== "unified" && (
            <PinnedFilePane
              round={round}
              selected={selected}
              mode={viewMode}
              githubFile={githubFiles.find((file) => file.path === selected.path)}
            />
          )}
          {!diffLoading && !diffError && !selected && (
            <p className="loading-state">No changed files in this review round.</p>
          )}
          <div className="decision">
            <span>{readOnly ? reason : "Formal review"}</span>
            <button
              className="approve"
              disabled={readOnly}
              title={readOnly ? reason : round.collection === "local" ? "Approval purges this local round after confirmation" : "Records a local decision; it does not publish or deliver"}
              onClick={() => round.collection === "local" ? onPurge("approve_local") : onApproveRemote()}
            >
              Approve
            </button>
            <button className="changes" disabled={readOnly} title={readOnly ? reason : ""} onClick={onRequestChanges}>
              Request changes
            </button>
            <button
              title={readOnly ? "Inspect and copy saved formal feedback history" : ""}
              onClick={() => { setPendingAnchor(null); setFeedbackOpen(true); }}
            >
              Formal feedback
            </button>
            <button disabled={readOnly} onClick={onComplete}>Complete</button>
          </div>
        </section>
        <ChatSheet
          round={round}
          readOnly={readOnly}
          readOnlyReason={reason}
          pendingAnchor={pendingAskAnchor}
          onAnchorConsumed={() => setPendingAskAnchor(null)}
        />
      </div>
      {feedbackOpen && (
        <FormalFeedbackDrawer
          round={round}
          initialAnchor={pendingAnchor}
          readOnly={readOnly}
          readOnlyReason={reason}
          onClose={() => { setFeedbackOpen(false); setPendingAnchor(null); }}
          onReproduce={() => {
            setFeedbackOpen(false);
            setPendingAnchor(null);
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
  readOnly,
  readOnlyReason,
  pendingAnchor,
  onAnchorConsumed,
}: {
  round: ReviewRound;
  readOnly: boolean;
  readOnlyReason: string;
  pendingAnchor: Anchor | null;
  onAnchorConsumed: () => void;
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
  const [starting, setStarting] = useState(false);
  const [authLabel, setAuthLabel] = useState("");

  const loadConversation = useCallback(async (conversation: AskConversation) => {
    setShown(conversation);
    setTurns(await listAskTurns(conversation.id));
  }, []);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      if (readOnly) {
        const [current, history] = await Promise.all([
          currentConversation(round.id),
          listPreviousChats(round.id),
        ]);
        setActive(current);
        setPrevious(history);
        setOptionValues({});
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
      setAuthLabel(current.provider_session_label ?? "");
      setOptionValues(Object.fromEntries(current.options.filter((option) => option.selected).map((option) => [option.key, option.selected as string])));
      await loadConversation(current);
      setError(null);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setLoading(false);
    }
  }, [loadConversation, readOnly, round.id]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const clear = async () => {
    try {
      if (!active) return;
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

  const startSession = async () => {
    if (!active) return;
    setStarting(true);
    setError(null);
    try {
      const session = await startCopilotSession(round.id, active.id, optionValues);
      setSessionId(session.sessionId);
      setOptionValues(session.activeOptions);
      setAuthLabel(`${session.authSource === "existing_cli_sign_in_read_only" ? "existing Copilot CLI sign-in" : "app OAuth"}${session.account ? ` · ${session.account}` : ""}`);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setStarting(false);
    }
  };

  const pollUntilDone = async (turnId: string) => {
    try {
      for (;;) {
        const result = await pollCopilotPrompt(turnId);
        setTurns((current) => {
          const without = current.filter((turn) => turn.id !== result.turn.id);
          return [...without, result.turn].sort((a, b) => a.created_at.localeCompare(b.created_at));
        });
        if (result.update.state !== "chunk") break;
      }
    } catch (problem) {
      if (cancelledTurnIds.current.delete(turnId)) {
        setError(null);
      } else {
        setError(toCommandError(problem));
      }
      await loadConversation(active as AskConversation);
    } finally {
      setStreamingTurnId(null);
    }
  };

  const send = async (event: React.FormEvent, retry?: AskTurn) => {
    event.preventDefault();
    if (!active || !sessionId) return;
    const text = retry?.prompt ?? prompt.trim();
    if (!text) return;
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
      void pollUntilDone(turn.id);
    } catch (problem) {
      setError(toCommandError(problem));
    }
  };

  const providerLost = !sessionId && turns.length > 0 && shown?.id === active?.id;
  const historyOnly = shown?.id !== active?.id || shown?.session_state === "history_only" || providerLost || readOnly;
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
    <aside className="chat" aria-label="Round chat">
      <header>
        <div><b>Chat</b><small> · {shortSha(round.id)}</small></div>
        <span className="session-state">{historyOnly ? "history only" : authLabel || "ready to start"}</span>
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
          <button className="primary" disabled={starting || !active} onClick={() => void startSession()}>
            {starting ? "Starting…" : "Start Copilot"}
          </button>
        )}
      </div>
      {shown?.options.length ? (
        <div className="options">
          {shown.options.map((option) => (
            <label key={option.key}>
              {option.label}
              <select
                disabled={!option.supported || historyOnly}
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
                {(option.values.length ? option.values : [option.selected ?? ""]).map((value) => (
                  <option key={value}>{value}</option>
                ))}
              </select>
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
          <article key={turn.id} className="chat-turn">
            <small>You · {turn.anchor ? `${turn.anchor.workspace_relative_path}:${turn.anchor.start_line}–${turn.anchor.end_line}` : "round follow-up"}</small>
            <p>{turn.prompt}</p>
            <small>Copilot · {turn.state}{Object.keys(turn.option_values).length ? ` · ${Object.entries(turn.option_values).map(([key, value]) => `${key}: ${value}`).join(" · ")}` : ""}</small>
            {turn.response_text && <p>{turn.response_text}</p>}
            {turn.failure_reason && (
              <p className="danger-text">
                {turn.failure_reason}{" "}
                {!historyOnly && sessionId && <button onClick={(event) => void send(event, turn)}>Retry as new prompt</button>}
              </p>
            )}
            {turn.state === "cancelled" && !historyOnly && sessionId && (
              <button onClick={(event) => void send(event, turn)}>Retry as new prompt</button>
            )}
          </article>
        ))}
        {error && <ErrorPanel error={error} />}
      </div>
      <form onSubmit={(event) => void send(event)}>
        <input
          disabled={historyOnly || !sessionId || Boolean(streamingTurnId)}
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
          <button disabled={historyOnly || !sessionId || !prompt.trim()} title={inputReason}>Send</button>
        )}
      </form>
    </aside>
  );
}

function FormalFeedbackDrawer({
  round,
  initialAnchor,
  readOnly,
  readOnlyReason,
  onClose,
  onReproduce,
}: {
  round: ReviewRound;
  initialAnchor: Anchor | null;
  readOnly: boolean;
  readOnlyReason: string;
  onClose: () => void;
  onReproduce: () => void;
}) {
  const [comments, setComments] = useState<FormalComment[]>([]);
  const [history, setHistory] = useState<DeliveryHistoryEntry[]>([]);
  const [draft, setDraft] = useState("");
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
      setDecision(savedDecision);
      setHistory(savedHistory);
      setError(null);
    } catch (problem) {
      setError(toCommandError(problem));
    } finally {
      setLoading(false);
    }
  }, [round.id]);

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
      await createFormalComment(round.id, draft.trim(), initialAnchor);
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
  mode,
  githubFile,
}: {
  round: ReviewRound;
  selected: { repository: RepositoryDiff; file: DiffFile; path: string };
  mode: "split" | "file";
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
  if (mode === "file") return <FullFileView file={right ?? left} />;
  return (
    <div className="split-files">
      <FullFileView file={left} empty="File is new on the RIGHT side." />
      <FullFileView file={right} empty="File was deleted from the RIGHT side." />
    </div>
  );
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
  repositoryRoot,
  importedComments,
  readOnly,
  onComment,
  onAsk,
}: {
  file: DiffFile;
  repositoryRoot: string;
  importedComments: ImportedComment[];
  readOnly: boolean;
  onComment: (anchor: Anchor) => void;
  onAsk: (anchor: Anchor) => void;
}) {
  const [activeHunk, setActiveHunk] = useState(0);
  useEffect(() => {
    document.getElementById("review-queue-active-hunk")?.scrollIntoView({
      block: "nearest",
      behavior: "smooth",
    });
  }, [activeHunk]);
  if (file.is_binary) {
    return <div className="code binary-state"><b>Binary file changed</b><p>The pinned Git patch is retained, but binary content is not rendered as text.</p></div>;
  }
  return (
    <div className="code" role="region" aria-label="Code diff" tabIndex={0}>
      <nav className="hunk-navigation" aria-label="Hunk navigation">
        <button disabled={activeHunk <= 0} onClick={() => setActiveHunk((value) => Math.max(0, value - 1))}>Previous hunk</button>
        <span>{file.hunks.length ? `${activeHunk + 1} / ${file.hunks.length}` : "No hunks"}</span>
        <button disabled={activeHunk >= file.hunks.length - 1} onClick={() => setActiveHunk((value) => Math.min(file.hunks.length - 1, value + 1))}>Next hunk</button>
        <span className="muted">Use Full file to expand context.</span>
      </nav>
      {file.hunks.map((hunk, index) => (
        <div id={index === activeHunk ? "review-queue-active-hunk" : undefined} className={index === activeHunk ? "active-hunk" : ""} key={`${hunk.old_start}:${hunk.new_start}:${index}`}>
          <DiffHunkView
            file={file}
            hunk={hunk}
            repositoryRoot={repositoryRoot}
            importedComments={importedComments}
            readOnly={readOnly}
            onComment={onComment}
            onAsk={onAsk}
          />
        </div>
      ))}
    </div>
  );
}

function DiffHunkView({
  file,
  hunk,
  repositoryRoot,
  importedComments,
  readOnly,
  onComment,
  onAsk,
}: {
  file: DiffFile;
  hunk: DiffHunk;
  repositoryRoot: string;
  importedComments: ImportedComment[];
  readOnly: boolean;
  onComment: (anchor: Anchor) => void;
  onAsk: (anchor: Anchor) => void;
}) {
  const [selection, setSelection] = useState<{ start: number; end: number } | null>(null);
  let oldLine = hunk.old_start;
  let newLine = hunk.new_start;
  const numberedLines = hunk.lines.map((line, index) => ({
    line,
    index,
    oldNumber: line.type === "addition" ? null : oldLine++,
    newNumber: line.type === "deletion" ? null : newLine++,
  }));
  const right = Boolean(file.new_path && file.new_blob_sha);
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
  const anchoredDiscussion = importedComments.filter((comment) => {
    const imported = comment.anchor;
    if (!imported || !anchor) return false;
    return imported.repository_id === anchor.repository_id
      && imported.workspace_relative_path === anchor.workspace_relative_path
      && imported.side === anchor.side
      && imported.end_line >= anchor.start_line
      && imported.start_line <= anchor.end_line;
  });
  return (
    <section className="diff-hunk">
      <div className="hunk-header">
        <span>@@ -{hunk.old_start},{hunk.old_lines} +{hunk.new_start},{hunk.new_lines} @@ {hunk.header}</span>
        <span>
          <button disabled={readOnly || !anchor} title={!anchor ? "The pinned blob is unavailable" : "Ask Copilot about this hunk"} onClick={() => anchor && onAsk(anchor)}>/ask</button>{" "}
          <button disabled={readOnly || !anchor} title={!anchor ? "The pinned blob is unavailable" : "Add a formal comment anchored to this hunk"} onClick={() => anchor && onComment(anchor)}>＋ Comment</button>
        </span>
      </div>
      {numberedLines.map(({ line, index, oldNumber, newNumber }) => {
        const selected = selection && index >= selection.start && index <= selection.end;
        return (
          <div
            className={`code-line ${line.type} ${selected ? "selected-code-line" : ""}`}
            key={index}
            role="button"
            tabIndex={0}
            aria-label={`Select ${path} line ${right ? newNumber ?? oldNumber : oldNumber ?? newNumber}`}
            onClick={(event) => {
              setSelection((current) => event.shiftKey && current
                ? { start: Math.min(current.start, index), end: Math.max(current.end, index) }
                : { start: index, end: index });
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                setSelection({ start: index, end: index });
              }
            }}
          >
            <span>{oldNumber ?? ""}</span><span>{newNumber ?? ""}</span>
            <code>{line.type === "addition" ? "+" : line.type === "deletion" ? "-" : " "}{line.content}</code>
          </div>
        );
      })}
      {anchoredDiscussion.map((comment) => (
        <details
          className={`imported-thread-inline ${comment.upstream_resolved ? "resolved-upstream" : ""}`}
          key={comment.id}
          open={!comment.upstream_resolved}
        >
          <summary>{comment.upstream_resolved ? "Resolved on GitHub" : "Imported review thread · read-only"}</summary>
          <article>
            <small>{comment.anchor?.workspace_relative_path}:{comment.anchor?.start_line}</small>
            <p><b>{comment.upstream_author}</b> · <time>{new Date(comment.upstream_created_at).toLocaleString()}</time></p>
            <p>{comment.body}</p>
            <a href={comment.source_url} target="_blank" rel="noreferrer">Open upstream thread</a>
          </article>
        </details>
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
    <details className="brief" open>
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
  const dialog = useDialogFocus(onClose);

  const invalidatePreflight = () => setPreflightFresh(false);
  const update = (field: keyof ReviewBrief, value: string) => {
    setBrief((current) => ({ ...current, [field]: value }));
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
        participatingRepositoryIds: participating.size ? [...participating] : [],
        preflightToken: null,
      });
      setPreflight(next);
      setParticipating(new Set(next.participatingRepositoryIds));
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
          <label>Workspace path<input value={workspacePath} onChange={(event) => { setWorkspacePath(event.target.value); setPreflight(null); setParticipating(new Set()); invalidatePreflight(); }} required autoFocus /></label>
          <label>Topic (stable)<input value={topic} onChange={(event) => { setTopic(event.target.value); invalidatePreflight(); }} required /></label>
          <label>Title<input value={brief.title} onChange={(event) => update("title", event.target.value)} required /></label>
          <label>What<textarea value={brief.what} onChange={(event) => update("what", event.target.value)} /></label>
          <label>Why<textarea value={brief.why} onChange={(event) => update("why", event.target.value)} /></label>
          <label>Approach / Alternatives<textarea value={brief.approach_alternatives} onChange={(event) => update("approach_alternatives", event.target.value)} /></label>
          <label>Testing<textarea value={brief.testing} onChange={(event) => update("testing", event.target.value)} /></label>
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
          <label>Clean destination<input value={destination} onChange={(event) => { setDestination(event.target.value); setPreview(null); setCompleted(false); }} /></label>
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
  const [installedVersion, setInstalledVersion] = useState("");
  const [diagnosticsPath, setDiagnosticsPath] = useState("");
  const [codeCopied, setCodeCopied] = useState(false);
  const [deviceFlowMessage, setDeviceFlowMessage] = useState("");
  const [devicePollDelay, setDevicePollDelay] = useState(5);
  const [, tick] = useState(0);
  const dialog = useDialogFocus(onClose);

  useEffect(() => {
    if (!deviceFlow || deviceFlow.phase === "expired") return;
    const timer = window.setInterval(() => tick((value) => value + 1), 1000);
    return () => window.clearInterval(timer);
  }, [deviceFlow]);

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
          <ConnectionRow label="Copilot /ask" status={health?.copilot} working={working} onConnect={() => void connect("copilot_app")} onDisconnect={(source) => void run(() => disconnectCapability("copilot_app", source))} />
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
}: {
  label: string;
  status?: ConnectionHealth["copilot"];
  working: boolean;
  onConnect: () => void;
  onDisconnect: (source: ConnectionHealth["copilot"]["source"]) => void;
}) {
  const connected = status?.state === "connected";
  return (
    <section className="connection-row">
      <div><b>{label}</b><p>{status?.explanation ?? "Checking connection…"}</p></div>
      <span className={connected ? "status good" : "status"}>{connected ? `✓ ${status?.account ?? status?.source.replaceAll("_", " ")}` : status?.state.replaceAll("_", " ") ?? "checking…"}</span>
      {connected ? (
        <button disabled={working} onClick={() => status && onDisconnect(status.source)}>
          {status?.source === "existing_copilot_cli" ? "Stop using existing sign-in" : "Disconnect"}
        </button>
      ) : (
        <button disabled={working || status?.state === "unavailable"} onClick={onConnect}>Connect app</button>
      )}
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
  const dialog = useDialogFocus(onClose);
  return (
    <div className="modal-backdrop">
      <section {...dialog} className="modal confirm-dialog" role="alertdialog" aria-modal="true" aria-labelledby="publish-title">
        <header><h2 id="publish-title">Publish GitHub review?</h2><button aria-label="Close" onClick={onClose}>×</button></header>
        <div className="detail-grid">
          <p><b>Target</b> {target.owner}/{target.repository} #{target.pull_number} at <code>{shortSha(target.head_sha)}</code></p>
          <p><b>Event</b> {attempt.preview.event.toUpperCase()}</p>
          <p><b>Formal comments</b> {attempt.request.comments.length}</p>
          {attempt.request.comments.map((comment) => (
            <article className="formal-comment" key={comment.formal_comment_id}>
              <small>{comment.disposition.replaceAll("_", " ")}{comment.fallback_reference ? ` · ${comment.fallback_reference}` : ""}</small>
              <p>{comment.body}</p>
            </article>
          ))}
          <p className="safe-copy">Only the formal decision and comments shown above will be published. `/ask` chats and imported comments are excluded.</p>
          {completed && <p className="status good">Published once as GitHub review {attempt.review_id}.</p>}
          {attempt.status === "unknown" && <p className="danger-text">The publish outcome is unknown. Inspect the pull request before trying anything else.</p>}
          {error && <ErrorPanel error={error} />}
          <div className="dialog-actions">
            <button onClick={onClose}>{completed ? "Done" : "Cancel"}</button>
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

async function githubAction(
  action: () => Promise<void>,
  setWorking: React.Dispatch<React.SetStateAction<boolean>>,
  setError: React.Dispatch<React.SetStateAction<CommandError | null>>,
) {
  setWorking(true);
  setError(null);
  try {
    await action();
  } catch (problem) {
    setError(toCommandError(problem));
  } finally {
    setWorking(false);
  }
}

function githubFilesToDiff(round: ReviewRound, files: GithubMaterializedFile[]): MaterializedDiff {
  const repository = round.manifest.repositories[0];
  const repositoryId = repository?.repository_id ?? round.topic_identity;
  return {
    repositories: [{
      repository_id: repositoryId,
      root: ".",
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

function fileKey(repositoryId: string, path: string) {
  return `${repositoryId}\u0000${path}`;
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
