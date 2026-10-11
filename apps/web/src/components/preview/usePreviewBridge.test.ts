// @vitest-environment happy-dom

import type {
  DesktopPreviewNavStatus,
  DesktopPreviewTabState,
  ScopedThreadRef,
} from "@bibcode/contracts";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

type EffectCallback = () => void | (() => void);

const hooks = vi.hoisted(() => {
  let cursor = 0;
  let refSlots = new Map<number, { current: unknown }>();

  return {
    effects: [] as EffectCallback[],
    beginRender(): void {
      cursor = 0;
      this.effects = [];
    },
    reset(): void {
      cursor = 0;
      refSlots = new Map();
      this.effects = [];
    },
    useEffect(effect: EffectCallback): void {
      cursor += 1;
      hooks.effects.push(effect);
    },
    useRef<T>(initialValue: T): { current: T } {
      const index = cursor;
      cursor += 1;
      const existing = refSlots.get(index);
      if (existing !== undefined) {
        return existing as { current: T };
      }
      const ref = { current: initialValue };
      refSlots.set(index, ref);
      return ref;
    },
  };
});

const testState = vi.hoisted(() => ({
  appliedStates: [] as Array<{ threadRef: unknown; tabId: string; state: unknown }>,
  browserMode: false,
  bridge: null as null | {
    onStateChange: (listener: (tabId: string, state: DesktopPreviewTabState) => void) => () => void;
  },
  clearCalls: [] as string[],
  listener: null as null | ((tabId: string, state: DesktopPreviewTabState) => void),
  localFailureCalls: [] as unknown[],
  reportCalls: [] as unknown[],
  unsubscribe: vi.fn(),
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useEffect: hooks.useEffect,
    useRef: hooks.useRef,
  };
});

vi.mock("~/browser/browserPointerStore", () => ({
  useBrowserPointerStore: (selector: (state: { clear: (tabId: string) => void }) => unknown) =>
    selector({
      clear: (tabId: string) => {
        testState.clearCalls.push(tabId);
      },
    }),
}));

vi.mock("~/previewStateStore", () => ({
  applyPreviewDesktopState: (threadRef: unknown, tabId: string, state: unknown) => {
    testState.appliedStates.push({ threadRef, tabId, state });
  },
  setPreviewLocalFailure: (...args: unknown[]) => testState.localFailureCalls.push(args),
}));

vi.mock("~/browser/previewGateway", () => ({
  canonicalizePreviewUrl: (url: string) =>
    url.replace(
      "http://10.0.0.2:41000/__bibcode/bootstrap?cap=C&to=%2Fa",
      "http://localhost:5173/a",
    ),
  isGatewayClientUrl: (url: string) => url.startsWith("http://10.0.0.2:41000/"),
}));

vi.mock("~/state/preview", () => ({
  previewEnvironment: { reportStatus: { key: "preview.reportStatus" } },
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: () => (input: unknown) => {
    testState.reportCalls.push(input);
    return Promise.resolve(undefined);
  },
}));

vi.mock("./previewBridge", () => ({
  get previewBridge() {
    return testState.bridge;
  },
  isBrowserMode: () => testState.browserMode,
}));

import { usePreviewBridge } from "./usePreviewBridge";

const threadRef = {
  environmentId: "environment-1",
  threadId: "thread-1",
} as ScopedThreadRef;

function state(navStatus: DesktopPreviewNavStatus): DesktopPreviewTabState {
  return {
    tabId: "tab-1",
    webContentsId: 42,
    navStatus,
    canGoBack: true,
    canGoForward: false,
    zoomFactor: 1.25,
    controller: "human",
    updatedAt: "2026-07-16T00:00:00.000Z",
  };
}

function renderHook(input: { threadRef?: ScopedThreadRef; tabId?: string } = {}): void {
  hooks.beginRender();
  const props = {
    threadRef: input.threadRef ?? threadRef,
    tabId: input.tabId ?? "tab-1",
  };
  function Harness(): null {
    usePreviewBridge(props);
    return null;
  }
  renderToStaticMarkup(createElement(Harness));
}

