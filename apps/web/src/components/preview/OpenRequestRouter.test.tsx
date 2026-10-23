// @vitest-environment happy-dom
import { RegistryContext } from "@effect/atom-react";
import { scopedThreadKey } from "@bibcode/client-runtime/environment";
import { EnvironmentId, ThreadId } from "@bibcode/contracts";
import { AsyncResult } from "effect/reactivity";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const environmentId = EnvironmentId.make("env-1");

const h = vi.hoisted(() => ({
  openLink: vi.fn(),
  enqueueOpenPrompt: vi.fn(),
  claim: vi.fn(),
  openPreview: vi.fn(),
  environments: [] as Array<{ environmentId: string }>,
  centerPanels: {} as Record<string, unknown>,
  threadTitles: {} as Record<string, string>,
}));
vi.mock("~/state/entities", () => ({
  readThreadShell: (ref: { threadId: string }) =>
    h.threadTitles[ref.threadId] === undefined ? null : { title: h.threadTitles[ref.threadId] },
}));
let browserLinkTarget = "app";
vi.mock("~/hooks/useSettings", () => ({
  getClientSettings: () => ({ browserLinkTarget }),
}));
vi.mock("~/centerPanelStore", () => ({
  useCenterPanelStore: { getState: () => ({ byThreadKey: h.centerPanels }) },
}));
vi.mock("~/browser/openLink", () => ({ openLink: h.openLink }));
vi.mock("~/browser/openPromptQueue", () => ({ enqueueOpenPrompt: h.enqueueOpenPrompt }));
vi.mock("~/state/environments", () => ({
  useEnvironments: () => ({ environments: h.environments }),
}));
vi.mock("~/state/preview", () => ({
  previewEnvironment: {
    events: (target: { environmentId: string }) => `events:${target.environmentId}`,
    claimOpenRequest: { label: "claim" },
    open: { label: "open" },
  },
}));
vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: (command: { label: string }) =>
    command.label === "claim" ? h.claim : h.openPreview,
}));

import { OpenRequestRouter } from "./OpenRequestRouter";

/** A registry whose atoms are plain keys; `emit` pushes a value to subscribers. */
function fakeRegistry(initial: unknown = AsyncResult.initial()) {
  const listeners = new Map<string, Set<(value: unknown) => void>>();
  return {
    subscribe(atom: string, f: (value: unknown) => void, options?: { immediate?: boolean }) {
      if (options?.immediate) f(initial);
      const set = listeners.get(atom) ?? new Set();
      set.add(f);
      listeners.set(atom, set);
      return () => set.delete(f);
    },
    emit(atom: string, value: unknown) {
      for (const f of listeners.get(atom) ?? []) f(value);
    },
  };
}

const routerOn = (threadId: string | null) =>
  ({
    state: {
      matches: [{ params: threadId ? { environmentId, threadId } : {} }],
    },
  }) as never;

const openRequested = (threadId: string, requestId = "req-1") =>
  AsyncResult.success({
    type: "openRequested",
    threadId,
    requestId,
    url: "http://localhost:5173/",
    createdAt: "2026-10-08T00:00:00.000Z",
  });

const roots: ReturnType<typeof createRoot>[] = [];
async function mount(registry: ReturnType<typeof fakeRegistry>, router: never) {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  roots.push(root);
  await act(async () =>
    root.render(
      <RegistryContext.Provider value={registry as never}>
        <OpenRequestRouter router={router} />
      </RegistryContext.Provider>,
    ),
  );
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.clearAllMocks();
  h.environments = [{ environmentId }];
  h.centerPanels = {};
  h.threadTitles = {};
  (window as { desktopBridge?: unknown }).desktopBridge = { preview: {} };
});

afterEach(async () => {
  for (const root of roots.splice(0)) await act(async () => root.unmount());
  delete (window as { desktopBridge?: unknown }).desktopBridge;
  document.body.replaceChildren();
});

const threadRef = { environmentId, threadId: ThreadId.make("thread-1") };

