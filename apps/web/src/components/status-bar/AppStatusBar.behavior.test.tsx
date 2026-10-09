import { EnvironmentId } from "@bibcode/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  selectedEnvironment: null as null | { environmentId: string },
  primaryLocalEnvironment: null as null | { environmentId: string },
  primaryLocalSelectionInput: null as unknown,
  refSeeds: {} as Record<number, unknown>,
  refs: [] as Array<{ current: unknown }>,
  refIndex: 0,
  stateIndex: 0,
  iconOnly: false,
  setIconOnly: vi.fn(),
  effects: [] as Array<() => void | (() => void)>,
  queries: [] as Array<{
    data: unknown;
    error: string | null;
    isPending: boolean;
    refresh: ReturnType<typeof vi.fn>;
    revalidate: ReturnType<typeof vi.fn>;
    requiresRetry: boolean;
  }>,
  queryInputs: [] as unknown[],
  providerUsage: vi.fn((input: unknown) => ({ kind: "usage", input })),
  diagnostics: vi.fn((input: unknown) => ({ kind: "diagnostics", input })),
  history: vi.fn((input: unknown) => ({ kind: "history", input })),
  terminalSessions: [] as unknown[],
  terminalInput: null as unknown,
  refreshProviderUsage: vi.fn(),
  consumeCodexRateLimitReset: vi.fn(),
  navigate: vi.fn(),
  clientSettings: {
    statusBarItems: ["claude", "codex", "resource-usage"],
    statusBarUsageMode: "detailed",
    usagePercentageDisplay: "remaining",
  },
  presentationOptions: [] as unknown[],
  providerControls: [] as Array<Record<string, unknown>>,
  buttons: [] as Array<Record<string, unknown>>,
  resourceSegments: [] as Array<Record<string, unknown>>,
  resizeCallback: null as ResizeObserverCallback | null,
  observe: vi.fn(),
  disconnect: vi.fn(),
}));

vi.mock("./ProfilerRecordingSegment", () => ({ ProfilerRecordingSegment: () => null }));

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useCallback: (callback: unknown) => callback,
  useEffect: (effect: () => void | (() => void)) => harness.effects.push(effect),
  useRef: (initial: unknown) => {
    const index = harness.refIndex++;
    const current = Object.hasOwn(harness.refSeeds, index) ? harness.refSeeds[index] : initial;
    const ref = { current };
    harness.refs[index] = ref;
    return ref;
  },
  useState: (initial: unknown) => {
    const index = harness.stateIndex++;
    return index === 0
      ? [harness.iconOnly, harness.setIconOnly]
      : [typeof initial === "function" ? (initial as () => unknown)() : initial, vi.fn()];
  },
}));
vi.mock("../../state/entities", () => ({
  useActiveEnvironmentId: () => harness.selectedEnvironment?.environmentId ?? null,
}));
vi.mock("../../state/environments", () => ({
  usePrimaryLocalEnvironmentForSelected: (selectedEnvironmentId: unknown) => {
    harness.primaryLocalSelectionInput = selectedEnvironmentId;
    return harness.primaryLocalEnvironment;
  },
}));
vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (input: unknown) => {
    const index = harness.queryInputs.length;
    harness.queryInputs.push(input);
    return harness.queries[index];
  },
}));
vi.mock("../../state/server", () => ({
  serverEnvironment: {
    providerUsage: (input: unknown) => harness.providerUsage(input),
    processDiagnostics: (input: unknown) => harness.diagnostics(input),
    processResourceHistory: (input: unknown) => harness.history(input),
    refreshProviderUsage: { label: "refresh" },
    consumeCodexRateLimitReset: { label: "reset" },
  },
}));
vi.mock("../../state/terminalSessions", () => ({
  useKnownTerminalSessions: (input: unknown) => {
    harness.terminalInput = input;
    return harness.terminalSessions;
  },
}));
vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: (command: { label: string }) =>
    command.label === "reset" ? harness.consumeCodexRateLimitReset : harness.refreshProviderUsage,
}));
vi.mock("../../hooks/useSettings", () => ({
  useClientSettings: (selector: (settings: typeof harness.clientSettings) => unknown) =>
    selector(harness.clientSettings),
}));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => harness.navigate,
}));
vi.mock("../ui/button", () => ({
  Button: (props: Record<string, unknown>) => {
    harness.buttons.push(props);
    return <button type="button">{props.children as React.ReactNode}</button>;
  },
}));
vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  TooltipTrigger: ({ render }: { render: React.ReactNode }) => <>{render}</>,
  TooltipPopup: ({ children }: { children: React.ReactNode }) => <span>{children}</span>,
}));
vi.mock("./ProviderUsageControl", () => ({
  ProviderUsageControl: (props: Record<string, unknown>) => {
    harness.providerControls.push(props);
    return <span data-provider-control />;
  },
}));
vi.mock("./ResourceUsageSegment", () => ({
  ResourceUsageSegment: (props: Record<string, unknown>) => {
    harness.resourceSegments.push(props);
    return <span data-resource-segment />;
  },
}));
vi.mock("./providerUsagePresentation", () => ({
  buildProviderUsageViewModels: (
    providers: ReadonlyArray<Record<string, unknown>>,
    options: unknown,
  ) => {
    harness.presentationOptions.push(options);
    return providers;
  },
  providerUsageRelativeLabelKey: () => "stable-label-bucket",
}));

