import { readFileSync } from "node:fs";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const paginationTitle = "Fix pagination cursor drift across core-api and web-frontend";
const retryTitle = "Add retry backoff to sync worker";
const githubTitle = "Improve error messages for expired tokens";
const completedTitle = "Rename legacy config module path constant";
const styles = readFileSync("src/styles.css", "utf8");
const recoverableError = {
  code: "fixture_recoverable_error",
  message: "The requested review update could not be completed.",
  data_safety: "The immutable diff and saved review data remain unchanged.",
  next_step: "Retry the explicit review action.",
};

function setViewport(width: number, height = 720) {
  Object.defineProperty(window, "innerWidth", { configurable: true, value: width });
  Object.defineProperty(window, "innerHeight", { configurable: true, value: height });
  window.dispatchEvent(new Event("resize"));
}

async function renderFixtureApp() {
  const { App } = await import("../src/App");
  render(<App />);
  await screen.findByRole("heading", { name: "Queue Home" });
}

async function openPaginationReview() {
  await openReview(paginationTitle);
}

async function openReview(title: string) {
  await renderFixtureApp();
  const card = screen.getByText(title).closest("article");
  if (!card) throw new Error("Fixture pagination review card was not rendered.");
  fireEvent.click(within(card).getByRole("button", { name: "Open review" }));
  expect((await screen.findAllByRole("region", { name: "Code diff" })).length).toBeGreaterThan(0);
}

async function openChat() {
  fireEvent.click(screen.getByRole("button", { name: "Open chat" }));
  await screen.findByRole("complementary", { name: "Round chat" });
}

afterEach(() => {
  vi.doUnmock("../src/api.fixture.ts");
  vi.resetModules();
  setViewport(1280);
});

beforeEach(() => setViewport(1024));

