/**
 * Behaviour tests for PreviewAutomationHosts.
 *
 * The component itself renders to `null`; the reachable logic lives inside the
 * `handleRequest` closure and the module-level async helpers it calls
 * (currentStatus / waitFor*). We use the repo instrumented-hooks pattern:
 * `vi.mock("react")` replaces useState/useEffect so effects are captured and
 * run manually. `handleRequest` is extracted through the `useAtomSet` seam —
 * the mount effect calls `setRequestHandler({ handle: handleRequest })`, which
 * our recorder captures — then invoked directly per operation. All I/O
 * collaborators (preview bridge, atom commands, preview state store, browser
 * recording, target resolvers) are mocked so each switch branch is driven
 * deterministically.
 */
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import {
  EnvironmentId,
  FILL_PREVIEW_VIEWPORT,
  PREVIEW_AUTOMATION_OPERATIONS,
  ThreadId,
} from "@bibcode/contracts";
import type { DesktopPreviewBridge } from "@bibcode/contracts";
import type { PreviewAutomationRequest } from "@bibcode/contracts";

const h = vi.hoisted(() => {
  const state = {
    // react instrumentation
    stateCalls: [] as Array<{ initial: unknown }>,
    effects: [] as Array<() => void | (() => void)>,
    cleanups: [] as Array<() => void>,
    // atom-react seams
    setRequestHandlerCalls: [] as Array<{
      handle: (r: PreviewAutomationRequest) => Promise<unknown>;
    }>,
    automationConnectionId: null as string | null,
    consumerAtom: { __consumer: true } as unknown,
    // environment gating
    environments: [] as Array<{ environmentId: unknown }>,
    automationHostInputs: [] as Array<{ supportedOperations: readonly string[] }>,
    // preview bridge
    previewBridge: null as unknown,
    automationStatus: { available: true, loading: false } as Record<string, unknown>,
    automationEvaluateResult: "complete" as unknown,
    automationResults: {} as Record<string, unknown>,
    navigateCalls: [] as Array<{ tabId: string; url: string }>,
    // preview state store
    previewState: {} as Record<string, unknown>,
    applyCalls: [] as unknown[],
    localFailureCalls: [] as unknown[],
    reconcileCalls: [] as unknown[],
    updateCalls: [] as unknown[],
    // atom command / query results
    commandCalls: [] as Array<{ label: string; input: unknown }>,
    commandResults: {} as Record<string, (input: unknown) => unknown>,
    listPreviewsResult: { _tag: "Success", value: { sessions: {} } } as Record<string, unknown>,
    listPreviewsCalls: [] as unknown[],
    // resolver mocks
    needsSync: false,
    openTab: null as string | null,
    target: { snapshot: null as unknown, tabId: null as string | null },
    openNeedsOverlay: false,
    viewportReady: true,
    resizeSetting: { _tag: "fill" } as unknown,
    resolvedUrl: "http://resolved.local/" as string,
    resolveCalls: [] as unknown[],
    resolveError: null as Error | null,
    gatewayNavUrl: null as string | null,
    gatewayMessage: null as string | null,
    gatewayResolveCalls: [] as Array<{ canonicalUrl: string; tabId?: string }>,
    // browser recording
    activeRecordingTabId: null as string | null,
    startBrowserRecordingImpl: (_tabId: string) => Promise.resolve("2026-01-01T00:00:00.000Z"),
    stopBrowserRecordingImpl: (_tabId: string) => Promise.resolve({ path: "/rec.webm" } as unknown),
    stopTarget: null as string | null,
    // surface store
    surfaceByTabId: {} as Record<string, { visible?: boolean }>,
    openBrowserCalls: [] as unknown[],
    refreshCalls: [] as unknown[],
    // webviews (document.querySelectorAll)
    webviews: [] as unknown[],
    // browser-mode open prompts
    promptCalls: [] as unknown[],
    // focus effect
    docHasFocus: true,
    windowListeners: [] as Array<{ type: string }>,
  };
  return state;
});

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  const resolveInitial = (initial: unknown): unknown =>
    typeof initial === "function" ? (initial as () => unknown)() : initial;
  const useState = (initial?: unknown) => {
    const resolved = resolveInitial(initial);
    h.stateCalls.push({ initial: resolved });
    return [resolved, () => undefined];
  };
  const useEffect = (effect: () => void | (() => void)) => {
    h.effects.push(effect);
  };
  return {
    ...actual,
    useState: useState as typeof actual.useState,
    useEffect: useEffect as typeof actual.useEffect,
    useLayoutEffect: useEffect as typeof actual.useLayoutEffect,
  };
});

vi.mock("@effect/atom-react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@effect/atom-react")>();
  return {
    ...actual,
    useAtomSet: () => (value: unknown) => {
      h.setRequestHandlerCalls.push(
        value as { handle: (r: PreviewAutomationRequest) => Promise<unknown> },
      );
    },
    useAtomValue: () => h.automationConnectionId,
  };
});

vi.mock("@bibcode/client-runtime/state/runtime", () => ({
  squashAtomCommandFailure: (result: { error?: unknown }) =>
    result.error ?? new Error("squashed failure"),
}));

vi.mock("@bibcode/shared/previewViewport", () => ({
  resolvePreviewViewport: () => h.resizeSetting,
}));