function runEffect(): void | (() => void) {
  expect(hooks.effects).toHaveLength(1);
  return hooks.effects[0]!();
}

function emit(tabId: string, navStatus: DesktopPreviewNavStatus): void {
  if (testState.listener === null) {
    throw new Error("Preview bridge listener was not installed.");
  }
  testState.listener(tabId, state(navStatus));
}

beforeEach(() => {
  hooks.reset();
  testState.appliedStates = [];
  testState.browserMode = false;
  testState.bridge = {
    onStateChange: (listener) => {
      testState.listener = listener;
      return testState.unsubscribe;
    },
  };
  testState.clearCalls = [];
  testState.listener = null;
  testState.localFailureCalls = [];
  testState.reportCalls = [];
  testState.unsubscribe.mockReset();
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("usePreviewBridge", () => {
  it("does not subscribe without a desktop bridge", () => {
    testState.bridge = null;

    renderHook();

    expect(runEffect()).toBeUndefined();
    expect(testState.listener).toBeNull();
  });

  it("does not subscribe outside a browser runtime", () => {
    vi.stubGlobal("window", undefined);

    renderHook();

    expect(runEffect()).toBeUndefined();
    expect(testState.listener).toBeNull();
  });

  it("ignores other tabs and returns the bridge cleanup", () => {
    renderHook();
    const cleanup = runEffect();

    emit("tab-2", { kind: "Loading", url: "https://example.test", title: "Example" });

    expect(cleanup).toBe(testState.unsubscribe);
    expect(testState.appliedStates).toEqual([]);
    expect(testState.reportCalls).toEqual([]);
  });

  it("projects desktop state and never reports Idle", () => {
    renderHook();
    runEffect();

    emit("tab-1", { kind: "Idle" });

    expect(testState.appliedStates).toEqual([
      {
        threadRef,
        tabId: "tab-1",
        state: {
          url: null,
          title: "",
          canGoBack: true,
          canGoForward: false,
          loading: false,
          zoomFactor: 1.25,
          controller: "human",
        },
      },
    ]);
    expect(testState.reportCalls).toEqual([]);
  });

  it("reports Loading and Success once per kind and URL", () => {
    renderHook();
    runEffect();
    const loading = { kind: "Loading", url: "https://example.test", title: "Loading" } as const;
    const success = { kind: "Success", url: "https://example.test", title: "Ready" } as const;

    emit("tab-1", loading);
    emit("tab-1", loading);
    emit("tab-1", success);
    emit("tab-1", success);

    expect(testState.reportCalls).toEqual([
      {
        environmentId: "environment-1",
        input: {
          threadId: "thread-1",
          tabId: "tab-1",
          canGoBack: true,
          canGoForward: false,
          navStatus: {
            _tag: "Loading",
            url: "https://example.test",
            title: "Loading",
          },
        },
      },
      {
        environmentId: "environment-1",
        input: {
          threadId: "thread-1",
          tabId: "tab-1",
          canGoBack: true,
          canGoForward: false,
          navStatus: {
            _tag: "Success",
            url: "https://example.test",
            title: "Ready",
          },
        },
      },
    ]);
  });

  it("projects and reports the canonical URL of a gateway page", () => {
    renderHook();
    runEffect();

    emit("tab-1", {
      kind: "Loading",
      url: "http://10.0.0.2:41000/__bibcode/bootstrap?cap=C&to=%2Fa",
      title: "",
    });
    emit("tab-1", { kind: "Loading", url: "http://localhost:5173/a", title: "" });

    expect(testState.appliedStates.map(({ state }) => (state as { url: unknown }).url)).toEqual([
      "http://localhost:5173/a",
      "http://localhost:5173/a",
    ]);
    // The bootstrap hop and the page it replaces are one canonical load.
    expect(testState.reportCalls).toMatchObject([
      { input: { navStatus: { _tag: "Loading", url: "http://localhost:5173/a" } } },
    ]);
  });

  it("always reports LoadFailed with failure details", () => {
    renderHook();
    runEffect();
    const failed = {
      kind: "LoadFailed",
      url: "https://example.test/failure",
      title: "Failed",
      code: -105,
      description: "Name not resolved",
    } as const;

    emit("tab-1", failed);
    emit("tab-1", failed);

    expect(testState.reportCalls).toHaveLength(2);
    expect(testState.reportCalls[0]).toEqual({
      environmentId: "environment-1",
      input: {
        threadId: "thread-1",
        tabId: "tab-1",
        canGoBack: true,
        canGoForward: false,
        navStatus: {
          _tag: "LoadFailed",
          url: "https://example.test/failure",
          title: "Failed",
          code: -105,
          description: "Name not resolved",
        },
      },
    });
  });

  it("keeps a failed load on this client's gateway origin local until the next navigation", () => {
    renderHook();
    runEffect();

    emit("tab-1", { kind: "Success", url: "http://10.0.0.2:41000/old", title: "Old" });
    testState.localFailureCalls = [];
    emit("tab-1", {
      kind: "LoadFailed",
      url: "http://10.0.0.2:41000/__bibcode/bootstrap?cap=C&to=%2Fa",
      title: "",
      code: -102,
      description: "ERR_CONNECTION_REFUSED",
    });

    // Another client may reach the gateway fine: nothing goes to shared state.
    expect(testState.reportCalls).toHaveLength(1);
    expect(testState.localFailureCalls).toEqual([
      [
        threadRef,
        "tab-1",
        { url: "http://localhost:5173/a", code: -102, description: "ERR_CONNECTION_REFUSED" },
      ],
    ]);

    emit("tab-1", { kind: "Loading", url: "http://10.0.0.2:41000/x", title: "" });
    expect(testState.localFailureCalls.at(-1)).toEqual([threadRef, "tab-1", null]);
  });

  it("keeps a page the browser-mode panel can't frame local to this client", () => {
    testState.browserMode = true;
    renderHook();
    runEffect();

    emit("tab-1", {
      kind: "LoadFailed",
      url: "https://example.com/",
      title: "",
      code: 0,
      description: "Can't frame it.",
    });

    // The desktop app shows this page fine: the shared tab must not fail.
    expect(testState.reportCalls).toHaveLength(0);
    expect(testState.localFailureCalls).toEqual([
      [
        threadRef,
        "tab-1",
        { url: "https://example.com/", code: 0, description: "Can't frame it." },
      ],
    ]);
  });

  it("clears a local failure when a history move settles without Loading, not on a repeat", () => {
    renderHook();
    runEffect();
    const page = { kind: "Success", url: "http://10.0.0.2:41000/a", title: "A" } as const;

    emit("tab-1", page);
    emit("tab-1", page); // e.g. a zoom change
    expect(testState.localFailureCalls).toEqual([]);

    emit("tab-1", {
      kind: "LoadFailed",
      url: "http://10.0.0.2:41000/a",
      title: "",
      code: -102,
      description: "ERR_CONNECTION_REFUSED",
    });
    // Back to the same page: the host reports only its Success.
    emit("tab-1", page);
    expect(testState.localFailureCalls.at(-1)).toEqual([threadRef, "tab-1", null]);
  });

  it("clears the pointer when loading begins or a settled URL changes", () => {
    renderHook();
    runEffect();

    emit("tab-1", { kind: "Idle" });
    emit("tab-1", { kind: "Loading", url: "https://one.test", title: "One" });
    emit("tab-1", { kind: "Loading", url: "https://one.test", title: "One" });
    emit("tab-1", { kind: "Success", url: "https://one.test", title: "One" });
    emit("tab-1", { kind: "Success", url: "https://two.test", title: "Two" });
    emit("tab-1", { kind: "Idle" });

    expect(testState.clearCalls).toEqual(["tab-1", "tab-1"]);
  });

  it("resets report de-duplication when the effect is installed again", () => {
    renderHook();
    runEffect();
    const loading = { kind: "Loading", url: "https://example.test", title: "Loading" } as const;
    emit("tab-1", loading);
    emit("tab-1", loading);

    renderHook();
    runEffect();
    emit("tab-1", loading);

    expect(testState.reportCalls).toHaveLength(2);
  });
});