describe("fixture-backed reviewer recovery", () => {
  it("opens a reviewer without creating a Copilot conversation or discovering capabilities", async () => {
    let active = vi.fn();
    let capabilities = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      active = vi.fn((...args: Parameters<typeof api.activeConversation>) =>
        api.activeConversation(...args));
      capabilities = vi.fn((...args: Parameters<typeof api.copilotCapabilities>) =>
        api.copilotCapabilities(...args));
      return {
        ...api,
        activeConversation: active,
        copilotCapabilities: capabilities,
      };
    });

    await openReview("Add retry backoff to sync worker");
    await Promise.resolve();
    expect(active).not.toHaveBeenCalled();
    expect(capabilities).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Open chat" }));
    await waitFor(() => expect(active).toHaveBeenCalledTimes(1));
    expect(capabilities).toHaveBeenCalledTimes(1);
  });

  it("shows the running app version without checking the update feed", async () => {
    let checkForUpdate = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      checkForUpdate = vi.fn(api.checkForUpdate);
      return { ...api, checkForUpdate };
    });

    await renderFixtureApp();
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const dialog = await screen.findByRole("dialog", { name: "Application settings" });

    expect(await within(dialog).findByText("Review Queue 0.1.0-fixture")).toBeVisible();
    expect(checkForUpdate).not.toHaveBeenCalled();
    expect(within(dialog).getByRole("button", { name: "Check for updates" })).toBeVisible();
  });

  it("only reorders from the focused queue-card surface and prevents browser shortcut behavior", async () => {
    let moveRound = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      moveRound = vi.fn().mockResolvedValue(undefined);
      return { ...api, moveRound };
    });

    await renderFixtureApp();
    const card = screen.getByText(paginationTitle).closest("article");
    if (!card) throw new Error("The fixture pagination review card was not rendered.");
    const open = within(card).getByRole("button", { name: "Open review" });

    open.focus();
    expect(fireEvent.keyDown(open, { key: "ArrowDown", altKey: true })).toBe(true);
    expect(moveRound).not.toHaveBeenCalled();

    card.focus();
    expect(fireEvent.keyDown(card, { key: "ArrowDown", altKey: true })).toBe(false);
    await waitFor(() => expect(moveRound).toHaveBeenCalledTimes(1));
    expect(moveRound).toHaveBeenCalledWith(expect.any(String), expect.any(Number));
  });

  it("runs request, complete, requeue, and delete only through explicit lifecycle actions", async () => {
    let requestChanges = vi.fn();
    let completeRound = vi.fn();
    let requeueRound = vi.fn();
    let purgeRound = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      requestChanges = vi.fn((...args: Parameters<typeof api.requestChanges>) =>
        api.requestChanges(...args));
      completeRound = vi.fn((...args: Parameters<typeof api.completeRound>) =>
        api.completeRound(...args));
      requeueRound = vi.fn((...args: Parameters<typeof api.requeueRound>) =>
        api.requeueRound(...args));
      purgeRound = vi.fn((...args: Parameters<typeof api.purgeRound>) =>
        api.purgeRound(...args));
      return { ...api, requestChanges, completeRound, requeueRound, purgeRound };
    });

    await openReview(retryTitle);
    fireEvent.click(screen.getByRole("button", { name: "Request changes" }));
    await waitFor(() => expect(requestChanges).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole("button", { name: "Complete" }));
    await screen.findByRole("heading", { name: "Queue Home" });
    await waitFor(() => expect(completeRound).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("checkbox", { name: /show completed .* old rounds/i }));
    const completedCard = (await screen.findByText(retryTitle)).closest("article");
    if (!completedCard) throw new Error("The completed lifecycle card was not rendered.");
    expect(within(completedCard).getByText("completed")).toBeVisible();
    fireEvent.click(within(completedCard).getByRole("button", { name: "Requeue" }));
    await waitFor(() => expect(requeueRound).toHaveBeenCalledTimes(1));

    const requeuedCard = (await screen.findByText(retryTitle)).closest("article");
    if (!requeuedCard) throw new Error("The requeued lifecycle card was not rendered.");
    expect(within(requeuedCard).getByText("queued")).toBeVisible();
    const more = requeuedCard.querySelector("summary[aria-label^='More actions']");
    if (!more) throw new Error("The lifecycle card did not expose more actions.");
    fireEvent.click(more);
    fireEvent.click(within(requeuedCard).getByRole("button", { name: "Delete" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Delete this review round?" });
    expect(dialog).toHaveTextContent("Source files, repositories, and submission commits are never touched.");
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(purgeRound).not.toHaveBeenCalled();
    expect(screen.getByText(retryTitle)).toBeVisible();

    fireEvent.click(within(requeuedCard).getByRole("button", { name: "Delete" }));
    const confirm = await screen.findByRole("alertdialog", { name: "Delete this review round?" });
    fireEvent.click(within(confirm).getByRole("button", { name: "Delete permanently" }));
    await waitFor(() => expect(purgeRound).toHaveBeenCalledWith(expect.any(String), "delete"));
    await waitFor(() => expect(screen.queryByText(retryTitle)).not.toBeInTheDocument());
  });

  it("cancels local approval without mutation and confirms the separate purge action", async () => {
    let purgeRound = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      purgeRound = vi.fn((...args: Parameters<typeof api.purgeRound>) =>
        api.purgeRound(...args));
      return { ...api, purgeRound };
    });

    await openReview(paginationTitle);
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    const first = await screen.findByRole("alertdialog", {
      name: "Approve and purge this local round?",
    });
    expect(first).toHaveTextContent("Source files, repositories, and submission commits are never touched.");
    fireEvent.click(within(first).getByRole("button", { name: "Cancel" }));
    expect(purgeRound).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Approve" })).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    const second = await screen.findByRole("alertdialog", {
      name: "Approve and purge this local round?",
    });
    fireEvent.click(within(second).getByRole("button", { name: "Approve and purge" }));
    await waitFor(() =>
      expect(purgeRound).toHaveBeenCalledWith(expect.any(String), "approve_local"));
    await screen.findByRole("heading", { name: "Queue Home" });
    expect(screen.queryByText(paginationTitle)).not.toBeInTheDocument();
  });

  it("offers an explicit switch from app OAuth back to the existing CLI sign-in", async () => {
    let selectExisting = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      const health = await api.connectionStatus();
      health.copilot = {
        ...health.copilot,
        state: "connected",
        source: "app_owned_oauth",
        account: "fixture-app-account",
      };
      selectExisting = vi.fn().mockResolvedValue({
        ...health,
        copilot: {
          ...health.copilot,
          source: "existing_copilot_cli",
          account: health.cli.account,
        },
      });
      return {
        ...api,
        connectionStatus: vi.fn().mockResolvedValue(health),
        selectExistingCopilotCli: selectExisting,
      };
    });

    await renderFixtureApp();
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const dialog = await screen.findByRole("dialog", { name: "Application settings" });
    const useExisting = within(dialog).getByRole("button", { name: "Use existing CLI sign-in" });

    fireEvent.click(useExisting);

    await waitFor(() => expect(selectExisting).toHaveBeenCalledTimes(1));
    expect(within(dialog).getByRole("button", { name: "Stop using existing sign-in" })).toBeVisible();
  });

  it("adopts connection health that resolves after Settings opens", async () => {
    let resolveHealth: (health: Awaited<ReturnType<typeof import("../src/api.fixture").connectionStatus>>) => void = () => {};
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      const health = await api.connectionStatus();
      const deferred = new Promise<typeof health>((resolve) => {
        resolveHealth = resolve;
      });
      return {
        ...api,
        connectionStatus: vi.fn().mockReturnValue(deferred),
      };
    });

    await renderFixtureApp();
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const dialog = await screen.findByRole("dialog", { name: "Application settings" });

    expect(within(dialog).getAllByText("Checking connection…")).toHaveLength(3);
    expect(within(dialog).getAllByText("checking…")).toHaveLength(4);

    const api = await import("../src/api.fixture");
    await act(async () => {
      resolveHealth(await api.retryConnection());
    });

    await waitFor(() => {
      expect(within(dialog).queryByText("Checking connection…")).not.toBeInTheDocument();
      expect(within(dialog).queryByText("checking…")).not.toBeInTheDocument();
    });
    expect(within(dialog).getByRole("button", { name: "Stop using existing sign-in" })).toBeVisible();
    expect(within(dialog).getByText(/healthy ·/i)).toBeVisible();
  });

  it("resolves a GitHub PR read-only before an explicit, exact confirmation queues it", async () => {
    let resolvePullRequest = vi.fn();
    let confirmPullRequest = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      const expected = await api.previewGithubPullRequest("https://github.com/acme-widgets/auth-service/pull/777");
      resolvePullRequest = vi.fn().mockResolvedValue(expected);
      confirmPullRequest = vi.fn((...args: Parameters<typeof api.confirmGithubPullRequest>) =>
        api.confirmGithubPullRequest(...args));
      return {
        ...api,
        previewGithubPullRequest: resolvePullRequest,
        confirmGithubPullRequest: confirmPullRequest,
      };
    });

    await renderFixtureApp();
    fireEvent.click(screen.getByRole("button", { name: "Review PR" }));
    const dialog = await screen.findByRole("dialog", { name: "Review pull request" });
    fireEvent.change(within(dialog).getByRole("textbox", { name: "GitHub pull request URL" }), {
      target: { value: "https://github.com/acme-widgets/auth-service/pull/777" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "Resolve pull request" }));

    await waitFor(() => expect(resolvePullRequest).toHaveBeenCalledWith("https://github.com/acme-widgets/auth-service/pull/777"));
    expect(confirmPullRequest).not.toHaveBeenCalled();
    expect(await within(dialog).findByRole("region", { name: "Pull request preview" })).toHaveTextContent("acme-widgets/auth-service#777");
    expect(within(dialog).getByText("Head SHA")).toBeVisible();
    expect(within(dialog).queryByText(/Resolve shows read-only metadata only/i)).not.toBeInTheDocument();

    fireEvent.click(within(dialog).getByRole("button", { name: "Confirm and add to queue" }));
    await waitFor(() => expect(confirmPullRequest).toHaveBeenCalledTimes(1));
    expect(confirmPullRequest.mock.calls[0][0]).toMatchObject({
      locator: { host: "github.com", owner: "acme-widgets", repository: "auth-service", pull_number: 777 },
      metadata: { head_sha: expect.any(String), base_sha: expect.any(String), state: "open", is_draft: false },
    });
  });

  it("records an explicitly selected originating session in both capture preview and submission without injecting into it", async () => {
    let preflight = vi.fn();
    let submit = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      preflight = vi.fn((...args: Parameters<typeof api.preflightLocal>) => api.preflightLocal(...args));
      submit = vi.fn((...args: Parameters<typeof api.submitLocal>) => api.submitLocal(...args));
      return { ...api, preflightLocal: preflight, submitLocal: submit };
    });

    await renderFixtureApp();
    fireEvent.click(screen.getByRole("button", { name: "Submit local" }));
    const dialog = await screen.findByRole("dialog", { name: "Submit local review" });
    const workspace = within(dialog).getByRole("textbox", { name: "Workspace path" });
    fireEvent.change(workspace, { target: { value: "/home/build/workspaces/web-platform" } });
    fireEvent.change(within(dialog).getByRole("textbox", { name: "Topic (stable)" }), { target: { value: "origin-route-fixture" } });
    fireEvent.change(within(dialog).getByRole("textbox", { name: "Title" }), { target: { value: "Capture route provenance" } });

    const session = await within(dialog).findByRole("combobox", { name: "Originating session" });
    await waitFor(() => expect(session).toHaveValue("route-fixture-1"));
    expect(within(dialog).getByText(/Review Queue never queues, interrupts, types, or injects/i)).toBeVisible();
    expect(within(dialog).getByText(/Original cwd/i).closest("p")).toHaveTextContent("/home/build/workspaces/web-platform");

    fireEvent.click(within(dialog).getByRole("button", { name: "Preview repositories" }));
    await waitFor(() => expect(preflight).toHaveBeenCalledTimes(1));
    expect(preflight.mock.calls[0][0]).toMatchObject({ originRouteId: "route-fixture-1" });
    await within(dialog).findByRole("region", { name: "Detected repositories" });

    fireEvent.change(session, { target: { value: "route-fixture-2" } });
    expect(within(dialog).getByText("Selection or form changed. Preview again before capture.")).toBeVisible();
    fireEvent.click(within(dialog).getByRole("button", { name: "Preview repositories" }));
    await waitFor(() => expect(preflight).toHaveBeenCalledTimes(2));
    expect(preflight.mock.calls[1][0]).toMatchObject({ originRouteId: "route-fixture-2" });
    await waitFor(() => expect(within(dialog).getByRole("button", { name: "Capture snapshot" })).toBeEnabled());

    fireEvent.click(within(dialog).getByRole("button", { name: "Capture snapshot" }));
    await waitFor(() => expect(submit).toHaveBeenCalledTimes(1));
    expect(submit.mock.calls[0][0]).toMatchObject({
      originRouteId: "route-fixture-2",
      preflightToken: expect.any(String),
    });
  });

  it("delivers formal feedback only after exact confirmation and never on cancel", async () => {
    let deliver = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      deliver = vi.fn((...args: Parameters<typeof api.deliverFeedback>) =>
        api.deliverFeedback(...args));
      return { ...api, deliverFeedback: deliver };
    });

    await openPaginationReview();
    fireEvent.click(screen.getByRole("button", { name: "Formal feedback" }));
    const drawer = await screen.findByRole("dialog", { name: "Formal feedback" });
    fireEvent.click(within(drawer).getByRole("button", { name: "Prepare immutable prompt" }));
    const reviewSend = await within(drawer).findByRole("button", { name: "Review Send…" });

    fireEvent.click(reviewSend);
    let confirmation = await screen.findByRole("alertdialog", {
      name: "Send formal feedback to originating agent?",
    });
    expect(within(confirmation).getByText(/2 undelivered comment revisions/i)).toBeVisible();
    expect(within(confirmation).getByText(/Confirm Send performs one desktop-only ACP request/i)).toBeVisible();
    fireEvent.click(within(confirmation).getByRole("button", { name: "Cancel" }));
    expect(deliver).not.toHaveBeenCalled();

    fireEvent.click(reviewSend);
    confirmation = await screen.findByRole("alertdialog", {
      name: "Send formal feedback to originating agent?",
    });
    fireEvent.click(within(confirmation).getByRole("button", { name: "Confirm Send" }));
    await waitFor(() => expect(deliver).toHaveBeenCalledTimes(1));
    expect(deliver).toHaveBeenCalledWith(
      expect.any(String),
      expect.any(String),
      "route-fixture-1",
      "queue",
    );
    expect(await within(drawer).findByText(/Delivered once to the originating agent as receipt/i)).toBeVisible();
  });

  it("requires a busy-agent policy and keeps copy and reproduction recovery after failure", async () => {
    const unreachable = {
      code: "acp_endpoint_unreachable",
      message: "The originating agent ACP endpoint could not be reached.",
      data_safety: "The immutable feedback remains saved and its revisions are still undelivered.",
      next_step: "Reconnect the agent, retry this explicit Send, or copy the prompt and reproduce the saved round.",
    };
    let deliver = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      const busyRoutes = (await api.listAgentRoutes()).map((route) =>
        route.id === "route-fixture-1" ? { ...route, status: "busy" } : route);
      deliver = vi.fn().mockRejectedValue(unreachable);
      return {
        ...api,
        listAgentRoutes: vi.fn().mockResolvedValue(busyRoutes),
        prepareFeedbackHandoff: vi.fn(async (...args: Parameters<typeof api.prepareFeedbackHandoff>) => ({
          ...await api.prepareFeedbackHandoff(...args),
          route_status: "busy",
          busy_policy_required: true,
        })),
        deliverFeedback: deliver,
      };
    });

    await openPaginationReview();
    fireEvent.click(screen.getByRole("button", { name: "Formal feedback" }));
    const drawer = await screen.findByRole("dialog", { name: "Formal feedback" });
    fireEvent.click(within(drawer).getByRole("button", { name: "Prepare immutable prompt" }));
    const queue = await within(drawer).findByRole("radio", { name: "Queue until idle" });
    const interrupt = within(drawer).getByRole("radio", { name: "Interrupt current turn" });
    expect(queue).toBeChecked();
    expect(interrupt).not.toBeChecked();
    fireEvent.click(interrupt);
    expect(interrupt).toBeChecked();
    fireEvent.click(within(drawer).getByRole("button", { name: "Review Send…" }));
    const confirmation = await screen.findByRole("alertdialog", {
      name: "Send formal feedback to originating agent?",
    });
    fireEvent.click(within(confirmation).getByRole("button", { name: "Confirm Send" }));

    await waitFor(() => expect(deliver).toHaveBeenCalledTimes(1));
    expect(deliver.mock.calls[0][3]).toBe("interrupt");
    const failure = await within(confirmation).findByRole("alert");
    expect(failure).toHaveTextContent(unreachable.message);
    expect(failure).toHaveTextContent(unreachable.data_safety);
    expect(failure).toHaveTextContent(
      "Choose Copy feedback prompt and submit it manually to the intended agent.",
    );
    expect(within(confirmation).getByRole("button", { name: "Copy immutable prompt" })).toBeVisible();
    expect(within(confirmation).getByRole("button", { name: "Preview reproduction…" })).toBeVisible();
  });

  it("opens cached GitHub review state without an implicit refresh, then refreshes only on click", async () => {
    let cachedRound = vi.fn();
    let refreshComments = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      const listRounds = vi.fn(async (...args: Parameters<typeof api.listRounds>) =>
        (await api.listRounds(...args)).map((round) => round.brief.title === githubTitle
          ? {
              ...round,
              manifest: {
                ...round.manifest,
                repositories: round.manifest.repositories.map((repository) => ({
                  ...repository,
                  root: "",
                })),
              },
            }
          : round));
      cachedRound = vi.fn(async (...args: Parameters<typeof api.cachedGithubRound>) => {
        const cached = await api.cachedGithubRound(...args);
        return {
          ...cached,
          imported_comments: cached.imported_comments.map((comment) => ({
            ...comment,
            anchor: comment.anchor ? { ...comment.anchor, side: comment.anchor.side.toLowerCase() } : null,
          })),
          last_staleness: {
            ...cached.last_staleness!,
            observed_head_sha: "e".repeat(64),
          },
        };
      });
      refreshComments = vi.fn(api.refreshGithubComments);
      return {
        ...api,
        listRounds,
        cachedGithubRound: cachedRound,
        refreshGithubComments: refreshComments,
      };
    });

    await openReview(githubTitle);

    await waitFor(() => expect(cachedRound).toHaveBeenCalledTimes(1));
    expect(refreshComments).not.toHaveBeenCalled();
    expect(screen.getAllByRole("region", { name: "Code diff" }).length).toBeGreaterThan(0);
    expect(screen.getByText(/Nice fix — can we also cover the "revoked" case/i)).toBeVisible();
    expect(screen.getByRole("button", { name: "Reply formally" })).toBeVisible();
    expect(screen.queryByLabelText(/Threads at \//i)).not.toBeInTheDocument();
    expect(screen.getByText(/Head moved from/i)).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "Refresh comments" }));
    await waitFor(() => expect(refreshComments).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(screen.queryByText(/Head moved from/i)).not.toBeInTheDocument());
    expect(screen.getByText(/Nice fix — can we also cover the "revoked" case/i)).toBeVisible();
  });

  it("renders the complete recovery contract, keeps the diff visible, and retries explicitly", async () => {
    let setViewed = vi.fn();
    const staleError = { ...recoverableError, code: "github_round_stale" };
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      setViewed = vi.fn()
        .mockRejectedValueOnce(staleError)
        .mockImplementation(api.setFileViewed);
      return { ...api, setFileViewed: setViewed };
    });

    await openPaginationReview();
    const markViewed = screen.getAllByRole("button")
      .find((button) => button.classList.contains("viewed-toggle") && button.textContent?.includes("Mark viewed"));
    if (!markViewed) throw new Error("The selected file did not expose its Viewed action.");
    fireEvent.click(markViewed);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(staleError.data_safety);
    expect(alert).toHaveTextContent("Why this matters:");
    expect(alert).toHaveTextContent("Refresh into new round");
    expect(alert).toHaveTextContent("Diagnostics:");
    expect(alert).toHaveTextContent("Back:");
    expect(screen.getAllByRole("region", { name: "Code diff" }).length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Dismiss" })).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(setViewed).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(screen.getAllByRole("region", { name: "Code diff" }).length).toBeGreaterThan(0);
  });

  it("lets an interrupted durable /ask turn start a fresh session and retry only on explicit click", async () => {
    let clearChat = vi.fn();
    let startSession = vi.fn();
    let sendPrompt = vi.fn();
    let releaseSend = () => {};
    const sendGate = new Promise<void>((resolve) => {
      releaseSend = resolve;
    });
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      clearChat = vi.fn((...args: Parameters<typeof api.clearCopilotChat>) =>
        api.clearCopilotChat(...args));
      startSession = vi.fn((...args: Parameters<typeof api.startCopilotSession>) =>
        api.startCopilotSession(...args));
      sendPrompt = vi.fn(async (...args: Parameters<typeof api.sendCopilotPrompt>) => {
        await sendGate;
        return api.sendCopilotPrompt(...args);
      });
      return {
        ...api,
        clearCopilotChat: clearChat,
        startCopilotSession: startSession,
        sendCopilotPrompt: sendPrompt,
      };
    });

    await openPaginationReview();
    await openChat();

    expect((await screen.findAllByText(/Review Queue restarted before Copilot finished responding/i)).length).toBeGreaterThan(0);
    const retry = screen.getByRole("button", { name: /retry as new prompt/i });
    expect(retry).toBeVisible();
    expect(clearChat).not.toHaveBeenCalled();
    expect(startSession).not.toHaveBeenCalled();
    expect(sendPrompt).not.toHaveBeenCalled();

    act(() => {
      retry.click();
      retry.click();
    });
    expect(retry).toBeDisabled();
    await waitFor(() => expect(sendPrompt).toHaveBeenCalledTimes(1));
    releaseSend();
    expect(await screen.findByRole("button", { name: /cancel/i })).toBeVisible();
    expect(clearChat).toHaveBeenCalledTimes(1);
    expect(startSession).toHaveBeenCalledTimes(1);
    expect(sendPrompt).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("combobox", { name: /previous chats/i })).toBeVisible();
  });

  it("submits a rapid double Send once and disables prompt controls before the API resolves", async () => {
    let sendPrompt = vi.fn();
    let releaseSend = () => {};
    const sendGate = new Promise<void>((resolve) => {
      releaseSend = resolve;
    });
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      sendPrompt = vi.fn(async (...args: Parameters<typeof api.sendCopilotPrompt>) => {
        await sendGate;
        return api.sendCopilotPrompt(...args);
      });
      return { ...api, sendCopilotPrompt: sendPrompt };
    });

    await openReview("Add retry backoff to sync worker");
    await openChat();
    fireEvent.click(await screen.findByRole("button", { name: "Start Copilot" }));
    const input = await screen.findByRole("textbox", { name: "Ask a follow-up" });
    await waitFor(() => expect(input).toBeEnabled());
    fireEvent.change(input, { target: { value: "Could this retry twice?" } });
    const send = screen.getByRole("button", { name: "Send" });
    await waitFor(() => expect(send).toBeEnabled());

    act(() => {
      send.click();
      send.click();
    });

    expect(sendPrompt).toHaveBeenCalledTimes(1);
    expect(send).toBeDisabled();
    expect(input).toBeDisabled();

    releaseSend();
    expect(await screen.findByRole("button", { name: "Cancel" })).toBeVisible();
    expect(sendPrompt).toHaveBeenCalledTimes(1);
  });

  it("keeps archived Previous chats permanently read-only, including interrupted turns", async () => {
    let clearChat = vi.fn();
    let startSession = vi.fn();
    let sendPrompt = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      clearChat = vi.fn((...args: Parameters<typeof api.clearCopilotChat>) =>
        api.clearCopilotChat(...args));
      startSession = vi.fn((...args: Parameters<typeof api.startCopilotSession>) =>
        api.startCopilotSession(...args));
      sendPrompt = vi.fn((...args: Parameters<typeof api.sendCopilotPrompt>) =>
        api.sendCopilotPrompt(...args));
      return {
        ...api,
        clearCopilotChat: clearChat,
        startCopilotSession: startSession,
        sendCopilotPrompt: sendPrompt,
        listAskTurns: async (conversationId: string) => {
          const turns = await api.listAskTurns(conversationId);
          return turns.map((turn) => turn.prompt.includes("current pagination cursor format")
            ? { ...turn, state: "interrupted" as const, failure_reason: "Archived fixture interruption." }
            : turn);
        },
      };
    });

    await openPaginationReview();
    await openChat();
    const previous = screen.getByRole("combobox", { name: /previous chats/i });
    const archived = within(previous).getByRole("option", { name: /previous chat 1/i });
    fireEvent.change(previous, { target: { value: (archived as HTMLOptionElement).value } });

    expect(await screen.findByText("Archived fixture interruption.")).toBeVisible();
    const retry = screen.getByRole("button", { name: /retry as new prompt/i });
    expect(retry).toBeDisabled();
    expect(retry).toHaveAttribute(
      "title",
      "Archived chats are permanently read-only. Return to Current chat to continue.",
    );
    expect(screen.getByRole("button", { name: "Clear chat" })).toBeDisabled();
    expect(screen.getByRole("textbox", { name: /ask a follow-up/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Start Copilot" })).not.toBeInTheDocument();

    fireEvent.click(retry);
    fireEvent.click(screen.getByRole("button", { name: "Clear chat" }));
    await Promise.resolve();
    expect(clearChat).not.toHaveBeenCalled();
    expect(startSession).not.toHaveBeenCalled();
    expect(sendPrompt).not.toHaveBeenCalled();
  });

  it("keeps completed round chat controls and interrupted-turn retry read-only", async () => {
    let clearChat = vi.fn();
    let startSession = vi.fn();
    let sendPrompt = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      const conversation: NonNullable<Awaited<ReturnType<typeof api.currentConversation>>> = {
        id: "completed-round-conversation",
        round_id: "round-local-rename-config",
        session_state: "can_continue",
        history_only_reason: null,
        provider_session_label: "saved Copilot session",
        options: [],
        created_at: "2026-07-29T00:00:00.000Z",
        archived_at: null,
      };
      const interrupted: Awaited<ReturnType<typeof api.listAskTurns>>[number] = {
        id: "completed-round-interrupted-turn",
        conversation_id: conversation.id,
        idempotency_key: "completed-round-interrupted-idempotency",
        prompt: "Can this completed review be retried?",
        anchor: null,
        option_values: {},
        state: "interrupted",
        created_at: "2026-07-29T00:00:01.000Z",
        completed_at: null,
        failure_reason: "Completed round fixture interruption.",
        response_text: "",
      };
      clearChat = vi.fn((...args: Parameters<typeof api.clearCopilotChat>) =>
        api.clearCopilotChat(...args));
      startSession = vi.fn((...args: Parameters<typeof api.startCopilotSession>) =>
        api.startCopilotSession(...args));
      sendPrompt = vi.fn((...args: Parameters<typeof api.sendCopilotPrompt>) =>
        api.sendCopilotPrompt(...args));
      return {
        ...api,
        currentConversation: (roundId: string) =>
          roundId === conversation.round_id ? Promise.resolve(conversation) : api.currentConversation(roundId),
        listPreviousChats: (roundId: string) =>
          roundId === conversation.round_id ? Promise.resolve([]) : api.listPreviousChats(roundId),
        listAskTurns: (conversationId: string) =>
          conversationId === conversation.id ? Promise.resolve([interrupted]) : api.listAskTurns(conversationId),
        clearCopilotChat: clearChat,
        startCopilotSession: startSession,
        sendCopilotPrompt: sendPrompt,
      };
    });

    await renderFixtureApp();
    fireEvent.click(screen.getByRole("checkbox", { name: /show completed .* old rounds/i }));
    const card = await screen.findByText(completedTitle);
    const article = card.closest("article");
    if (!article) throw new Error("The completed fixture review card was not rendered.");
    fireEvent.click(within(article).getByRole("button", { name: "Open review" }));
    expect((await screen.findAllByRole("region", { name: "Code diff" })).length).toBeGreaterThan(0);
    await openChat();

    expect(await screen.findByText("Completed round fixture interruption.")).toBeVisible();
    const retry = screen.getByRole("button", { name: /retry as new prompt/i });
    expect(retry).toBeDisabled();
    expect(retry).toHaveAttribute("title", "Completed — Requeue to review again");
    expect(screen.getByRole("button", { name: "Clear chat" })).toBeDisabled();
    expect(screen.getByRole("textbox", { name: /ask a follow-up/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Start Copilot" })).not.toBeInTheDocument();

    fireEvent.click(retry);
    fireEvent.click(screen.getByRole("button", { name: "Clear chat" }));
    await Promise.resolve();
    expect(clearChat).not.toHaveBeenCalled();
    expect(startSession).not.toHaveBeenCalled();
    expect(sendPrompt).not.toHaveBeenCalled();
  });

  it("renders saved /ask and formal threads inline and converts a Copilot answer into a formal draft", async () => {
    await openPaginationReview();

    await screen.findAllByText(
      /Why do we decode the cursor again inside decode_cursor instead of trusting the caller already validated it/i,
    );
    const askThread = Array.from(document.querySelectorAll<HTMLElement>(".inline-ask-thread"))
      .find((thread) => thread.textContent?.includes("Why do we decode the cursor again"));
    if (!askThread) throw new Error("The saved Copilot turn was not rendered inline.");
    expect(within(askThread).getByText(/Copilot completed/i)).toBeVisible();
    expect(within(askThread).getByText(/We decode twice because/i)).toBeVisible();

    expect(await screen.findByText(/Good call bumping a version into the payload/i, {
      selector: ".inline-formal-comment p",
    })).toBeVisible();

    const cursorTreeItem = screen.getByText("cursor.py").closest("li");
    if (!cursorTreeItem) throw new Error("The cursor file was not present in the file tree.");
    expect(within(cursorTreeItem).getByText("◇ 1")).toBeVisible();

    fireEvent.click(within(askThread).getByRole("button", { name: "Convert to comment" }));
    const drawer = await screen.findByRole("dialog", { name: "Formal feedback" });
    const draft = within(drawer).getByRole("textbox", {
      name: /Comment on core-api\/src\/pagination\/cursor\.py:17–19/i,
    });
    expect((draft as HTMLTextAreaElement).value).toContain("We decode twice because");
  });

  it("keeps an imported GitHub thread inline and preserves its upstream thread when replying formally", async () => {
    let createComment = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      createComment = vi.fn((...args: Parameters<typeof api.createFormalComment>) =>
        api.createFormalComment(...args));
      return { ...api, createFormalComment: createComment };
    });

    await openReview(githubTitle);
    const tokenFile = (await screen.findByText("token_errors.ts")).closest("button");
    if (!tokenFile) throw new Error("The token error source file was not present in the file tree.");
    fireEvent.click(tokenFile);

    const importedBody = await screen.findByText(
      /Nice fix — can we also cover the "revoked" case with a similarly specific message/i,
    );
    expect(screen.getAllByText(
      /Nice fix — can we also cover the "revoked" case with a similarly specific message/i,
    )).toHaveLength(1);
    const importedThread = importedBody.closest(".imported-thread-inline");
    if (!importedThread) throw new Error("The imported GitHub comment was not rendered inline.");

    fireEvent.click(within(importedThread).getByRole("button", { name: "Reply formally" }));
    const drawer = await screen.findByRole("dialog", { name: "Formal feedback" });
    const draft = within(drawer).getByRole("textbox", {
      name: /Comment on auth-service\/src\/auth\/token_errors\.ts:9–9/i,
    });
    fireEvent.change(draft, { target: { value: "I added coverage for revoked tokens too." } });
    fireEvent.click(within(drawer).getByRole("button", { name: "Add comment" }));

    await waitFor(() => expect(createComment).toHaveBeenCalledTimes(1));
    expect(createComment.mock.calls[0][3]).toBe("thread-1");
    expect(await within(drawer).findByText("I added coverage for revoked tokens too.")).toBeVisible();

    fireEvent.click(within(drawer).getByRole("button", { name: "Close formal feedback" }));
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    const publish = screen.getByRole("button", { name: "Publish review" });
    await waitFor(() => expect(publish).toBeEnabled());
    expect(screen.getByRole("button", { name: /Queue Home/ })).toBeVisible();
    fireEvent.click(publish);

    const publishDialog = await screen.findByRole("alertdialog", { name: "Publish GitHub review?" });
    expect(within(publishDialog).getByText("Review write").closest("p")).toHaveTextContent("APPROVE · 0 comments");
    expect(within(publishDialog).getByText("Threaded reply writes").closest("p")).toHaveTextContent("1");
    expect(within(publishDialog).getByText("I added coverage for revoked tokens too.")).toBeVisible();
    expect(publishDialog.querySelector(".safe-copy")).toHaveTextContent(
      "one GitHub review write plus 1 threaded reply write",
    );

    fireEvent.click(within(publishDialog).getByRole("button", { name: "Publish APPROVE" }));
    expect(await within(publishDialog).findByText(/Published once as GitHub review/i)).toBeVisible();
    expect(within(publishDialog).getByText("1 threaded reply published.")).toBeVisible();
  });

  it("keeps publish unreachable without a decision and explains the one enabling action", async () => {
    await openReview(githubTitle);
    const publish = screen.getByRole("button", { name: "Publish review" });
    await waitFor(() => expect(publish).toBeDisabled());
    expect(publish).toHaveAttribute(
      "title",
      "Record Approve or Request changes before publishing",
    );
    expect(screen.queryByRole("alertdialog", { name: "Publish GitHub review?" })).not.toBeInTheDocument();
  });

  it("does not let a late decision from a replaced GitHub round enable publishing", async () => {
    let resolveOldDecision: ((decision: "approve" | "request_changes" | null) => void) | undefined;
    let getDecision = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      const oldRoundId = "round-github-expired-tokens";
      const oldRound = await api.getRound(oldRoundId);
      const refreshedRound = {
        ...oldRound,
        id: "round-github-expired-tokens-refreshed",
        manifest_hash: "f".repeat(64),
      };
      getDecision = vi.fn((roundId: string) => {
        if (roundId === oldRoundId) {
          return new Promise<"approve" | "request_changes" | null>((resolve) => {
            resolveOldDecision = resolve;
          });
        }
        return Promise.resolve(null);
      });
      return {
        ...api,
        getRoundDecision: getDecision,
        cachedGithubRound: async (roundId: string) => {
          const result = await api.cachedGithubRound(
            roundId === refreshedRound.id ? oldRoundId : roundId,
          );
          return roundId === oldRoundId
            ? {
                ...result,
                last_staleness: {
                  ...result.last_staleness!,
                  observed_head_sha: "e".repeat(64),
                },
              }
            : { ...result, round_id: refreshedRound.id };
        },
        refreshGithubComments: async (roundId: string) => {
          const result = await api.refreshGithubComments(
            roundId === refreshedRound.id ? oldRoundId : roundId,
          );
          return {
            ...result,
            staleness: {
              ...result.staleness,
              observed_head_sha: "e".repeat(64),
            },
          };
        },
        refreshGithubRound: vi.fn().mockResolvedValue({
          outcome: "superseded",
          round: refreshedRound,
        }),
        openGithubPullRequest: (roundId: string) =>
          api.openGithubPullRequest(roundId === refreshedRound.id ? oldRoundId : roundId),
        listFormalComments: (roundId: string) =>
          roundId === refreshedRound.id ? Promise.resolve([]) : api.listFormalComments(roundId),
      };
    });

    await openReview(githubTitle);
    fireEvent.click(await screen.findByRole("button", { name: "Refresh into new round" }));
    await waitFor(() => expect(getDecision).toHaveBeenCalledWith("round-github-expired-tokens-refreshed"));

    resolveOldDecision?.("approve");
    await Promise.resolve();

    expect(screen.getByRole("button", { name: "Publish review" })).toBeDisabled();
  });

  it("keeps a cancelled prompt cancelled when its already-started poll resolves late", async () => {
    type PollResult = Awaited<ReturnType<(typeof import("../src/api.fixture"))["pollCopilotPrompt"]>>;
    let resolvePoll: ((result: PollResult) => void) | undefined;
    const poll = vi.fn(() => new Promise<PollResult>((resolve) => { resolvePoll = resolve; }));

    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      return { ...api, pollCopilotPrompt: poll };
    });

    await openPaginationReview();
    await openChat();
    fireEvent.click(screen.getByRole("button", { name: /retry as new prompt/i }));
    await screen.findByRole("button", { name: "Cancel" });
    await waitFor(() => expect(poll).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(await screen.findByText(/Copilot · cancelled/i)).toBeVisible();

    resolvePoll?.({
      turn: {
        id: "late-poll-result",
        conversation_id: "late-conversation",
        idempotency_key: "late-idempotency-key",
        prompt: "late prompt",
        anchor: null,
        option_values: {},
        state: "completed",
        created_at: new Date().toISOString(),
        completed_at: new Date().toISOString(),
        failure_reason: null,
        response_text: "A stale response must never render after cancellation.",
      },
      update: { state: "completed", prompt_id: "late-poll-result" },
    });

    await waitFor(() => {
      expect(screen.getByText(/Copilot · cancelled/i)).toBeVisible();
      expect(screen.queryByText("A stale response must never render after cancellation.")).not.toBeInTheDocument();
    });
  });

  it("renders a visible, actionable machine failure rather than dropping it", async () => {
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      return {
        ...api,
        fetchMachineIndex: vi.fn().mockRejectedValue({
          code: "machine_unreachable",
          message: "Fixture machine is unreachable.",
          data_safety: "Cached rounds remain unchanged.",
          next_step: "Reconnect the fixture machine and retry.",
        }),
      };
    });

    await renderFixtureApp();
    fireEvent.click(screen.getByRole("button", { name: /fixture build machine/i }));
    await screen.findByRole("heading", { name: "Fixture Build Machine" });
    fireEvent.click(screen.getByRole("button", { name: /refresh machine queue/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Fixture machine is unreachable.");
    expect(alert).toHaveTextContent("Reconnect the fixture machine and retry.");
  });

  it("gives a cached machine round the same queue actions and rematerializes its remote source", async () => {
    let materialize = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      materialize = vi.fn((...args: Parameters<typeof api.materializeMachineRound>) =>
        api.materializeMachineRound(...args));
      return { ...api, materializeMachineRound: materialize };
    });

    await renderFixtureApp();

    expect(screen.getByRole("option", { name: "Fixture Build Machine" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /fixture build machine/i }));
    await screen.findByRole("heading", { name: "Fixture Build Machine" });

    const card = screen.getByText("Refactor session cache eviction").closest("article");
    if (!card) throw new Error("The cached machine round card was not rendered.");
    expect(within(card).getByRole("button", { name: "Open review" })).toBeVisible();
    expect(within(card).getByRole("button", { name: /Move Refactor session cache eviction up/i })).toBeVisible();
    expect(within(card).getByRole("button", { name: /Move Refactor session cache eviction down/i })).toBeVisible();
    const moreActions = card.querySelector("summary[aria-label^='More actions']");
    if (!moreActions) throw new Error("The cached machine round did not expose its overflow actions.");
    fireEvent.click(moreActions);
    expect(within(card).getByRole("button", { name: "Edit brief / details" })).toBeVisible();
    expect(within(card).getByRole("button", { name: "Reproduce…" })).toBeVisible();
    expect(within(card).getByRole("button", { name: "Copy feedback prompt" })).toBeVisible();
    expect(within(card).getByRole("button", { name: "Complete" })).toBeVisible();
    expect(within(card).getByRole("button", { name: "Delete" })).toBeVisible();
    const refreshSource = within(card).getByRole("button", { name: "Refresh remote source" });

    fireEvent.click(refreshSource);
    await waitFor(() => expect(materialize).toHaveBeenCalledWith(
      "machine-fixture-buildbox",
      "item-cache-eviction",
    ));
    expect((await screen.findAllByRole("region", { name: "Code diff" })).length).toBeGreaterThan(0);
  });

  it("renders source actions and approval behavior from the persisted adapter contract, not queue collection", async () => {
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      const moveGithubRoundToLocalQueue = (round: Awaited<ReturnType<typeof api.getRound>>) =>
        round.brief.title === githubTitle ? { ...round, collection: "local" as const } : round;
      return {
        ...api,
        listRounds: async (...args: Parameters<typeof api.listRounds>) =>
          (await api.listRounds(...args)).map(moveGithubRoundToLocalQueue),
        getRound: async (...args: Parameters<typeof api.getRound>) =>
          moveGithubRoundToLocalQueue(await api.getRound(...args)),
      };
    });

    await openReview(githubTitle);
    expect(screen.getByRole("group", { name: "GitHub review actions" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Refresh comments" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Check head" })).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Publish review" })).toBeEnabled());
    expect(screen.getByRole("button", { name: /Queue Home/ })).toBeVisible();
    expect(screen.queryByRole("alertdialog", { name: /Approve and purge/i })).not.toBeInTheDocument();
  });
});

