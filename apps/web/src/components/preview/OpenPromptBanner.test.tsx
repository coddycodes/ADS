// @vitest-environment happy-dom
import { EnvironmentId, ThreadId } from "@bibcode/contracts";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  openLink: vi.fn(),
  openPreview: vi.fn(),
  navigate: vi.fn(),
}));
vi.mock("~/browser/openLink", () => ({ openLink: h.openLink }));
vi.mock("~/state/use-atom-command", () => ({ useAtomCommand: () => h.openPreview }));
vi.mock("~/state/preview", () => ({ previewEnvironment: { open: {} } }));
vi.mock("~/state/session", () => ({ readPreparedConnection: () => ({ label: "Build box" }) }));
vi.mock("~/hooks/useSettings", () => ({
  getClientSettings: () => ({ browserLinkTarget: "app" }),
}));

import { enqueueOpenPrompt, resetOpenPromptsForTests } from "~/browser/openPromptQueue";

import { OpenPromptBanner } from "./OpenPromptBanner";

const threadRef = {
  environmentId: EnvironmentId.make("env-1"),
  threadId: ThreadId.make("thread-1"),
};

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

beforeEach(async () => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  resetOpenPromptsForTests();
  vi.clearAllMocks();
  h.openLink.mockReturnValue("system");
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () =>
    root.render(<OpenPromptBanner router={{ navigate: h.navigate } as never} />),
  );
});

afterEach(async () => {
  await act(async () => root.unmount());
  document.body.replaceChildren();
});

const button = (name: string) =>
  Array.from(container.querySelectorAll("button")).find((b) => b.textContent === name)!;

