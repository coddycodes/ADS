/**
 * Behaviour tests for PreviewNewWindowRouter.
 *
 * The component renders `null` and does its work in a mount effect. Following
 * the PreviewAutomationHosts pattern, `vi.mock("react")` captures effects so
 * they can be run manually after a static render; the desktop bridge listener
 * is captured from the `previewBridge` module handle's `onNewWindowRequest`.
 */
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { EnvironmentId, ThreadId } from "@coddy/contracts";

const h = vi.hoisted(() => ({
  effects: [] as Array<() => void | (() => void)>,
  cleanups: [] as Array<() => void>,
  listener: null as ((tabId: string, url: string) => void) | null,
  unsubscribed: 0,
  threadRef: null as unknown,
  resolution: { kind: "reachable", url: "" } as Record<string, unknown>,
  resolveCalls: [] as Array<[unknown, string]>,
  openCalls: [] as Array<Record<string, unknown>>,
  noticeCalls: [] as unknown[],
  openPreview: () => Promise.resolve(),
  previewBridge: null as unknown,
}));

vi.mock("./previewBridge", () => ({
  get previewBridge() {
    return h.previewBridge;
  },
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  const useEffect = (effect: () => void | (() => void)) => {
    h.effects.push(effect);
  };
  return { ...actual, useEffect: useEffect as typeof actual.useEffect };
});

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: () => h.openPreview,
}));

vi.mock("~/state/preview", () => ({
  previewEnvironment: { open: { label: "preview.open" } },
}));

vi.mock("~/previewStateStore", () => ({
  findPreviewThreadForTab: () => h.threadRef,
}));

vi.mock("~/browser/browserTargetResolver", () => ({
  resolvePreviewTarget: (environmentId: unknown, url: string) => {
    h.resolveCalls.push([environmentId, url]);
    return h.resolution.kind === "reachable" ? { kind: "reachable", url } : h.resolution;
  },
}));

vi.mock("~/browser/previewGateway", () => ({
  canonicalizePreviewUrl: (url: string) =>
    url.replace("http://127.0.0.1:50000", "http://localhost:5173"),
}));

vi.mock("~/browser/linkNotices", () => ({
  showPreviewUnreachableNotice: (resolution: unknown) => {
    h.noticeCalls.push(resolution);
  },
}));

vi.mock("~/browser/openFileInPreview", () => ({
  openUrlInPreview: (input: Record<string, unknown>) => {
    h.openCalls.push(input);
    return Promise.resolve();
  },
}));

import { PreviewNewWindowRouter } from "./PreviewNewWindowRouter";

const environmentId = EnvironmentId.make("environment-local");
const threadRef = { environmentId, threadId: ThreadId.make("thread-1") };

function mountRouter(): (tabId: string, url: string) => void {
  renderToStaticMarkup(<PreviewNewWindowRouter />);
  for (const effect of h.effects.splice(0)) {
    const cleanup = effect();
    if (typeof cleanup === "function") h.cleanups.push(cleanup);
  }
  expect(h.listener, "expected onNewWindowRequest to be subscribed").not.toBeNull();
  return h.listener!;
}

beforeEach(() => {
  h.effects.length = 0;
  h.listener = null;
  h.unsubscribed = 0;
  h.threadRef = threadRef;
  h.resolution = { kind: "reachable", url: "" };
  h.resolveCalls.length = 0;
  h.openCalls.length = 0;
  h.noticeCalls.length = 0;
  h.previewBridge = {
    onNewWindowRequest: (listener: (tabId: string, url: string) => void) => {
      h.listener = listener;
      return () => {
        h.unsubscribed += 1;
      };
    },
  };
  // The module handle is the only bridge source; the window is never read.
  vi.stubGlobal("window", {});
});

afterEach(() => {
  for (const cleanup of h.cleanups.splice(0)) cleanup();
  vi.unstubAllGlobals();
});

describe("PreviewNewWindowRouter", () => {
  it("opens popups from a preview tab as a new tab in the same thread", () => {
    const listener = mountRouter();

    listener("tab_1", "https://x.test/");

    expect(h.resolveCalls).toEqual([[environmentId, "https://x.test/"]]);
    expect(h.openCalls).toEqual([
      { threadRef, url: "https://x.test/", openPreview: h.openPreview },
    ]);
    expect(h.noticeCalls).toEqual([]);
  });

  it("ignores new-window requests for unknown tabs", () => {
    h.threadRef = null;
    const listener = mountRouter();

    expect(() => listener("tab_unknown", "https://x.test/")).not.toThrow();
    expect(h.openCalls).toEqual([]);
    expect(h.noticeCalls).toEqual([]);
  });

  it("shows the unreachable notice for server-loopback popups on SSH threads", () => {
    h.resolution = { kind: "unreachable", reason: "ssh", environmentLabel: "Box" };
    const listener = mountRouter();

    listener("tab_1", "http://localhost:3000/");

    expect(h.noticeCalls).toEqual([
      { kind: "unreachable", reason: "ssh", environmentLabel: "Box" },
    ]);
    expect(h.openCalls).toEqual([]);
  });

  it("opens a gateway page's popup at its canonical URL", () => {
    h.resolution = { kind: "gateway", via: "ssh", url: "http://localhost:5173/next" };
    const listener = mountRouter();

    listener("tab_1", "http://127.0.0.1:50000/next");

    expect(h.resolveCalls).toEqual([[environmentId, "http://localhost:5173/next"]]);
    expect(h.openCalls).toEqual([
      { threadRef, url: "http://localhost:5173/next", openPreview: h.openPreview },
    ]);
  });

  it("subscribes to nothing without a preview bridge", () => {
    h.previewBridge = null;
    renderToStaticMarkup(<PreviewNewWindowRouter />);
    for (const effect of h.effects.splice(0)) expect(effect()).toBeUndefined();
    expect(h.listener).toBeNull();
  });

  it("unsubscribes from the desktop bridge on unmount", () => {
    mountRouter();
    for (const cleanup of h.cleanups.splice(0)) cleanup();
    expect(h.unsubscribed).toBe(1);
  });
});
