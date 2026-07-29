import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const paginationTitle = "Fix pagination cursor drift across core-api and web-frontend";

function setViewport(width: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, value: width });
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
  it("lets an interrupted durable /ask turn start a fresh session and retry only on explicit click", async () => {
    await openPaginationReview();
    await openChat();

    expect(await screen.findByText(/Review Queue restarted before Copilot finished responding/i)).toBeVisible();
    const retry = screen.getByRole("button", { name: /retry as new prompt/i });
    expect(retry).toBeVisible();

    fireEvent.click(retry);
    expect(await screen.findByRole("button", { name: /cancel/i })).toBeVisible();
    expect(screen.getByRole("combobox", { name: /previous chats/i })).toBeVisible();
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

  it("keeps a cached machine round visible and openable from its machine queue", async () => {
    await renderFixtureApp();

    expect(screen.getByRole("option", { name: "Fixture Build Machine" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /fixture build machine/i }));
    await screen.findByRole("heading", { name: "Fixture Build Machine" });

    fireEvent.click(screen.getByRole("button", { name: "Open cached review" }));
    expect((await screen.findAllByRole("region", { name: "Code diff" })).length).toBeGreaterThan(0);
  });
});

describe("responsive reviewer escape hatches", () => {
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
});

describe("diff anchor selection", () => {
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
    fireEvent.click(screen.getByRole("button", { name: /split/i }));

    const leftLine = screen.getAllByRole("button", { name: /select .* left line/i })[0];
    expect(leftLine).toBeDefined();
    fireEvent.click(leftLine);
    expect(leftLine).toHaveClass("selected-code-line");

    const hunk = leftLine.closest(".diff-hunk");
    if (!hunk) throw new Error("The selected split line was not inside a diff hunk.");
    fireEvent.click(within(hunk).getByRole("button", { name: "/ask" }));

    await openChat();
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
});