describe("OpenRequestRouter", () => {
  it("claims open requests and only the winner opens", async () => {
    h.claim
      .mockResolvedValueOnce(AsyncResult.success({ claimed: true }))
      .mockResolvedValueOnce(AsyncResult.success({ claimed: false }));
    const registry = fakeRegistry();
    await mount(registry, routerOn("thread-1"));
    await mount(registry, routerOn("thread-1"));

    await act(async () => registry.emit(`events:${environmentId}`, openRequested("thread-1")));

    expect(h.claim).toHaveBeenCalledTimes(2);
    expect(h.claim).toHaveBeenCalledWith({ environmentId, input: { requestId: "req-1" } });
    expect(h.openLink).toHaveBeenCalledTimes(1);
    expect(h.openLink).toHaveBeenCalledWith({
      url: "http://localhost:5173/",
      threadRef,
      invert: false,
      openPreview: h.openPreview,
    });
  });

  it("warns with the request id, never the address, when the claim fails", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    h.claim.mockResolvedValue(AsyncResult.failure(new Error("socket closed") as never));
    const registry = fakeRegistry();
    await mount(registry, routerOn("thread-1"));

    await act(async () => registry.emit(`events:${environmentId}`, openRequested("thread-1")));

    expect(warn).toHaveBeenCalledTimes(1);
    const logged = JSON.stringify(warn.mock.calls[0]);
    expect(logged).toContain("req-1");
    expect(logged).not.toContain("localhost:5173");
    expect(h.openLink).not.toHaveBeenCalled();
    warn.mockRestore();
  });

  it("asks about a request for a thread not on screen after giving others a chance", async () => {
    vi.useFakeTimers();
    try {
      h.claim.mockResolvedValue(AsyncResult.success({ claimed: true }));
      h.threadTitles = { "thread-1": "Fix login" };
      const registry = fakeRegistry();
      await mount(registry, routerOn("thread-2"));

      await act(async () => registry.emit(`events:${environmentId}`, openRequested("thread-1")));
      expect(h.claim).not.toHaveBeenCalled();

      await act(async () => vi.advanceTimersByTime(2_000));
      expect(h.claim).toHaveBeenCalledTimes(1);
      // Opening from a thread the user isn't looking at needs their click, in every mode.
      expect(h.openLink).not.toHaveBeenCalled();
      expect(h.enqueueOpenPrompt).toHaveBeenCalledWith({
        id: "req-1",
        source: "command",
        url: "http://localhost:5173/",
        threadRef,
        threadTitle: "“Fix login”",
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("never claims an off-screen request from a hidden client", async () => {
    vi.useFakeTimers();
    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    try {
      const registry = fakeRegistry();
      await mount(registry, routerOn("thread-2"));

      await act(async () => registry.emit(`events:${environmentId}`, openRequested("thread-1")));
      await act(async () => vi.advanceTimersByTime(2_000));

      expect(h.claim).not.toHaveBeenCalled();
    } finally {
      visibility.mockRestore();
      vi.useRealTimers();
    }
  });

  it("does nothing with a claim that settles after it unmounted", async () => {
    let finish!: (value: unknown) => void;
    h.claim.mockReturnValue(new Promise((resolve) => (finish = resolve)));
    const registry = fakeRegistry();
    await mount(registry, routerOn("thread-1"));

    await act(async () => registry.emit(`events:${environmentId}`, openRequested("thread-1")));
    for (const root of roots.splice(0)) await act(async () => root.unmount());
    await act(async () => finish(AsyncResult.success({ claimed: true })));

    expect(h.openLink).not.toHaveBeenCalled();
  });

  it("remembers a bounded number of handled requests", async () => {
    h.claim.mockResolvedValue(AsyncResult.success({ claimed: false }));
    const registry = fakeRegistry();
    await mount(registry, routerOn("thread-1"));

    await act(async () => {
      for (let i = 0; i <= 256; i += 1) {
        registry.emit(`events:${environmentId}`, openRequested("thread-1", `r${i}`));
      }
      // The newest is still remembered; the oldest has been forgotten.
      registry.emit(`events:${environmentId}`, openRequested("thread-1", "r256"));
      registry.emit(`events:${environmentId}`, openRequested("thread-1", "r0"));
    });

    expect(h.claim).toHaveBeenCalledTimes(258);
  });

  it("routes requests from a chat panel shown beside the routed thread", async () => {
    h.claim.mockResolvedValue(AsyncResult.success({ claimed: true }));
    h.centerPanels = {
      [scopedThreadKey({ environmentId, threadId: ThreadId.make("host-thread") })]: {
        groups: [
          {
            id: "root",
            surfaceIds: ["chat-host", "chat:thread-1"],
            activeSurfaceId: "chat:thread-1",
          },
        ],
      },
    };
    const registry = fakeRegistry();
    await mount(registry, routerOn("host-thread"));

    await act(async () => registry.emit(`events:${environmentId}`, openRequested("thread-1")));

    // The panel's thread has no browser panel on screen, so it opens in the system browser.
    expect(h.openLink).toHaveBeenCalledWith(expect.objectContaining({ threadRef, invert: true }));
  });

  it("asks instead of opening when the user leaves the thread during the claim", async () => {
    let finish!: (value: unknown) => void;
    h.claim.mockReturnValue(new Promise((resolve) => (finish = resolve)));
    h.threadTitles = { "thread-1": "Fix login" };
    const registry = fakeRegistry();
    const router = { state: { matches: [{ params: { environmentId, threadId: "thread-1" } }] } };
    await mount(registry, router as never);

    await act(async () => registry.emit(`events:${environmentId}`, openRequested("thread-1")));
    router.state.matches = [{ params: { environmentId, threadId: "thread-2" } }];
    await act(async () => finish(AsyncResult.success({ claimed: true })));

    expect(h.openLink).not.toHaveBeenCalled();
    expect(h.enqueueOpenPrompt).toHaveBeenCalledWith(
      expect.objectContaining({ id: "req-1", threadRef, threadTitle: "“Fix login”" }),
    );
  });

  it("opens without asking when the user arrives at the thread during the claim", async () => {
    vi.useFakeTimers();
    try {
      let finish!: (value: unknown) => void;
      h.claim.mockReturnValue(new Promise((resolve) => (finish = resolve)));
      const registry = fakeRegistry();
      const router = { state: { matches: [{ params: { environmentId, threadId: "thread-2" } }] } };
      await mount(registry, router as never);

      await act(async () => registry.emit(`events:${environmentId}`, openRequested("thread-1")));
      await act(async () => vi.advanceTimersByTime(2_000));
      router.state.matches = [{ params: { environmentId, threadId: "thread-1" } }];
      await act(async () => finish(AsyncResult.success({ claimed: true })));

      expect(h.enqueueOpenPrompt).not.toHaveBeenCalled();
      expect(h.openLink).toHaveBeenCalledWith(
        expect.objectContaining({ threadRef, invert: false }),
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("asks before opening in browser mode, where no click backs a new tab", async () => {
    delete (window as { desktopBridge?: unknown }).desktopBridge;
    h.claim.mockResolvedValue(AsyncResult.success({ claimed: true }));
    const registry = fakeRegistry();
    await mount(registry, routerOn("thread-1"));

    await act(async () => registry.emit(`events:${environmentId}`, openRequested("thread-1")));

    expect(h.enqueueOpenPrompt).toHaveBeenCalledWith({
      id: "req-1",
      source: "command",
      url: "http://localhost:5173/",
      threadRef,
    });
    expect(h.openLink).not.toHaveBeenCalled();
  });

  it("lets a visible client win: a hidden one claims only after a delay", async () => {
    vi.useFakeTimers();
    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    try {
      h.claim.mockResolvedValue(AsyncResult.success({ claimed: true }));
      const registry = fakeRegistry();
      await mount(registry, routerOn("thread-1"));

      await act(async () => registry.emit(`events:${environmentId}`, openRequested("thread-1")));
      expect(h.claim).not.toHaveBeenCalled();

      await act(async () => vi.advanceTimersByTime(2_000));
      expect(h.claim).toHaveBeenCalledTimes(1);

      // A thread left before the delay ends is not claimed.
      const left = fakeRegistry();
      const router = { state: { matches: [{ params: { environmentId, threadId: "thread-1" } }] } };
      await mount(left, router as never);
      await act(async () => left.emit(`events:${environmentId}`, openRequested("thread-1", "r2")));
      router.state.matches = [{ params: { environmentId, threadId: "thread-2" } }];
      await act(async () => vi.advanceTimersByTime(2_000));
      expect(h.claim).toHaveBeenCalledTimes(1);
    } finally {
      visibility.mockRestore();
      vi.useRealTimers();
    }
  });

  it("does not replay the last event it finds when it subscribes, nor claim twice", async () => {
    h.claim.mockResolvedValue(AsyncResult.success({ claimed: true }));
    const registry = fakeRegistry(openRequested("thread-1", "stale"));
    await mount(registry, routerOn("thread-1"));
    expect(h.claim).not.toHaveBeenCalled();

    await act(async () => {
      registry.emit(`events:${environmentId}`, openRequested("thread-1", "fresh"));
      registry.emit(`events:${environmentId}`, openRequested("thread-1", "fresh"));
    });
    expect(h.claim).toHaveBeenCalledTimes(1);
    expect(h.openLink).toHaveBeenCalledTimes(1);
  });
});