vi.mock("~/previewStateStore", () => ({
  applyPreviewServerSnapshot: (...args: unknown[]) => h.applyCalls.push(args),
  readThreadPreviewState: () => h.previewState,
  reconcilePreviewServerSessions: (...args: unknown[]) => h.reconcileCalls.push(args),
  setPreviewLocalFailure: (...args: unknown[]) => h.localFailureCalls.push(args),
  updatePreviewServerSnapshot: (...args: unknown[]) => h.updateCalls.push(args),
}));

vi.mock("~/rightPanelStore", () => ({
  useRightPanelStore: {
    getState: () => ({
      openBrowser: (...args: unknown[]) => h.openBrowserCalls.push(args),
    }),
  },
}));

vi.mock("~/browser/browserTargetResolver", () => ({
  resolveBrowserNavigationTarget: (_environmentId: unknown, target: unknown) => {
    h.resolveCalls.push(target);
    if (h.resolveError) throw h.resolveError;
    return { resolvedUrl: h.resolvedUrl };
  },
}));

vi.mock("~/browser/previewGateway", () => ({
  resolveForNavigation: (input: { canonicalUrl: string; tabId?: string }) => {
    h.gatewayResolveCalls.push(input);
    return Promise.resolve(
      h.gatewayMessage
        ? { kind: "unreachable", message: h.gatewayMessage }
        : { kind: "ok", url: h.gatewayNavUrl ?? input.canonicalUrl },
    );
  },
  isGatewayBootstrapUrl: (url: string | null) => url?.includes("/__bibcode/bootstrap") ?? false,
  canonicalizePreviewUrl: (url: string) =>
    url.startsWith("http://127.0.0.1:50000/") ? h.resolvedUrl : url,
}));

vi.mock("~/browser/browserRecording", () => ({
  readActiveBrowserRecordingTabId: () => h.activeRecordingTabId,
  startBrowserRecording: (tabId: string) => h.startBrowserRecordingImpl(tabId),
  stopBrowserRecording: (tabId: string) => h.stopBrowserRecordingImpl(tabId),
}));

vi.mock("~/browser/browserRecordingScope", () => ({
  resolveBrowserRecordingStopTarget: () => h.stopTarget,
}));

vi.mock("~/browser/browserSurfaceStore", () => ({
  useBrowserSurfaceStore: {
    getState: () => ({ byTabId: h.surfaceByTabId }),
  },
}));

vi.mock("~/state/environments", () => ({
  useEnvironments: () => ({ environments: h.environments }),
}));

vi.mock("~/state/preview", () => ({
  previewEnvironment: {
    automationRequests: (target: { input: { supportedOperations: readonly string[] } }) => {
      h.automationHostInputs.push(target.input);
      return { label: "automationRequests" };
    },
    list: Object.assign((target: unknown) => ({ label: "list", target }), { label: "list" }),
    open: { label: "open" },
    gatewayOpen: { label: "gatewayOpen" },
    resize: { label: "resize" },
    respondToAutomation: { label: "respondToAutomation" },
    focusAutomationHost: { label: "focusAutomationHost" },
  },
}));

vi.mock("~/state/use-atom-query-runner", () => ({
  useAtomQueryRunner: () => (target: unknown) => {
    h.listPreviewsCalls.push(target);
    return Promise.resolve(h.listPreviewsResult);
  },
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: (command: { label?: string } | null | undefined) => {
    const label = command && typeof command.label === "string" ? command.label : "unknown";
    return (input: unknown) => {
      h.commandCalls.push({ label, input });
      const respond = h.commandResults[label] ?? (() => ({ _tag: "Success", value: undefined }));
      return Promise.resolve(respond(input));
    };
  },
}));

vi.mock("./previewBridge", () => ({
  get previewBridge() {
    return h.previewBridge;
  },
  isBrowserMode: () => h.previewBridge === null,
}));

vi.mock("./previewAutomationOpenReadiness", () => ({
  previewAutomationOpenNeedsOverlay: () => h.openNeedsOverlay,
}));

vi.mock("./previewAutomationRequestConsumer", () => ({
  createPreviewAutomationRequestConsumerAtom: () => h.consumerAtom,
}));

vi.mock("./previewAutomationTarget", () => ({
  needsPreviewAutomationSessionSync: () => h.needsSync,
  resolvePreviewAutomationOpenTab: () => h.openTab,
  resolvePreviewAutomationTarget: () => h.target,
}));

vi.mock("~/browser/openPromptQueue", () => ({
  enqueueOpenPrompt: (prompt: unknown) => h.promptCalls.push(prompt),
}));

vi.mock("./previewViewportReadiness", () => ({
  isPreviewViewportReady: () => h.viewportReady,
}));

import { RegistryContext } from "@effect/atom-react";
import { PreviewAutomationHosts } from "./PreviewAutomationHosts";
import { registerPreviewRuntimeCapabilities } from "~/previewRuntimeCapabilities";
import {
  PreviewAutomationOperationError,
  PreviewAutomationPageUnreachableError,
  PreviewAutomationRecordingNotActiveError,
  PreviewAutomationTargetUnavailableError,
} from "./previewAutomationErrors";

const environmentId = EnvironmentId.make("environment-1");
const threadId = ThreadId.make("thread-1");