import {
  AppStatusBar,
  AppStatusBarView,
  createStatusBarRefreshHandler,
  createStatusBarResourceRefreshHandler,
  startStatusBarUsageAutoRefresh,
} from "./AppStatusBar";

function query(data: unknown = null, isPending = false, error: string | null = null) {
  return { data, error, isPending, refresh: vi.fn(), revalidate: vi.fn(), requiresRetry: false };
}

function renderStatusBar(): string {
  harness.refIndex = 0;
  harness.stateIndex = 0;
  harness.refs.length = 0;
  harness.effects.length = 0;
  harness.queryInputs.length = 0;
  harness.buttons.length = 0;
  harness.providerControls.length = 0;
  harness.resourceSegments.length = 0;
  return renderToStaticMarkup(<AppStatusBar />);
}

function invokeRef(index: number): void {
  const callback = harness.refs[index]?.current;
  if (typeof callback !== "function") throw new Error(`Missing callback ref ${index}`);
  callback();
}

function latestProviderControl(): Record<string, unknown> {
  const control = harness.providerControls.at(-1);
  if (control === undefined) throw new Error("ProviderUsageControl was not rendered.");
  return control;
}

beforeEach(() => {
  harness.selectedEnvironment = null;
  harness.primaryLocalEnvironment = null;
  harness.primaryLocalSelectionInput = null;
  harness.refSeeds = {};
  harness.iconOnly = false;
  harness.setIconOnly.mockReset();
  harness.queries = [query(), query(), query()];
  harness.providerUsage.mockClear();
  harness.diagnostics.mockClear();
  harness.history.mockClear();
  harness.terminalSessions = [];
  harness.terminalInput = null;
  harness.refreshProviderUsage.mockReset();
  harness.refreshProviderUsage.mockResolvedValue({ _tag: "Success" });
  harness.consumeCodexRateLimitReset.mockReset();
  harness.navigate.mockReset().mockResolvedValue(undefined);
  harness.clientSettings = {
    statusBarItems: ["claude", "codex", "resource-usage"],
    statusBarUsageMode: "detailed",
    usagePercentageDisplay: "remaining",
  };
  harness.presentationOptions.length = 0;
  harness.providerControls.length = 0;
  harness.resourceSegments.length = 0;
  harness.resizeCallback = null;
  harness.observe.mockReset();
  harness.disconnect.mockReset();
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(callback: ResizeObserverCallback) {
        harness.resizeCallback = callback;
      }
      observe = harness.observe;
      disconnect = harness.disconnect;
    },
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("status bar refresh guards", () => {
  it("skips provider and resource refreshes without an environment", async () => {
    const refreshProviderUsage = vi.fn();
    const refreshUsageQuery = vi.fn();
    const refreshDiagnostics = vi.fn();
    const refreshHistory = vi.fn();
    const refreshInput = {
      environmentId: null,
      refreshProviderUsage,
      refreshUsageQuery,
      refreshProcessDiagnostics: refreshDiagnostics,
      refreshLocalProcessDiagnostics: refreshHistory,
      refreshResourceHistory: refreshHistory,
    };
    await createStatusBarRefreshHandler(refreshInput)(false);
    const resourceRefreshInput = {
      environmentId: null,
      refreshProcessDiagnostics: refreshDiagnostics,
      refreshLocalProcessDiagnostics: refreshHistory,
      refreshResourceHistory: refreshHistory,
    };
    createStatusBarResourceRefreshHandler(resourceRefreshInput)();
    expect(refreshProviderUsage).not.toHaveBeenCalled();
    expect(refreshDiagnostics).not.toHaveBeenCalled();
  });

  it("absorbs rejected background refreshes", async () => {
    vi.useFakeTimers();
    const refresh = vi.fn().mockRejectedValue(new Error("offline"));
    const cleanup = startStatusBarUsageAutoRefresh({ refresh, intervalMs: 10 });
    await Promise.resolve();
    await Promise.resolve();
    vi.advanceTimersByTime(10);
    await Promise.resolve();
    expect(refresh).toHaveBeenCalledTimes(2);
    cleanup();
  });
});

describe("AppStatusBar", () => {
  it("keeps the provider query warm while hidden and re-enables from cached data", () => {
    const environmentId = EnvironmentId.make("environment-warm");
    const cachedUsage = { providers: [{ provider: "claude" }, { provider: "codex" }] };
    harness.selectedEnvironment = { environmentId };
    harness.clientSettings.statusBarItems = ["resource-usage"];
    harness.queries = [query(cachedUsage), query(), query()];

    renderStatusBar();
    expect(harness.providerUsage).toHaveBeenCalledWith({ environmentId, input: {} });
    expect(harness.providerControls).toHaveLength(0);
    expect(harness.refreshProviderUsage).not.toHaveBeenCalled();

    harness.clientSettings.statusBarItems = ["claude", "codex", "resource-usage"];
    renderStatusBar();
    expect(harness.providerControls).toHaveLength(2);
    expect(harness.providerControls.map((control) => control.viewModel)).toEqual(
      cachedUsage.providers,
    );
    expect(harness.refreshProviderUsage).not.toHaveBeenCalled();
  });

  it("propagates percentage, detail, breakpoint, and settings navigation", () => {
    const environmentId = EnvironmentId.make("environment-settings");
    harness.selectedEnvironment = { environmentId };
    harness.clientSettings.statusBarUsageMode = "compact";
    harness.clientSettings.usagePercentageDisplay = "used";
    harness.iconOnly = true;
    harness.queries = [query({ providers: [{ provider: "codex" }] }, true), query(), query()];

    renderStatusBar();
    expect(harness.presentationOptions.at(-1)).toMatchObject({ percentageDisplay: "used" });
    expect(harness.providerControls[0]).toMatchObject({
      statusBarUsageMode: "compact",
      iconOnly: true,
      viewModel: { provider: "codex" },
    });

    (latestProviderControl().onOpenProviderSettings as () => void)();
    expect(harness.navigate).toHaveBeenCalledWith({ to: "/settings/providers" });
  });

  it("uses null queries and tears down refreshers without an environment", () => {
    const usageCleanup = vi.fn();
    const resourceCleanup = vi.fn();
    harness.refSeeds = { 3: usageCleanup, 4: "old", 5: resourceCleanup, 6: "old" };
    renderStatusBar();
    expect(harness.queryInputs).toEqual([null, null, null]);
    expect(harness.terminalInput).toEqual({ environmentId: null, threadId: null });
    harness.effects[0]?.();
    harness.effects[1]?.();
    harness.effects[3]?.();
    harness.effects[4]?.();
    expect(usageCleanup).toHaveBeenCalledOnce();
    expect(resourceCleanup).toHaveBeenCalledOnce();
    expect(harness.refs[3]?.current).toBeNull();
    expect(harness.refs[6]?.current).toBeNull();
    expect(harness.effects[5]?.()).toBeUndefined();

    harness.refs[3]!.current = usageCleanup;
    harness.refs[4]!.current = "old";
    harness.refs[5]!.current = resourceCleanup;
    harness.refs[6]!.current = "old";
    const unmount = harness.effects[2]?.();
    if (typeof unmount === "function") unmount();
    expect(usageCleanup).toHaveBeenCalledTimes(2);
    expect(resourceCleanup).toHaveBeenCalledTimes(2);
  });

  it("queries selected and desktop-local live diagnostics without requesting history", () => {
    const environmentId = EnvironmentId.make("environment-1");
    const localEnvironmentId = EnvironmentId.make("primary");
    harness.selectedEnvironment = { environmentId };
    harness.primaryLocalEnvironment = { environmentId: localEnvironmentId };
    harness.queries = [query(null), query(), query()];
    harness.refSeeds = { 4: environmentId, 6: environmentId };
    renderStatusBar();
    expect(harness.queryInputs).toHaveLength(3);
    expect(harness.primaryLocalSelectionInput).toBe(environmentId);
    expect(harness.providerUsage).toHaveBeenCalledWith({ environmentId, input: {} });
    expect(harness.diagnostics).toHaveBeenNthCalledWith(1, { environmentId, input: {} });
    expect(harness.diagnostics).toHaveBeenNthCalledWith(2, {
      environmentId: localEnvironmentId,
      input: {},
    });
    expect(harness.history).not.toHaveBeenCalled();
    harness.effects[3]?.();
    harness.effects[4]?.();
    expect(harness.refs[3]?.current).toBeNull();
    expect(harness.refs[5]?.current).toBeNull();
  });

  it("avoids a local diagnostics RPC for browser or selected-local environments", () => {
    const environmentId = EnvironmentId.make("primary");
    harness.selectedEnvironment = { environmentId };
    harness.primaryLocalEnvironment = null;
    harness.queries = [query({ providers: [] }), query({ diagnostics: true }), query()];

    renderStatusBar();

    expect(harness.queryInputs[2]).toBeNull();
    expect(harness.diagnostics).toHaveBeenCalledOnce();
    expect(harness.history).not.toHaveBeenCalled();
  });

  it("replaces refresh intervals when the environment changes", async () => {
    vi.useFakeTimers();
    const environmentId = EnvironmentId.make("environment-2");
    const localEnvironmentId = EnvironmentId.make("primary");
    const oldUsageCleanup = vi.fn();
    const oldResourceCleanup = vi.fn();
    harness.selectedEnvironment = { environmentId };
    harness.primaryLocalEnvironment = { environmentId: localEnvironmentId };
    harness.queries = [
      query({ providers: [] }),
      query({ diagnostics: true }),
      query({
        totals: { core: { processCount: 1, rssBytes: 1024, cpuPercent: 1 } },
        uiCoverage: { status: "available" },
      }),
    ];
    harness.terminalSessions = [{ id: 1 }, { id: 2 }];
    harness.iconOnly = true;
    harness.refSeeds = {
      3: oldUsageCleanup,
      4: EnvironmentId.make("environment-old"),
      5: oldResourceCleanup,
      6: EnvironmentId.make("environment-old"),
    };
    const markup = renderStatusBar();
    expect(markup).toContain("data-resource-segment");
    expect(harness.resourceSegments[0]).toMatchObject({
      terminalCount: 2,
      iconOnly: true,
      diagnostics: {
        diagnostics: { diagnostics: true },
        queryError: null,
      },
      localDiagnostics: {
        diagnostics: {
          totals: { core: { processCount: 1, rssBytes: 1024, cpuPercent: 1 } },
          uiCoverage: { status: "available" },
        },
        queryError: null,
      },
    });
    harness.effects[0]?.();
    harness.effects[1]?.();
    harness.effects[3]?.();
    harness.effects[4]?.();
    await Promise.resolve();
    expect(oldUsageCleanup).toHaveBeenCalledOnce();
    expect(oldResourceCleanup).toHaveBeenCalledOnce();
    expect(harness.refreshProviderUsage).toHaveBeenCalledOnce();
    expect(harness.queries[1]?.revalidate).toHaveBeenCalledTimes(2);
    expect(harness.queries[2]?.revalidate).toHaveBeenCalledTimes(2);
    expect(typeof harness.refs[3]?.current).toBe("function");
    expect(typeof harness.refs[5]?.current).toBe("function");
    invokeRef(3);
    invokeRef(5);
  });

  it("keeps exhausted queries latched through usage and resource timer ticks", async () => {
    vi.useFakeTimers();
    harness.selectedEnvironment = { environmentId: EnvironmentId.make("remote") };
    harness.primaryLocalEnvironment = { environmentId: EnvironmentId.make("local") };
    harness.queries = [
      query({ providers: [] }, false, "The connection dropped."),
      query(null, false, "The connection dropped."),
      query(null, false, "The connection dropped."),
    ];
    for (const value of harness.queries) value.requiresRetry = true;
    renderStatusBar();
    const cleanups = harness.effects.map((effect) => effect());
    try {
      await vi.advanceTimersByTimeAsync(61_000); // 30 s usage and 2 s resource intervals
      expect(harness.refreshProviderUsage).toHaveBeenCalledTimes(3);
      for (const value of harness.queries) {
        expect(value.revalidate).toHaveBeenCalled();
        expect(value.refresh).not.toHaveBeenCalled();
        expect(value.requiresRetry).toBe(true);
      }
    } finally {
      for (const cleanup of cleanups.toReversed()) if (typeof cleanup === "function") cleanup();
    }
  });

  it("keeps safe no-op refreshers until command handlers are installed", () => {
    vi.useFakeTimers();
    const environmentId = EnvironmentId.make("environment-initializing");
    harness.selectedEnvironment = { environmentId };
    harness.queries = [query({ providers: [] }), query(), query()];
    renderStatusBar();
    harness.effects[3]?.();
    harness.effects[4]?.();
    expect(typeof harness.refs[3]?.current).toBe("function");
    expect(typeof harness.refs[5]?.current).toBe("function");
    invokeRef(3);
    invokeRef(5);
  });

  it("switches to icon-only mode from observer and fallback widths", () => {
    const element = { clientWidth: 480 };
    harness.refSeeds = { 0: element };
    renderStatusBar();
    const cleanup = harness.effects[5]?.();
    expect(harness.observe).toHaveBeenCalledWith(element);
    harness.resizeCallback?.(
      [{ contentRect: { width: 900 } } as ResizeObserverEntry],
      {} as ResizeObserver,
    );
    expect(harness.setIconOnly).toHaveBeenCalledWith(false);
    harness.resizeCallback?.(
      [{ contentRect: { width: 800 } } as ResizeObserverEntry],
      {} as ResizeObserver,
    );
    expect(harness.setIconOnly).toHaveBeenCalledWith(true);
    harness.resizeCallback?.(
      [{ contentRect: { width: 600 } } as ResizeObserverEntry],
      {} as ResizeObserver,
    );
    expect(harness.setIconOnly).toHaveBeenCalledWith(true);
    harness.resizeCallback?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
    expect(harness.setIconOnly).toHaveBeenCalledWith(true);
    if (typeof cleanup === "function") cleanup();
    expect(harness.disconnect).toHaveBeenCalledOnce();
  });
});

describe("AppStatusBarView defaults", () => {
  it("renders no provider control or refresh affordance without provider snapshots", () => {
    renderToStaticMarkup(
      <AppStatusBarView
        usage={null}
        diagnostics={{ diagnostics: null, queryError: null }}
        localDiagnostics={null}
        terminalCount={0}
        isRefreshing
        iconOnly
        onRefresh={vi.fn()}
      />,
    );
    expect(harness.providerControls).toHaveLength(0);
    expect(harness.buttons).toHaveLength(0);
  });

  it("filters provider snapshots for display and controls Resource Manager independently", () => {
    const usage = {
      providers: [{ provider: "claude" }, { provider: "codex" }],
    } as never;

    renderToStaticMarkup(
      <AppStatusBarView
        usage={usage}
        diagnostics={{ diagnostics: null, queryError: null }}
        localDiagnostics={null}
        terminalCount={0}
        showClaudeUsage={false}
        showCodexUsage
        showResourceUsage={false}
        onRefresh={vi.fn()}
      />,
    );

    expect(harness.providerControls).toHaveLength(1);
    expect(harness.providerControls[0]?.viewModel).toEqual({ provider: "codex" });
    expect(harness.resourceSegments).toHaveLength(0);

    harness.providerControls.length = 0;
    harness.resourceSegments.length = 0;
    renderToStaticMarkup(
      <AppStatusBarView
        usage={usage}
        diagnostics={{ diagnostics: null, queryError: null }}
        localDiagnostics={null}
        terminalCount={0}
        showClaudeUsage
        showCodexUsage={false}
        showResourceUsage
        onRefresh={vi.fn()}
      />,
    );
    expect(harness.providerControls[0]?.viewModel).toEqual({ provider: "claude" });
    expect(harness.resourceSegments).toHaveLength(1);
  });

  it("renders Claude then Codex as independent triggers followed by one shared refresh", () => {
    const onRefresh = vi.fn();
    const markup = renderToStaticMarkup(
      <AppStatusBarView
        usage={{ providers: [{ provider: "codex" }, { provider: "claude" }] } as never}
        diagnostics={{ diagnostics: null, queryError: null }}
        localDiagnostics={null}
        terminalCount={0}
        onRefresh={onRefresh}
      />,
    );

    expect(harness.providerControls.map((control) => control.viewModel)).toEqual([
      { provider: "claude" },
      { provider: "codex" },
    ]);
    expect(markup.match(/aria-label="Refresh provider usage"/g)).toHaveLength(1);
    expect(markup.indexOf("data-provider-control")).toBeLessThan(
      markup.indexOf('aria-label="Refresh provider usage"'),
    );
  });

  it("removes provider controls and their refresh affordance when both are hidden", () => {
    const markup = renderToStaticMarkup(
      <AppStatusBarView
        usage={{ providers: [{ provider: "claude" }, { provider: "codex" }] } as never}
        diagnostics={{ diagnostics: null, queryError: null }}
        localDiagnostics={null}
        terminalCount={0}
        showClaudeUsage={false}
        showCodexUsage={false}
        showResourceUsage
        onRefresh={vi.fn()}
      />,
    );

    expect(harness.providerControls).toHaveLength(0);
    expect(harness.buttons).toHaveLength(0);
    expect(markup).not.toContain("Refresh status bar usage");
    expect(harness.resourceSegments).toHaveLength(1);
  });

  it("renders no status indicators when every status bar item is hidden", () => {
    const markup = renderToStaticMarkup(
      <AppStatusBarView
        usage={{ providers: [{ provider: "claude" }, { provider: "codex" }] } as never}
        diagnostics={{ diagnostics: null, queryError: null }}
        localDiagnostics={null}
        terminalCount={0}
        showClaudeUsage={false}
        showCodexUsage={false}
        showResourceUsage={false}
        onRefresh={vi.fn()}
      />,
    );

    expect(harness.providerControls).toHaveLength(0);
    expect(harness.resourceSegments).toHaveLength(0);
    expect(harness.buttons).toHaveLength(0);
    expect(markup).not.toContain("Refresh status bar usage");
  });
});