describe("responsive reviewer escape hatches", () => {
  it("keeps a long generic dialog scrollable and keyboard-contained at the 560px minimum", async () => {
    setViewport(560, 720);
    await renderFixtureApp();
    const trigger = screen.getByRole("button", { name: "Submit local" });
    trigger.focus();
    fireEvent.click(trigger);

    const dialog = await screen.findByRole("dialog", { name: "Submit local review" });
    const backdrop = dialog.parentElement;
    if (!backdrop) throw new Error("The submit dialog backdrop was not rendered.");
    expect(backdrop).toHaveClass("modal-backdrop");
    const backdropRule = styles.match(/\.modal-backdrop\s*\{[^}]*\}/g)
      ?.find((rule) => rule.includes("overflow-y"));
    const modalRule = styles.match(/\.modal\s*\{[^}]*\}/)?.[0];
    expect(backdropRule).toContain("overflow-y: auto");
    expect(modalRule).toContain("max-height: calc(100dvh - 40px)");
    expect(modalRule).toContain("overflow-y: auto");

    const workspace = within(dialog).getByRole("textbox", { name: "Workspace path" });
    await waitFor(() => expect(document.activeElement).toBe(workspace));
    expect(within(dialog).getByRole("button", { name: "Preview repositories" })).toBeVisible();
    expect(within(dialog).getByRole("button", { name: "Capture snapshot" })).toBeInTheDocument();

    const close = within(dialog).getByRole("button", { name: "Close" });
    const cancel = within(dialog).getByRole("button", { name: "Cancel" });
    cancel.focus();
    fireEvent.keyDown(cancel, { key: "Tab" });
    expect(document.activeElement).toBe(close);
    fireEvent.keyDown(close, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(cancel);
  });

  it("keeps narrow source-rail and GitHub reviewer actions named and discoverable", async () => {
    setViewport(560);
    await renderFixtureApp();

    expect(screen.getByRole("button", { name: /this Mac, 4 active/i })).toHaveAttribute("title", "this Mac; 4 active");
    expect(screen.getByRole("button", { name: /Fixture Build Machine, connected, 2 cached/i }))
      .toHaveAttribute("title", "Fixture Build Machine; connected; 2 cached");
    expect(screen.getByRole("button", { name: "Add machine" })).toHaveAttribute("title", "Add machine");

    const card = screen.getByText(githubTitle).closest("article");
    if (!card) throw new Error("The fixture GitHub review card was not rendered.");
    fireEvent.click(within(card).getByRole("button", { name: "Open review" }));
    await screen.findAllByRole("region", { name: "Code diff" });

    const actions = screen.getByRole("group", { name: "GitHub review actions" });
    expect(within(actions).getByRole("button", { name: "Refresh comments" })).toBeVisible();
    expect(within(actions).getByRole("button", { name: "Check head" })).toBeVisible();
    expect(within(actions).getByRole("button", { name: "Publish review" })).toBeVisible();
  });

  it("offers a Chat control when the persistent chat column is unavailable at 1024px", async () => {
    setViewport(1024);
    await openPaginationReview();

    const chat = screen.getByRole("button", { name: "Open chat" });
    expect(chat).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(chat);
    const chatToggle = screen.getAllByRole("button", { name: "Close chat" })
      .find((button) => button.getAttribute("aria-controls") === "round-chat");
    if (!chatToggle) throw new Error("The toolbar chat toggle did not remain available after opening.");
    expect(chatToggle).toHaveAttribute("aria-expanded", "true");
  });

  it("offers a Files control when the file list is unavailable at 560px", async () => {
    setViewport(560);
    await openPaginationReview();

    const files = screen.getByRole("button", { name: "Open files" });
    expect(files).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(files);
    expect(screen.getByRole("button", { name: "Close files" })).toHaveAttribute("aria-expanded", "true");
  });

  it.each([1024, 1280])(
    "removes collapsed Files from rendering and the accessibility tree at %ipx",
    async (width) => {
      setViewport(width);
      await openPaginationReview();

      const files = document.getElementById("review-files");
      if (!(files instanceof HTMLElement)) throw new Error("The reviewer Files pane was not rendered.");
      const filter = within(files).getByPlaceholderText("Filter paths");
      const close = screen.getByRole("button", { name: "Close files" });
      close.focus();
      fireEvent.click(close);

      expect(files).toHaveAttribute("hidden");
      expect(files).toHaveAttribute("aria-hidden", "true");
      expect(filter).not.toBeVisible();
      expect(document.activeElement).toBe(close);
      expect(close).toHaveAccessibleName("Open files");

      fireEvent.click(close);
      expect(files).not.toHaveAttribute("hidden");
      expect(files).toHaveAttribute("aria-hidden", "false");
      expect(filter).toBeVisible();
    },
  );
});