type Handle = (request: PreviewAutomationRequest) => Promise<unknown>;

const registry = { refresh: vi.fn() };

function makeWebview(tabId: string) {
  return {
    getAttribute: (name: string) => {
      switch (name) {
        case "data-preview-tab":
          return tabId;
        case "data-preview-viewport-key":
          return "fill";
        case "data-preview-css-width":
          return "1024";
        case "data-preview-css-height":
          return "768";
        default:
          return null;
      }
    },
    executeJavaScript: () => Promise.resolve({ width: 1024, height: 768 }),
  };
}

function makeRequest(overrides: Partial<PreviewAutomationRequest> = {}): PreviewAutomationRequest {
  return {
    requestId: "req-1",
    operation: "status",
    threadId,
    timeoutMs: 5000,
    input: undefined,
    ...overrides,
  } as unknown as PreviewAutomationRequest;
}

/** Render one host through the wrapper and extract its `handleRequest`. */
function mountHost(): Handle {
  h.stateCalls.length = 0;
  h.effects.length = 0;
  h.setRequestHandlerCalls.length = 0;
  h.environments = [{ environmentId }];
  renderToStaticMarkup(
    <RegistryContext.Provider value={registry as never}>
      <PreviewAutomationHosts />
    </RegistryContext.Provider>,
  );
  // Run captured effects (first is the setRequestHandler mount effect).
  for (const effect of Array.from(h.effects)) {
    const cleanup = effect();
    if (typeof cleanup === "function") h.cleanups.push(cleanup as () => void);
  }
  const last = h.setRequestHandlerCalls.at(-1);
  expect(last, "expected setRequestHandler to be called with a handle").toBeDefined();
  return last!.handle;
}