describe("OpenPromptBanner", () => {
  it("names who asked and the full address", async () => {
    expect(container.textContent).toBe("");
    await act(async () =>
      enqueueOpenPrompt({ id: "r1", source: "command", url: "http://localhost:5173/", threadRef }),
    );
    expect(container.textContent).toContain(
      "A command wants to open http://localhost:5173/ on Build box",
    );
    const region = container.querySelector('[role="region"]');
    expect(region?.getAttribute("aria-label")).toBe("Requests to open links");
    expect(region?.querySelector('[aria-live="polite"]')?.textContent).toContain(
      "A command wants to open",
    );
    // The app's top banner stack positions it, so it never covers the reload prompt.
    expect(region?.className).not.toContain("fixed");

    await act(async () => button("Dismiss").click());
    await act(async () =>
      enqueueOpenPrompt({ source: "agent", url: "http://localhost:3000/", threadRef }),
    );
    expect(container.textContent).toContain(
      "Agent wants to open http://localhost:3000/ on Build box",
    );
  });

  it("opens through openLink from the click and removes the prompt", async () => {
    await act(async () =>
      enqueueOpenPrompt({ id: "r1", source: "command", url: "http://localhost:5173/", threadRef }),
    );
    await act(async () => button("Open").click());

    expect(h.openLink).toHaveBeenCalledWith({
      url: "http://localhost:5173/",
      threadRef,
      invert: false,
      openPreview: h.openPreview,
      onPopupBlocked: expect.any(Function),
      onUnopened: expect.any(Function),
      onError: expect.any(Function),
    });
    expect(container.textContent).toBe("");
  });

  it("opens a blocked new tab in a new tab again, not in the BiBCode browser", async () => {
    await act(async () =>
      enqueueOpenPrompt({
        source: "link",
        blocked: true,
        url: "http://localhost:5173/",
        threadRef,
      }),
    );
    await act(async () => button("Open").click());

    expect(h.openLink).toHaveBeenCalledWith(expect.objectContaining({ invert: true }));
  });

  it("keeps the prompt when opening is refused, so it can be retried", async () => {
    h.openLink.mockReturnValueOnce("unreachable");
    await act(async () =>
      enqueueOpenPrompt({ id: "r1", source: "command", url: "http://localhost:5173/", threadRef }),
    );
    await act(async () => button("Open").click());

    expect(container.textContent).toContain("A command wants to open http://localhost:5173/");
  });

  it("offers the request again when the gateway refuses after the tab opened", async () => {
    let unopened!: () => void;
    h.openLink.mockImplementationOnce((input: { onUnopened: () => void }) => {
      unopened = input.onUnopened;
      return "system";
    });
    await act(async () =>
      enqueueOpenPrompt({ id: "r1", source: "command", url: "http://localhost:5173/", threadRef }),
    );
    await act(async () => button("Open").click());
    expect(container.textContent).toBe("");

    await act(async () => unopened());
    expect(container.textContent).toContain("A command wants to open http://localhost:5173/");
    expect(container.textContent).not.toContain("blocked");
  });

  it("keeps a prompt whose tab the browser blocked, so Open can be retried", async () => {
    h.openLink.mockImplementationOnce((input: { onPopupBlocked: (url: string) => void }) =>
      input.onPopupBlocked("http://localhost:5173/"),
    );
    await act(async () =>
      enqueueOpenPrompt({ id: "r1", source: "command", url: "http://localhost:5173/", threadRef }),
    );
    await act(async () => button("Open").click());

    expect(container.textContent).toContain(
      "Your browser blocked a new tab for http://localhost:5173/ on Build box",
    );
    // The retry opens the tab, but the gateway then fails in a retryable way.
    let unopened!: () => void;
    h.openLink.mockImplementationOnce((input: { onUnopened: () => void }) => {
      unopened = input.onUnopened;
      return "system";
    });
    await act(async () => button("Open").click());
    expect(h.openLink).toHaveBeenCalledTimes(2);
    expect(container.textContent).toBe("");
    await act(async () => unopened());
    // Offered again as the command's request, not as a blocked tab.
    expect(container.textContent).toContain("A command wants to open http://localhost:5173/");
    expect(container.textContent).not.toContain("blocked");
  });

  it("does not bring a dismissed prompt back and shows the next one", async () => {
    await act(async () => {
      enqueueOpenPrompt({ id: "r1", source: "command", url: "http://localhost:5173/", threadRef });
      enqueueOpenPrompt({ id: "r1", source: "command", url: "http://localhost:5173/", threadRef });
      enqueueOpenPrompt({ id: "r2", source: "agent", url: "http://localhost:3000/", threadRef });
    });
    expect(container.textContent).toContain("(1 more waiting)");
    await act(async () => button("Dismiss").click());
    expect(h.openLink).not.toHaveBeenCalled();
    expect(container.textContent).not.toContain("more waiting");
    expect(container.textContent).toContain("http://localhost:3000/");

    await act(async () => button("Dismiss").click());
    expect(container.textContent).toBe("");
  });

  it("names a thread that isn't on screen and shows it before opening", async () => {
    const order: string[] = [];
    h.navigate.mockImplementation(() => {
      order.push("navigate");
      return Promise.resolve();
    });
    h.openLink.mockImplementation(() => {
      order.push("open");
      return "app";
    });
    await act(async () =>
      enqueueOpenPrompt({
        id: "r1",
        source: "command",
        url: "http://localhost:5173/",
        threadRef,
        threadTitle: "“Fix login”",
      }),
    );
    expect(container.textContent).toContain(
      "A command in “Fix login” wants to open http://localhost:5173/ on Build box",
    );

    await act(async () => button("Show thread and open").click());
    expect(order).toEqual(["navigate", "open"]);
    expect(h.navigate).toHaveBeenCalledWith({
      to: "/$environmentId/$threadId",
      params: { environmentId: threadRef.environmentId, threadId: threadRef.threadId },
    });
    expect(h.openLink).toHaveBeenCalledWith(expect.objectContaining({ threadRef }));
  });

  it("offers the request again when the internal browser fails to open it", async () => {
    let failed!: () => void;
    h.openLink.mockImplementationOnce((input: { onError: () => void }) => {
      failed = input.onError;
      return "app";
    });
    await act(async () =>
      enqueueOpenPrompt({ id: "r1", source: "command", url: "http://localhost:5173/", threadRef }),
    );
    await act(async () => button("Open").click());
    expect(container.textContent).toBe("");

    await act(async () => failed());
    expect(container.textContent).toContain("A command wants to open http://localhost:5173/");
  });

  it("says how to stop a blocked tab and offers the link to copy", async () => {
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    h.openLink.mockImplementationOnce((input: { onPopupBlocked: (url: string) => void }) =>
      input.onPopupBlocked("http://localhost:5173/"),
    );
    await act(async () =>
      enqueueOpenPrompt({ id: "r1", source: "command", url: "http://localhost:5173/", threadRef }),
    );
    expect(container.textContent).not.toContain("Allow pop-ups");
    await act(async () => button("Open").click());

    expect(container.textContent).toContain("Allow pop-ups for this site to open links directly.");
    await act(async () => button("Copy link").click());
    expect(writeText).toHaveBeenCalledWith("http://localhost:5173/");
    // Copying doesn't answer the request.
    expect(container.textContent).toContain("http://localhost:5173/");
  });
});