describe("diff anchor selection", () => {
  it("uses roving keyboard focus for Unified and Split diff tabs", async () => {
    await openPaginationReview();

    const unified = screen.getByRole("tab", { name: "unified" });
    const split = screen.getByRole("tab", { name: "split" });
    expect(unified).toHaveAttribute("aria-selected", "true");
    expect(unified).toHaveAttribute("tabindex", "0");
    expect(split).toHaveAttribute("tabindex", "-1");

    fireEvent.keyDown(unified, { key: "ArrowRight" });
    await waitFor(() => expect(split).toHaveAttribute("aria-selected", "true"));
    expect(split).toHaveAttribute("tabindex", "0");
    await waitFor(() => expect(document.activeElement).toBe(split));

    fireEvent.click(screen.getAllByRole("button", { name: "Full file" })[0]);
    await waitFor(() => expect(unified).toHaveAttribute("aria-selected", "true"));
    expect(split).toHaveAttribute("aria-selected", "false");
  });

  it("uses one keyboard tab stop per hunk and arrow-key navigation between lines", async () => {
    await openReview("Add retry backoff to sync worker");

    const hunk = document.querySelector<HTMLElement>(".diff-hunk");
    if (!hunk) throw new Error("The fixture diff hunk was not rendered.");
    const lines = within(hunk).getAllByRole("button", { name: /select .* line/i });
    expect(lines.filter((line) => line.getAttribute("tabindex") === "0")).toHaveLength(1);

    lines[0].focus();
    fireEvent.keyDown(lines[0], { key: "ArrowDown" });
    expect(document.activeElement).toBe(lines[1]);
    expect(lines[1]).toHaveAttribute("tabindex", "0");
    expect(lines[0]).toHaveAttribute("tabindex", "-1");
  });

  it("navigates hunks across the entire continuous multi-file review", async () => {
    await openPaginationReview();

    const navigation = screen.getByRole("navigation", { name: "Review hunk navigation" });
    const next = within(navigation).getByRole("button", { name: "Next hunk in review" });
    await waitFor(() => expect(next).toBeEnabled());
    const initiallySelected = document.querySelector<HTMLElement>(".continuous-diff-path[aria-current='true']");
    const firstFile = initiallySelected?.closest(".continuous-diff-file");
    if (!initiallySelected || !firstFile) throw new Error("The first changed file was not selected.");
    const firstPath = initiallySelected.textContent;
    const firstFileHunks = firstFile.querySelectorAll(".diff-hunk").length;
    expect(firstFileHunks).toBeGreaterThan(0);

    for (let index = 0; index < firstFileHunks; index += 1) {
      fireEvent.click(next);
    }

    await waitFor(() => {
      const selected = document.querySelector<HTMLElement>(".continuous-diff-path[aria-current='true']");
      expect(selected?.textContent).not.toBe(firstPath);
    });
    expect(document.querySelector(".active-hunk[id^='review-queue-hunk-']")).toBeInTheDocument();
  });

  it("creates a side-correct /ask anchor from a split-diff line", async () => {
    const sendPrompt = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      return {
        ...api,
        sendCopilotPrompt: (...args: Parameters<typeof api.sendCopilotPrompt>) => {
          sendPrompt(...args);
          return api.sendCopilotPrompt(...args);
        },
      };
    });

    await openReview("Add retry backoff to sync worker");
    fireEvent.click(screen.getByRole("tab", { name: /split/i }));

    const leftLine = screen.getAllByRole("button", { name: /select .* left line/i })[0];
    expect(leftLine).toBeDefined();
    fireEvent.click(leftLine);
    expect(leftLine).toHaveClass("selected-code-line");

    const hunk = leftLine.closest(".diff-hunk");
    if (!hunk) throw new Error("The selected split line was not inside a diff hunk.");
    fireEvent.click(within(hunk).getByRole("button", { name: "/ask" }));

    expect(await screen.findByRole("complementary", { name: "Round chat" })).toHaveAttribute(
      "aria-hidden",
      "false",
    );
    fireEvent.click(await screen.findByRole("button", { name: "Start Copilot" }));
    const input = await screen.findByRole("textbox", { name: "Ask a follow-up" });
    await waitFor(() => expect(input).toBeEnabled());
    fireEvent.change(input, { target: { value: "Why did this line change?" } });
    await waitFor(() => expect(screen.getByRole("button", { name: "Send" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expect(sendPrompt).toHaveBeenCalledTimes(1));
    expect(sendPrompt.mock.calls[0][3]).toMatchObject({
      side: "LEFT",
      workspace_relative_path: "notify-worker/src/worker/sync_client.py",
      start_line: expect.any(Number),
      end_line: expect.any(Number),
    });
  });

  it("discloses a stale Copilot option and only drops it after an explicit zero-prompt reset", async () => {
    let startSession = vi.fn();
    let sendPrompt = vi.fn();
    vi.doMock("../src/api.fixture.ts", async (importOriginal) => {
      const api = await importOriginal<typeof import("../src/api.fixture")>();
      startSession = vi.fn((...args: Parameters<typeof api.startCopilotSession>) =>
        api.startCopilotSession(...args));
      sendPrompt = vi.fn((...args: Parameters<typeof api.sendCopilotPrompt>) =>
        api.sendCopilotPrompt(...args));
      return {
        ...api,
        activeConversation: async (...args: Parameters<typeof api.activeConversation>) => {
          const conversation = await api.activeConversation(...args);
          return {
            ...conversation,
            options: [
              ...conversation.options.filter((option) => option.key !== "context_window"),
              {
                key: "context_window",
                label: "Context window",
                kind: "select" as const,
                values: ["managed_80"],
                selected: "managed_80",
                supported: true,
                unavailable_reason: null,
              },
            ],
          };
        },
        startCopilotSession: startSession,
        sendCopilotPrompt: sendPrompt,
      };
    });

    await openReview("Add retry backoff to sync worker");
    await openChat();

    const warning = await screen.findByRole("alert");
    expect(within(warning).getByText("Unavailable saved Copilot options")).toBeVisible();
    expect(within(warning).getByText("context_window=managed_80")).toBeVisible();
    expect(within(warning).getByText(/will not be sent/i)).toBeVisible();
    expect(sendPrompt).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", {
      name: "Reset unavailable options and start Copilot",
    }));

    await waitFor(() => expect(startSession).toHaveBeenCalledTimes(1));
    expect(startSession.mock.calls[0][2]).not.toHaveProperty("context_window");
    expect(sendPrompt).not.toHaveBeenCalled();
    await waitFor(() => {
      expect(screen.queryByRole("button", {
        name: "Reset unavailable options and start Copilot",
      })).not.toBeInTheDocument();
    });
  });
});