beforeEach(() => {
  h.stateCalls.length = 0;
  h.effects.length = 0;
  h.setRequestHandlerCalls.length = 0;
  h.automationConnectionId = null;
  h.environments = [];
  h.automationHostInputs.length = 0;
  h.automationStatus = { available: true, loading: false, visible: false, url: null, title: null };
  h.automationEvaluateResult = "complete";
  h.automationResults = {
    snapshot: { snapshot: true },
    click: { clicked: true },
    type: { typed: true },
    press: { pressed: true },
    scroll: { scrolled: true },
    evaluate: { evaluated: true },
    waitFor: { waited: true },
  };
  h.navigateCalls.length = 0;
  h.previewState = {
    snapshot: null,
    sessions: {},
    desktopByTabId: {},
    localFailures: {},
  };
  h.applyCalls.length = 0;
  h.localFailureCalls.length = 0;
  h.reconcileCalls.length = 0;
  h.updateCalls.length = 0;
  h.commandCalls.length = 0;
  h.commandResults = {};
  h.listPreviewsResult = { _tag: "Success", value: { sessions: {} } };
  h.listPreviewsCalls.length = 0;
  h.needsSync = false;
  h.openTab = null;
  h.target = { snapshot: null, tabId: null };
  h.openNeedsOverlay = false;
  h.viewportReady = true;
  h.resizeSetting = FILL_PREVIEW_VIEWPORT;
  h.resolvedUrl = "http://resolved.local/";
  h.resolveCalls.length = 0;
  h.resolveError = null;
  h.gatewayNavUrl = null;
  h.gatewayMessage = null;
  h.gatewayResolveCalls.length = 0;
  h.activeRecordingTabId = null;
  h.startBrowserRecordingImpl = () => Promise.resolve("2026-01-01T00:00:00.000Z");
  h.stopBrowserRecordingImpl = () => Promise.resolve({ path: "/rec.webm" });
  h.stopTarget = null;
  h.surfaceByTabId = {};
  h.openBrowserCalls.length = 0;
  h.refreshCalls.length = 0;
  h.webviews = [];
  h.promptCalls.length = 0;
  h.docHasFocus = true;
  h.windowListeners.length = 0;
  h.cleanups.length = 0;

  h.previewBridge = {
    navigate: (tabId: string, url: string) => {
      h.navigateCalls.push({ tabId, url });
      return Promise.resolve();
    },
    automation: {
      status: () => Promise.resolve(h.automationStatus),
      evaluate: () => Promise.resolve(h.automationEvaluateResult),
      snapshot: () => Promise.resolve(h.automationResults.snapshot),
      click: () => Promise.resolve(h.automationResults.click),
      type: () => Promise.resolve(h.automationResults.type),
      press: () => Promise.resolve(h.automationResults.press),
      scroll: () => Promise.resolve(h.automationResults.scroll),
      waitFor: () => Promise.resolve(h.automationResults.waitFor),
    },
  };

  const windowStub = {
    setTimeout: (cb: () => void) => {
      cb();
      return 0;
    },
    clearTimeout: () => undefined,
    addEventListener: (type: string) => {
      h.windowListeners.push({ type });
    },
    removeEventListener: () => undefined,
  };
  vi.stubGlobal("window", windowStub);
  vi.stubGlobal("document", {
    querySelectorAll: () => h.webviews,
    hasFocus: () => h.docHasFocus,
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

/** Configure a ready tab: desktop overlay present + bridge status available. */
function seedReadyTab(tabId: string) {
  const snapshot = { tabId, navStatus: { _tag: "Idle" }, viewport: FILL_PREVIEW_VIEWPORT };
  h.previewState = {
    snapshot,
    sessions: { [tabId]: snapshot },
    desktopByTabId: { [tabId]: { loading: false } },
    localFailures: {},
  };
  h.target = { snapshot, tabId };
  h.webviews = [makeWebview(tabId)];
}

describe("PreviewAutomationHosts wrapper", () => {
  it("renders nothing when the preview bridge has no automation surface", () => {
    h.previewBridge = { navigate: () => Promise.resolve() }; // no `.automation`
    h.environments = [{ environmentId }];
    const markup = renderToStaticMarkup(<PreviewAutomationHosts />);
    expect(markup).toBe("");
    expect(h.stateCalls).toHaveLength(0);
  });

  it("renders one host per environment when automation is available", () => {
    h.environments = [{ environmentId }, { environmentId: EnvironmentId.make("environment-2") }];
    h.stateCalls.length = 0;
    renderToStaticMarkup(
      <RegistryContext.Provider value={registry as never}>
        <PreviewAutomationHosts />
      </RegistryContext.Provider>,
    );
    // Each host makes 3 useState calls (clientId, connectionAtom, handlerAtom).
    expect(h.stateCalls.length).toBe(6);
  });

  it("advertises every operation for a fully automatable bridge", () => {
    mountHost();
    expect(h.automationHostInputs.at(-1)?.supportedOperations).toEqual([
      ...PREVIEW_AUTOMATION_OPERATIONS,
    ]);
  });

  it("advertises only status, open, and navigate for a bridge without automation", () => {
    registerPreviewRuntimeCapabilities(h.previewBridge as DesktopPreviewBridge, {
      picker: false,
      recording: false,
      automation: false,
      imageClipboard: false,
      screenshot: true,
      pageTools: true,
    });
    mountHost();
    expect(h.automationHostInputs.at(-1)?.supportedOperations).toEqual([
      "status",
      "open",
      "navigate",
    ]);
  });
});

describe("browser mode (no preview bridge)", () => {
  beforeEach(() => {
    h.previewBridge = null;
  });

  it("advertises only status and open", () => {
    mountHost();
    expect(h.automationHostInputs.at(-1)?.supportedOperations).toEqual(["status", "open"]);
  });

  it("reports the no-automation status", async () => {
    const handle = mountHost();
    await expect(handle(makeRequest({ operation: "status" }))).resolves.toEqual({
      available: false,
      visible: false,
      tabId: null,
      url: null,
      title: null,
      loading: false,
    });
  });

  it("browser-mode automation open returns pending-user and shows the prompt", async () => {
    const handle = mountHost();
    h.resolvedUrl = "http://localhost:5173/";

    await expect(
      handle(makeRequest({ operation: "open", input: { url: "localhost:5173" } as unknown })),
    ).resolves.toEqual({ status: "pending-user" });

    expect(h.resolveCalls).toEqual([{ kind: "url", url: "http://localhost:5173/" }]);
    expect(h.promptCalls).toEqual([
      { source: "agent", url: "http://localhost:5173/", threadRef: { environmentId, threadId } },
    ]);
    expect(h.commandCalls).toHaveLength(0);
  });

  it("refuses an address the browser cannot open instead of prompting for it", async () => {
    const handle = mountHost();

    const error = await handle(
      makeRequest({ operation: "open", input: { url: "file:///tmp/index.html" } as unknown }),
    ).then(
      () => null,
      (e) => e,
    );

    expect(error).toBeInstanceOf(PreviewAutomationOperationError);
    expect(h.promptCalls).toHaveLength(0);
  });

  it("tells the agent why an unreachable address cannot open", async () => {
    const handle = mountHost();
    h.resolveError = new Error("This address is on Box, not this computer.");

    const error = await handle(
      makeRequest({ operation: "open", input: { url: "http://localhost:3000" } as unknown }),
    ).then(
      () => null,
      (e) => e,
    );

    expect(error).toBeInstanceOf(PreviewAutomationOperationError);
    expect(h.promptCalls).toHaveLength(0);
  });
});

describe("handleRequest: status", () => {
  it("returns bridge status merged with visibility + viewport when a desktop overlay exists", async () => {
    const handle = mountHost();
    seedReadyTab("tab-1");
    h.surfaceByTabId = { "tab-1": { visible: true } };
    h.automationStatus = { available: true, loading: false, url: "http://a", title: "A" };

    const result = (await handle(makeRequest({ operation: "status", tabId: "tab-1" }))) as Record<
      string,
      unknown
    >;

    expect(result.available).toBe(true);
    expect(result.visible).toBe(true);
    expect(result.viewport).toEqual({ width: 1024, height: 768 });
    expect(result.viewportSetting).toEqual(FILL_PREVIEW_VIEWPORT);
  });

  it("reports a tab this client can't load instead of a status that looks fine", async () => {
    const handle = mountHost();
    seedReadyTab("tab-1");
    h.previewState = {
      ...h.previewState,
      localFailures: {
        "tab-1": {
          url: "http://localhost:5173/",
          code: 0,
          description: "Build box isn't connected. Reconnect it, then open the link again.",
        },
      },
    };

    const error = await handle(makeRequest({ operation: "status", tabId: "tab-1" })).then(
      () => null,
      (e: unknown) => e,
    );

    expect(error).toBeInstanceOf(PreviewAutomationPageUnreachableError);
    expect((error as Error).message).toBe(
      "Preview tab tab-1 couldn't load its page: Build box isn't connected. Reconnect it, then open the link again.",
    );
  });

  it("derives status from the snapshot navStatus when no desktop overlay exists", async () => {
    const handle = mountHost();
    h.previewState = {
      snapshot: {
        tabId: "tab-9",
        navStatus: { _tag: "Loading", url: "http://loading", title: "Loading" },
      },
      sessions: {},
      desktopByTabId: {},
      localFailures: {},
    };
    h.target = { snapshot: h.previewState.snapshot, tabId: "tab-9" };
    h.webviews = [];

    const result = (await handle(makeRequest({ operation: "status" }))) as Record<string, unknown>;

    expect(result.available).toBe(true);
    expect(result.url).toBe("http://loading");
    expect(result.title).toBe("Loading");
    expect(result.loading).toBe(true);
    expect(result.tabId).toBe("tab-9");
  });

  it("falls back to the session status when the native tab has detached", async () => {
    const handle = mountHost();
    seedReadyTab("tab-1");
    const snapshot = {
      tabId: "tab-1",
      navStatus: { _tag: "Loading", url: "http://committed/", title: "" },
    };
    h.target = { snapshot, tabId: "tab-1" };
    h.automationStatus = { available: false, loading: false, url: null, title: null };

    const result = (await handle(makeRequest({ operation: "status", tabId: "tab-1" }))) as Record<
      string,
      unknown
    >;

    expect(result).toMatchObject({ available: true, url: "http://committed/", loading: true });
  });

  it("reports no tab / idle navStatus when nothing is resolved", async () => {
    const handle = mountHost();
    h.target = { snapshot: null, tabId: null };
    h.previewBridge = null;

    const result = (await handle(makeRequest({ operation: "status" }))) as Record<string, unknown>;

    expect(result.available).toBe(false);
    expect(result.tabId).toBeNull();
    expect(result.url).toBeNull();
    expect(result.loading).toBe(false);
  });
});

describe("handleRequest: session sync", () => {
  it("refreshes and reconciles server sessions before resolving a target", async () => {
    const handle = mountHost();
    h.needsSync = true;
    h.listPreviewsResult = {
      _tag: "Success",
      value: { sessions: [{ tabId: "tab-1" }] },
    };
    // After reconcile, the state exposes the ready tab.
    seedReadyTab("tab-1");

    await handle(makeRequest({ operation: "snapshot", tabId: "tab-1" }));

    expect(registry.refresh).toHaveBeenCalled();
    expect(h.listPreviewsCalls.length).toBe(1);
    expect(h.reconcileCalls.length).toBe(1);
  });

  it("wraps a failed session-sync list in a PreviewAutomationOperationError", async () => {
    const handle = mountHost();
    h.needsSync = true;
    h.listPreviewsResult = { _tag: "Failure", error: new Error("list boom") };

    const error = await handle(makeRequest({ operation: "snapshot", tabId: "tab-1" })).then(
      () => null,
      (e) => e,
    );
    expect(error).toBeInstanceOf(PreviewAutomationOperationError);
  });
});

describe("handleRequest: open", () => {
  it("opens a fresh tab, applies the snapshot, and shows the browser panel", async () => {
    const handle = mountHost();
    h.openTab = null; // nothing to reuse
    h.commandResults.open = () => ({
      _tag: "Success",
      value: { tabId: "tab-new", navStatus: { _tag: "Idle" } },
    });
    h.previewState = { snapshot: null, sessions: {}, desktopByTabId: {}, localFailures: {} };
    h.target = { snapshot: { tabId: "tab-new", navStatus: { _tag: "Idle" } }, tabId: "tab-new" };
    h.webviews = [makeWebview("tab-new")];

    const result = (await handle(
      makeRequest({ operation: "open", input: { show: true } as unknown }),
    )) as Record<string, unknown>;

    expect(h.commandCalls.some((c) => c.label === "open")).toBe(true);
    expect(h.applyCalls.length).toBe(1);
    expect(h.openBrowserCalls.length).toBe(1);
    expect(result.tabId).toBe("tab-new");
  });

  it("reuses an existing tab and navigates when a url is supplied", async () => {
    const handle = mountHost();
    seedReadyTab("tab-existing");
    h.openTab = "tab-existing";
    h.openNeedsOverlay = true;
    h.automationStatus = { ...h.automationStatus, tabId: "tab-existing" };

    const result = (await handle(
      makeRequest({
        operation: "open",
        input: { url: "example.com", reuseExistingTab: true, show: false } as unknown,
      }),
    )) as Record<string, unknown>;

    // No open command needed; navigation drove the reused tab.
    expect(h.commandCalls.some((c) => c.label === "open")).toBe(false);
    expect(h.navigateCalls).toContainEqual({ tabId: "tab-existing", url: h.resolvedUrl });
    expect(result.tabId).toBe("tab-existing");
  });

  it("resolves the url of a fresh tab through the environment before opening it", async () => {
    const handle = mountHost();
    h.openTab = null;
    h.commandResults.open = () => ({
      _tag: "Success",
      value: { tabId: "tab-new", navStatus: { _tag: "Idle" } },
    });

    await handle(
      makeRequest({
        operation: "open",
        input: { url: "http://localhost:3000", show: false } as unknown,
      }),
    );

    expect(h.resolveCalls).toEqual([{ kind: "url", url: "http://localhost:3000" }]);
    expect(h.commandCalls.find((c) => c.label === "open")?.input).toMatchObject({
      input: { url: h.resolvedUrl },
    });
  });

  it("waits for a fresh tab's first load and reports why it couldn't load", async () => {
    const handle = mountHost();
    h.openTab = null;
    h.openNeedsOverlay = true;
    const snapshot = { tabId: "tab-new", navStatus: { _tag: "Loading", url: h.resolvedUrl } };
    h.commandResults.open = () => ({ _tag: "Success", value: snapshot });
    // The tab's native host resolved the URL and couldn't reach it from here.
    h.previewState = {
      snapshot,
      sessions: { "tab-new": snapshot },
      desktopByTabId: { "tab-new": { url: null, loading: false } },
      localFailures: {
        "tab-new": { url: h.resolvedUrl, code: 0, description: "Build box isn't connected." },
      },
    };

    const error = await handle(
      makeRequest({ operation: "open", input: { url: "http://localhost:5173" } as unknown }),
    ).then(
      () => null,
      (e: unknown) => e,
    );

    expect(error).toBeInstanceOf(PreviewAutomationPageUnreachableError);
    expect((error as Error).message).toContain("Build box isn't connected.");
  });

  it("reports a reused tab this client can't load instead of returning it as fine", async () => {
    const handle = mountHost();
    seedReadyTab("tab-existing");
    h.openTab = "tab-existing";
    h.previewState = {
      ...h.previewState,
      localFailures: {
        "tab-existing": { url: "http://localhost:5173/", code: 0, description: "Lost route." },
      },
    };

    const error = await handle(
      makeRequest({ operation: "open", input: { reuseExistingTab: true } as unknown }),
    ).then(
      () => null,
      (e: unknown) => e,
    );

    expect(error).toBeInstanceOf(PreviewAutomationPageUnreachableError);
    expect((error as Error).message).toContain("Lost route.");
  });

  it("returns a fresh tab's status once its first page starts loading", async () => {
    const handle = mountHost();
    h.openTab = null;
    h.openNeedsOverlay = true;
    const snapshot = { tabId: "tab-new", navStatus: { _tag: "Loading", url: h.resolvedUrl } };
    h.commandResults.open = () => ({ _tag: "Success", value: snapshot });
    h.previewState = {
      snapshot,
      sessions: { "tab-new": snapshot },
      desktopByTabId: { "tab-new": { url: h.resolvedUrl, loading: true } },
      localFailures: {},
    };
    h.target = { snapshot, tabId: "tab-new" };
    h.automationStatus = { ...h.automationStatus, tabId: "tab-new" };

    const result = (await handle(
      makeRequest({ operation: "open", input: { url: "http://localhost:5173" } as unknown }),
    )) as Record<string, unknown>;

    expect(result.tabId).toBe("tab-new");
  });

  it("always reveals the tab on a navigation-only bridge, which drives only the visible tab", async () => {
    registerPreviewRuntimeCapabilities(h.previewBridge as DesktopPreviewBridge, {
      picker: false,
      recording: false,
      automation: false,
      imageClipboard: false,
      screenshot: true,
      pageTools: true,
    });
    const handle = mountHost();
    h.openTab = null;
    h.commandResults.open = () => ({
      _tag: "Success",
      value: { tabId: "tab-new", navStatus: { _tag: "Idle" } },
    });

    await handle(makeRequest({ operation: "open", input: { show: false } as unknown }));

    expect(h.openBrowserCalls).toHaveLength(1);
  });

  it("keeps a fully automatable bridge's hidden open hidden", async () => {
    const handle = mountHost();
    h.openTab = null;
    h.commandResults.open = () => ({
      _tag: "Success",
      value: { tabId: "tab-new", navStatus: { _tag: "Idle" } },
    });

    await handle(makeRequest({ operation: "open", input: { show: false } as unknown }));

    expect(h.openBrowserCalls).toHaveLength(0);
  });

  it("refuses to open an unreachable url instead of loading this computer's localhost", async () => {
    const handle = mountHost();
    h.openTab = null;
    h.resolveError = new Error("This address is on Box, not this computer.");

    const error = await handle(
      makeRequest({ operation: "open", input: { url: "http://localhost:3000" } as unknown }),
    ).then(
      () => null,
      (e) => e,
    );

    expect(error).toBeInstanceOf(PreviewAutomationOperationError);
    expect(h.commandCalls.some((c) => c.label === "open")).toBe(false);
  });

  it("wraps an open-command failure", async () => {
    const handle = mountHost();
    h.openTab = null;
    h.commandResults.open = () => ({ _tag: "Failure", error: new Error("open boom") });

    const error = await handle(makeRequest({ operation: "open", input: {} as unknown })).then(
      () => null,
      (e) => e,
    );
    expect(error).toBeInstanceOf(PreviewAutomationOperationError);
  });
});

describe("handleRequest: navigate + resize", () => {
  it("navigates a ready tab and returns the refreshed status", async () => {
    const handle = mountHost();
    seedReadyTab("tab-1");

    const result = (await handle(
      makeRequest({
        operation: "navigate",
        tabId: "tab-1",
        input: { url: "example.com", readiness: "load" } as unknown,
      }),
    )) as Record<string, unknown>;

    expect(h.navigateCalls).toContainEqual({ tabId: "tab-1", url: h.resolvedUrl });
    expect(result.available).toBe(true);
  });

  it("navigates the native view through the gateway and reports the canonical URL", async () => {
    const handle = mountHost();
    seedReadyTab("tab-1");
    h.resolvedUrl = "http://localhost:5173/";
    h.gatewayNavUrl = "http://127.0.0.1:50000/__bibcode/bootstrap?cap=C&to=%2F";
    // The page the bootstrap replaced itself with, on this client's gateway origin.
    h.automationStatus = { ...h.automationStatus, url: "http://127.0.0.1:50000/" };

    const result = (await handle(
      makeRequest({
        operation: "navigate",
        tabId: "tab-1",
        input: { url: "http://localhost:5173/", readiness: "load" } as unknown,
      }),
    )) as Record<string, unknown>;

    expect(h.gatewayResolveCalls).toMatchObject([
      { canonicalUrl: "http://localhost:5173/", tabId: "tab-1" },
    ]);
    expect(h.navigateCalls).toContainEqual({ tabId: "tab-1", url: h.gatewayNavUrl });
    expect(result.url).toBe("http://localhost:5173/");
  });

  it.each(["load", "domContentLoaded"] as const)(
    "waits past the gateway bootstrap hop for %s readiness",
    async (readiness) => {
      const handle = mountHost();
      seedReadyTab("tab-1");
      const bootstrap = {
        ...h.automationStatus,
        loading: false,
        url: "http://127.0.0.1:50000/__bibcode/bootstrap?cap=C&to=%2F",
      };
      const real = { ...h.automationStatus, loading: false, url: "http://127.0.0.1:50000/" };
      // Overlay readiness, then the bootstrap page, then the real page.
      const status = vi
        .fn()
        .mockResolvedValueOnce(real)
        .mockResolvedValueOnce(bootstrap)
        .mockResolvedValue(real);
      (h.previewBridge as { automation: { status: unknown } }).automation.status = status;

      await handle(
        makeRequest({
          operation: "navigate",
          tabId: "tab-1",
          input: { url: "http://localhost:5173/", readiness } as unknown,
        }),
      );

      // overlay check, bootstrap (keeps waiting), real (ready), final status.
      expect(status).toHaveBeenCalledTimes(4);
    },
  );

  it("fails navigation with the gateway's reason", async () => {
    const handle = mountHost();
    seedReadyTab("tab-1");
    h.gatewayMessage = "Nothing is listening.";

    const error = await handle(
      makeRequest({
        operation: "navigate",
        tabId: "tab-1",
        input: { url: "http://localhost:5173/" } as unknown,
      }),
    ).then(
      () => null,
      (e: unknown) => e,
    );

    // The reason reaches the agent: the wire carries only the error's message.
    expect(error).toBeInstanceOf(PreviewAutomationPageUnreachableError);
    expect((error as Error).message).toContain("Nothing is listening.");
    expect(h.navigateCalls).toEqual([]);
  });

  it("clears a stale local failure before driving a resolved navigation", async () => {
    const handle = mountHost();
    seedReadyTab("tab-1");

    await handle(
      makeRequest({
        operation: "navigate",
        tabId: "tab-1",
        input: { url: "http://localhost:5173/" } as unknown,
      }),
    );

    expect(h.localFailureCalls).toEqual([[{ environmentId, threadId }, "tab-1", null]]);
  });

  it("waits for load instead of evaluating script on a bridge without automation", async () => {
    const bridge = h.previewBridge as DesktopPreviewBridge;
    registerPreviewRuntimeCapabilities(bridge, {
      picker: false,
      recording: false,
      automation: false,
      imageClipboard: false,
      screenshot: true,
      pageTools: true,
    });
    const evaluate = vi.fn(() => Promise.reject(new Error("not supported")));
    (bridge.automation as { evaluate: unknown }).evaluate = evaluate;
    const handle = mountHost();
    seedReadyTab("tab-1");

    const result = (await handle(
      makeRequest({
        operation: "navigate",
        tabId: "tab-1",
        input: { url: "example.com", readiness: "domContentLoaded" } as unknown,
      }),
    )) as Record<string, unknown>;

    expect(evaluate).not.toHaveBeenCalled();
    expect(result.available).toBe(true);
  });

  it("resizes a ready tab and returns the applied viewport", async () => {
    const handle = mountHost();
    seedReadyTab("tab-1");
    h.commandResults.resize = () => ({ _tag: "Success", value: { tabId: "tab-1" } });

    const result = (await handle(
      makeRequest({
        operation: "resize",
        tabId: "tab-1",
        input: { viewport: FILL_PREVIEW_VIEWPORT } as unknown,
      }),
    )) as Record<string, unknown>;

    expect(h.commandCalls.some((c) => c.label === "resize")).toBe(true);
    expect(h.updateCalls.length).toBe(1);
    expect(result).toEqual({
      tabId: "tab-1",
      setting: FILL_PREVIEW_VIEWPORT,
      viewport: { width: 1024, height: 768 },
    });
  });

  it("wraps a resize-command failure", async () => {
    const handle = mountHost();
    seedReadyTab("tab-1");
    h.commandResults.resize = () => ({ _tag: "Failure", error: new Error("resize boom") });

    const error = await handle(
      makeRequest({ operation: "resize", tabId: "tab-1", input: {} as unknown }),
    ).then(
      () => null,
      (e) => e,
    );
    expect(error).toBeInstanceOf(PreviewAutomationOperationError);
  });
});

describe("handleRequest: passthrough bridge operations", () => {
  const cases: Array<[PreviewAutomationRequest["operation"], string]> = [
    ["snapshot", "snapshot"],
    ["click", "click"],
    ["type", "type"],
    ["press", "press"],
    ["scroll", "scroll"],
    ["evaluate", "evaluate"],
    ["waitFor", "waitFor"],
  ];
  for (const [operation, key] of cases) {
    it(`forwards ${operation} to the bridge automation surface`, async () => {
      const handle = mountHost();
      seedReadyTab("tab-1");
      const result = await handle(
        makeRequest({ operation, tabId: "tab-1", input: { any: true } as unknown }),
      );
      const expected =
        operation === "evaluate" ? h.automationEvaluateResult : h.automationResults[key];
      expect(result).toEqual(expected);
    });
  }

  it("raises a target-unavailable failure when no ready tab exists", async () => {
    const handle = mountHost();
    h.previewState = {
      snapshot: null,
      sessions: { "tab-1": { tabId: "tab-1" } },
      desktopByTabId: {},
      localFailures: {},
    };
    h.needsSync = false;
    // request has explicit tab but bridge is missing
    h.previewBridge = null;

    const error = await handle(
      makeRequest({ operation: "snapshot", tabId: "tab-1", input: {} as unknown }),
    ).then(
      () => null,
      (e) => e,
    );
    // requireReadyTab throws a host-typed error that fromCause re-surfaces unwrapped.
    expect(error).toBeInstanceOf(PreviewAutomationTargetUnavailableError);
  });

  it("wraps a rejected bridge call in a PreviewAutomationOperationError", async () => {
    const handle = mountHost();
    seedReadyTab("tab-1");
    (h.previewBridge as { automation: { snapshot: () => Promise<unknown> } }).automation.snapshot =
      () => Promise.reject(new Error("bridge boom"));

    const error = await handle(
      makeRequest({ operation: "snapshot", tabId: "tab-1", input: {} as unknown }),
    ).then(
      () => null,
      (e) => e,
    );
    expect(error).toBeInstanceOf(PreviewAutomationOperationError);
  });
});

describe("handleRequest: recording", () => {
  it("starts a recording on a ready tab", async () => {
    const handle = mountHost();
    seedReadyTab("tab-1");
    h.startBrowserRecordingImpl = () => Promise.resolve("2026-02-02T00:00:00.000Z");

    const result = (await handle(
      makeRequest({ operation: "recordingStart", tabId: "tab-1", input: {} as unknown }),
    )) as Record<string, unknown>;

    expect(result).toEqual({
      tabId: "tab-1",
      recording: true,
      startedAt: "2026-02-02T00:00:00.000Z",
    });
  });

  it("stops the active recording and returns the artifact", async () => {
    const handle = mountHost();
    seedReadyTab("tab-1");
    h.activeRecordingTabId = "tab-1";
    h.stopTarget = "tab-1";
    h.stopBrowserRecordingImpl = () => Promise.resolve({ path: "/out.webm" });

    const result = await handle(
      makeRequest({ operation: "recordingStop", tabId: "tab-1", input: {} as unknown }),
    );
    expect(result).toEqual({ path: "/out.webm" });
  });

  it("raises a recording-not-active error when nothing is recording", async () => {
    const handle = mountHost();
    seedReadyTab("tab-1");
    h.activeRecordingTabId = null;
    h.stopTarget = null;

    const error = await handle(
      makeRequest({ operation: "recordingStop", tabId: "tab-1", input: {} as unknown }),
    ).then(
      () => null,
      (e) => e,
    );
    // This tagged error is re-surfaced unwrapped by fromCause.
    expect(error).toBeInstanceOf(PreviewAutomationRecordingNotActiveError);
  });
});

describe("focus reporting effect", () => {
  it("reports host focus and registers window focus/blur listeners", () => {
    h.automationConnectionId = "conn-1";
    mountHost();

    // The focus effect ran during mountHost(); it should have registered
    // listeners and reported focus through the command.
    expect(h.windowListeners.map((l) => l.type)).toEqual(expect.arrayContaining(["focus", "blur"]));
    expect(h.commandCalls.some((c) => c.label === "focusAutomationHost")).toBe(true);
  });

  it("skips the focus report when there is no connection id", () => {
    h.automationConnectionId = null;
    mountHost();
    expect(h.commandCalls.some((c) => c.label === "focusAutomationHost")).toBe(false);
    // Listeners are still registered so a later connection can report.
    expect(h.windowListeners.map((l) => l.type)).toEqual(expect.arrayContaining(["focus", "blur"]));
  });
});
