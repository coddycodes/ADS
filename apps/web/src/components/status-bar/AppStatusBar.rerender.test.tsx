// @vitest-environment happy-dom

import type {
  EnvironmentId,
  ServerProcessDiagnosticsResult,
  ServerProviderUsageResult,
} from "@bibcode/contracts";
import { RegistryContext } from "@effect/atom-react";
import * as Cause from "effect/Cause";
import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";
import { AsyncResult, Atom, AtomRegistry } from "effect/unstable/reactivity";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  primaryEnvironmentId: null as EnvironmentId | null,
  selectedEnvironmentId: null as EnvironmentId | null,
  localEnvironmentId: null as EnvironmentId | null,
  usageAtom: null as unknown,
  usageAtoms: new Map<string, unknown>(),
  diagnosticsAtoms: new Map<string, unknown>(),
  providerUsageCalls: [] as EnvironmentId[],
  processDiagnosticsCalls: [] as EnvironmentId[],
  resourceProps: [] as Array<Record<string, unknown>>,
  providerProps: [] as Array<Record<string, unknown>>,
  providerMountIds: [] as number[],
  refreshProviderUsage: vi.fn(),
  consumeCodexRateLimitReset: vi.fn(),
  navigate: vi.fn(),
}));

vi.mock("./ProfilerRecordingSegment", () => ({ ProfilerRecordingSegment: () => null }));

vi.mock("../../state/environments", () => ({
  usePrimaryEnvironment: () =>
    harness.primaryEnvironmentId === null ? null : { environmentId: harness.primaryEnvironmentId },
  usePrimaryLocalEnvironmentForSelected: () =>
    harness.localEnvironmentId === null ? null : { environmentId: harness.localEnvironmentId },
}));

vi.mock("../../state/entities", () => ({
  useActiveEnvironmentId: () => harness.selectedEnvironmentId,
}));

vi.mock("../../state/server", () => ({
  serverEnvironment: {
    providerUsage: ({ environmentId }: { environmentId: EnvironmentId }) => {
      harness.providerUsageCalls.push(environmentId);
      return harness.usageAtoms.get(environmentId) ?? harness.usageAtom;
    },
    processDiagnostics: ({ environmentId }: { environmentId: EnvironmentId }) => {
      harness.processDiagnosticsCalls.push(environmentId);
      const atom = harness.diagnosticsAtoms.get(environmentId);
      if (atom === undefined) throw new Error(`Missing diagnostics atom for ${environmentId}`);
      return atom;
    },
    refreshProviderUsage: { label: "refresh-provider-usage" },
    consumeCodexRateLimitReset: { label: "consume-codex-rate-limit-reset" },
  },
}));

vi.mock("../../state/terminalSessions", () => ({
  useKnownTerminalSessions: () => [],
}));

vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: (command: { label: string }) =>
    command.label === "consume-codex-rate-limit-reset"
      ? harness.consumeCodexRateLimitReset
      : harness.refreshProviderUsage,
}));

vi.mock("../../hooks/useSettings", () => ({
  useClientSettings: (
    selector: (settings: {
      statusBarItems: string[];
      statusBarUsageMode: string;
      usagePercentageDisplay: string;
    }) => unknown,
  ) =>
    selector({
      statusBarItems: ["claude", "codex", "resource-usage"],
      statusBarUsageMode: "detailed",
      usagePercentageDisplay: "remaining",
    }),
  useUpdateClientSettings: () => vi.fn(),
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => harness.navigate,
}));

vi.mock("../ui/button", () => ({
  Button: ({ children, ...props }: { children: ReactNode }) => (
    <button type="button" {...props}>
      {children}
    </button>
  ),
}));

vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children: ReactNode }) => <>{children}</>,
  TooltipTrigger: ({ render }: { render: ReactNode }) => <>{render}</>,
  TooltipPopup: ({ children }: { children: ReactNode }) => <>{children}</>,
}));

vi.mock("./ProviderUsageControl", async () => {
  const React = await vi.importActual<typeof import("react")>("react");
  let nextMountId = 0;
  return {
    ProviderUsageControl: (props: Record<string, unknown>) => {
      const mountId = React.useRef(++nextMountId).current;
      harness.providerMountIds.push(mountId);
      harness.providerProps.push(props);
      return <span data-provider-usage />;
    },
  };
});

vi.mock("./ResourceUsageSegment", () => ({
  ResourceUsageSegment: (props: Record<string, unknown>) => {
    harness.resourceProps.push(props);
    return <span data-resource-usage />;
  },
}));

import { AppStatusBar } from "./AppStatusBar";

const readAt = DateTime.makeUnsafe("2026-07-19T18:00:00.000Z");
const remoteEnvironmentId = "remote" as EnvironmentId;
const primaryEnvironmentId = "primary" as EnvironmentId;

function diagnostics(
  rssBytes: number,
  patch: Partial<ServerProcessDiagnosticsResult> = {},
): ServerProcessDiagnosticsResult {
  return {
    serverPid: 100,
    readAt,
    totals: {
      combined: { processCount: 1, rssBytes, cpuPercent: 1 },
      core: { processCount: 1, rssBytes, cpuPercent: 1 },
      external: { processCount: 0, rssBytes: 0, cpuPercent: 0 },
    },
    uiCoverage: { status: "available", message: Option.none() },
    processes: [],
    error: Option.none(),
    ...patch,
  };
}

const usage: ServerProviderUsageResult = {
  readAt,
  isFetching: false,
  providers: [],
};

function codexUsage(usedPercent: number, readAtValue = readAt): ServerProviderUsageResult {
  return {
    readAt: readAtValue,
    isFetching: false,
    providers: [
      {
        provider: "codex",
        status: "ok",
        session: {
          usedPercent,
          windowMinutes: 300,
          resetsAt: null,
          resetDescription: null,
        },
        weekly: null,
        fableWeekly: null,
        planType: "plus",
        rateLimitResetCredits: {
          availableCount: 1,
          totalEarnedCount: 1,
          nextExpiresAt: null,
        },
        updatedAt: readAtValue,
        error: null,
        metadata: {},
      },
    ],
  };
}

function deferred<T>(): {
  readonly promise: Promise<T>;
  readonly resolve: (value: T) => void;
} {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

function providerPercentageLabel(): string | undefined {
  return (
    latestProviderProps().viewModel as {
      compactWindows: Array<{ percentageLabel: string }>;
    }
  ).compactWindows[0]?.percentageLabel;
}

let root: Root;
let container: HTMLDivElement;
let registry: AtomRegistry.AtomRegistry;
let remoteDiagnosticsAtom: Atom.Writable<
  AsyncResult.AsyncResult<ServerProcessDiagnosticsResult, Error>,
  AsyncResult.AsyncResult<ServerProcessDiagnosticsResult, Error>
>;
let localDiagnosticsAtom: Atom.Writable<
  AsyncResult.AsyncResult<ServerProcessDiagnosticsResult, Error>,
  AsyncResult.AsyncResult<ServerProcessDiagnosticsResult, Error>
>;

function makeDiagnosticsAtom(
  initial: AsyncResult.AsyncResult<ServerProcessDiagnosticsResult, Error>,
): Atom.Writable<
  AsyncResult.AsyncResult<ServerProcessDiagnosticsResult, Error>,
  AsyncResult.AsyncResult<ServerProcessDiagnosticsResult, Error>
> {
  return Atom.make(initial);
}

function latestResourceProps(): Record<string, unknown> {
  const props = harness.resourceProps.at(-1);
  if (props === undefined) throw new Error("ResourceUsageSegment was not rendered.");
  return props;
}

function latestProviderProps(): Record<string, unknown> {
  const props = harness.providerProps.at(-1);
  if (props === undefined) throw new Error("ProviderUsageControl was not rendered.");
  return props;
}

async function renderStatusBar(): Promise<void> {
  await act(async () => {
    root.render(
      <RegistryContext.Provider value={registry}>
        <AppStatusBar />
      </RegistryContext.Provider>,
    );
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  harness.primaryEnvironmentId = primaryEnvironmentId;
  harness.selectedEnvironmentId = remoteEnvironmentId;
  harness.localEnvironmentId = null;
  harness.providerUsageCalls = [];
  harness.processDiagnosticsCalls = [];
  harness.resourceProps = [];
  harness.providerProps = [];
  harness.providerMountIds = [];
  harness.refreshProviderUsage.mockReset().mockResolvedValue({ _tag: "Success" });
  harness.consumeCodexRateLimitReset.mockReset();
  harness.navigate.mockReset();

  const initialRemoteDiagnostics: AsyncResult.AsyncResult<ServerProcessDiagnosticsResult, Error> =
    AsyncResult.success(diagnostics(700_000_000));
  const initialLocalDiagnostics: AsyncResult.AsyncResult<ServerProcessDiagnosticsResult, Error> =
    AsyncResult.failure<ServerProcessDiagnosticsResult, Error>(
      Cause.fail(new Error("Local diagnostics failed.")),
    );
  remoteDiagnosticsAtom = makeDiagnosticsAtom(initialRemoteDiagnostics);
  localDiagnosticsAtom = makeDiagnosticsAtom(initialLocalDiagnostics);
  harness.usageAtom = Atom.make(AsyncResult.success(usage));
  harness.usageAtoms = new Map();
  harness.diagnosticsAtoms = new Map([
    [remoteEnvironmentId, remoteDiagnosticsAtom],
    [primaryEnvironmentId, localDiagnosticsAtom],
  ]);
  registry = AtomRegistry.make({
    scheduleTask: (task) => {
      task();
      return () => {};
    },
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  registry.dispose();
  container.remove();
  vi.unstubAllGlobals();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("AppStatusBar real hook rerenders", () => {
  it("shows and refreshes the selected server's usage when the primary connection stays local", async () => {
    harness.usageAtoms.set(primaryEnvironmentId, Atom.make(AsyncResult.success(codexUsage(90))));
    harness.usageAtoms.set(remoteEnvironmentId, Atom.make(AsyncResult.success(codexUsage(30))));

    await renderStatusBar();

    expect(providerPercentageLabel()).toBe("70% remaining");
    expect(harness.refreshProviderUsage).toHaveBeenCalledWith({
      environmentId: remoteEnvironmentId,
      input: { providers: ["claude", "codex"], force: false },
    });

    harness.selectedEnvironmentId = primaryEnvironmentId;
    await renderStatusBar();
    expect(providerPercentageLabel()).toBe("10% remaining");

    harness.selectedEnvironmentId = remoteEnvironmentId;
    await renderStatusBar();
    expect(providerPercentageLabel()).toBe("70% remaining");
  });

  it.each(["loading", "offline"] as const)(
    "does not display local usage while the selected remote is %s",
    async (state) => {
      harness.usageAtoms.set(primaryEnvironmentId, Atom.make(AsyncResult.success(codexUsage(90))));
      harness.usageAtoms.set(
        remoteEnvironmentId,
        Atom.make(
          state === "loading"
            ? AsyncResult.initial<ServerProviderUsageResult, Error>(true)
            : AsyncResult.failure<ServerProviderUsageResult, Error>(
                Cause.fail(new Error("Remote server is offline.")),
              ),
        ),
      );

      await renderStatusBar();

      expect(harness.providerProps).toEqual([]);
      expect(harness.providerUsageCalls).not.toContain(primaryEnvironmentId);
      expect(harness.refreshProviderUsage).not.toHaveBeenCalled();
    },
  );

  it("renders reset command usage through the mounted provider cache immediately", async () => {
    const beforeReset = codexUsage(83);
    const afterReset = codexUsage(0, DateTime.makeUnsafe("2026-07-19T18:01:00.000Z"));
    const usageAtom = harness.usageAtom as Atom.Writable<
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>,
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>
    >;
    registry.set(usageAtom, AsyncResult.success(beforeReset));
    harness.consumeCodexRateLimitReset.mockResolvedValue(
      AsyncResult.success({ outcome: "reset", usage: afterReset }),
    );

    await renderStatusBar();
    expect(
      (
        latestProviderProps().viewModel as {
          compactWindows: Array<{ percentageLabel: string }>;
        }
      ).compactWindows[0]?.percentageLabel,
    ).toBe("17% remaining");
    harness.refreshProviderUsage.mockClear();

    await act(async () => {
      await (
        latestProviderProps().onConsumeCodexRateLimitReset as (
          requestId: string,
        ) => Promise<unknown>
      )("reset-request-1");
    });

    expect(harness.consumeCodexRateLimitReset).toHaveBeenCalledWith({
      environmentId: remoteEnvironmentId,
      input: { requestId: "reset-request-1" },
    });
    expect(
      (
        latestProviderProps().viewModel as {
          compactWindows: Array<{ percentageLabel: string }>;
        }
      ).compactWindows[0]?.percentageLabel,
    ).toBe("100% remaining");
    expect(harness.refreshProviderUsage).not.toHaveBeenCalled();
  });

  it("binds a reset overlay to the query emission current when redemption completes", async () => {
    const usageAtom = harness.usageAtom as Atom.Writable<
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>,
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>
    >;
    registry.set(usageAtom, AsyncResult.success(codexUsage(83)));
    const pendingReset = deferred<ReturnType<typeof AsyncResult.success>>();
    harness.consumeCodexRateLimitReset.mockReturnValue(pendingReset.promise);
    await renderStatusBar();

    let resetPromise: Promise<unknown> | undefined;
    await act(async () => {
      resetPromise = (
        latestProviderProps().onConsumeCodexRateLimitReset as (
          requestId: string,
        ) => Promise<unknown>
      )("intervening-emission-reset");
      await Promise.resolve();
    });
    await act(async () => {
      registry.set(usageAtom, AsyncResult.success(codexUsage(55)));
    });
    expect(providerPercentageLabel()).toBe("45% remaining");

    pendingReset.resolve(
      AsyncResult.success({
        outcome: "reset",
        usage: codexUsage(0, DateTime.makeUnsafe("2026-07-19T18:01:00.000Z")),
      }),
    );
    await act(async () => {
      await resetPromise;
    });
    expect(providerPercentageLabel()).toBe("100% remaining");

    await act(async () => {
      registry.set(usageAtom, AsyncResult.success(codexUsage(60)));
    });
    expect(providerPercentageLabel()).toBe("40% remaining");
  });

  it("retires a reset overlay on a later equal-timestamp query emission", async () => {
    const beforeReset = codexUsage(83);
    const usageAtom = harness.usageAtom as Atom.Writable<
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>,
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>
    >;
    const originalEmission = AsyncResult.success<ServerProviderUsageResult, Error>(beforeReset);
    registry.set(usageAtom, originalEmission);
    harness.consumeCodexRateLimitReset.mockResolvedValue(
      AsyncResult.success({ outcome: "reset", usage: codexUsage(0) }),
    );
    await renderStatusBar();

    await act(async () => {
      await (
        latestProviderProps().onConsumeCodexRateLimitReset as (
          requestId: string,
        ) => Promise<unknown>
      )("equal-timestamp-reset");
    });
    expect(providerPercentageLabel()).toBe("100% remaining");

    await act(async () => {
      registry.set(usageAtom, AsyncResult.success(codexUsage(55)));
    });
    expect(providerPercentageLabel()).toBe("45% remaining");

    await act(async () => {
      registry.set(usageAtom, originalEmission);
    });
    expect(providerPercentageLabel()).toBe("17% remaining");
  });

  it("keeps a reset overlay through SWR waiting until the next success settles", async () => {
    const beforeReset = codexUsage(83);
    const usageAtom = harness.usageAtom as Atom.Writable<
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>,
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>
    >;
    const sourceSuccess = AsyncResult.success<ServerProviderUsageResult, Error>(beforeReset);
    registry.set(usageAtom, sourceSuccess);
    harness.consumeCodexRateLimitReset.mockResolvedValue(
      AsyncResult.success({ outcome: "reset", usage: codexUsage(0) }),
    );
    await renderStatusBar();

    await act(async () => {
      await (
        latestProviderProps().onConsumeCodexRateLimitReset as (
          requestId: string,
        ) => Promise<unknown>
      )("swr-waiting-success-reset");
    });
    expect(providerPercentageLabel()).toBe("100% remaining");

    await act(async () => {
      registry.set(usageAtom, AsyncResult.waiting(sourceSuccess));
    });
    expect(providerPercentageLabel()).toBe("100% remaining");

    await act(async () => {
      registry.set(usageAtom, AsyncResult.success(codexUsage(45)));
    });
    expect(providerPercentageLabel()).toBe("55% remaining");
  });

  it("keeps a reset overlay when SWR waiting settles as failure with previous success", async () => {
    const beforeReset = codexUsage(83);
    const usageAtom = harness.usageAtom as Atom.Writable<
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>,
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>
    >;
    const sourceSuccess = AsyncResult.success<ServerProviderUsageResult, Error>(beforeReset);
    registry.set(usageAtom, sourceSuccess);
    harness.consumeCodexRateLimitReset.mockResolvedValue(
      AsyncResult.success({ outcome: "reset", usage: codexUsage(0) }),
    );
    await renderStatusBar();

    await act(async () => {
      await (
        latestProviderProps().onConsumeCodexRateLimitReset as (
          requestId: string,
        ) => Promise<unknown>
      )("swr-waiting-failure-reset");
    });
    await act(async () => {
      registry.set(usageAtom, AsyncResult.waiting(sourceSuccess));
    });
    expect(providerPercentageLabel()).toBe("100% remaining");

    await act(async () => {
      registry.set(
        usageAtom,
        AsyncResult.failure(Cause.fail(new Error("Refresh failed.")), {
          previousSuccess: Option.some(sourceSuccess),
        }),
      );
    });
    expect(providerPercentageLabel()).toBe("100% remaining");

    await act(async () => {
      registry.set(usageAtom, AsyncResult.success(codexUsage(65)));
    });
    expect(providerPercentageLabel()).toBe("35% remaining");
  });

  it("retires a reset overlay when a later query emission regresses the server clock", async () => {
    const usageAtom = harness.usageAtom as Atom.Writable<
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>,
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>
    >;
    registry.set(usageAtom, AsyncResult.success(codexUsage(83)));
    harness.consumeCodexRateLimitReset.mockResolvedValue(
      AsyncResult.success({
        outcome: "reset",
        usage: codexUsage(0, DateTime.makeUnsafe("2026-07-19T18:01:00.000Z")),
      }),
    );
    await renderStatusBar();

    await act(async () => {
      await (
        latestProviderProps().onConsumeCodexRateLimitReset as (
          requestId: string,
        ) => Promise<unknown>
      )("clock-regression-reset");
    });
    expect(providerPercentageLabel()).toBe("100% remaining");

    await act(async () => {
      registry.set(
        usageAtom,
        AsyncResult.success(codexUsage(60, DateTime.makeUnsafe("2026-07-19T17:59:00.000Z"))),
      );
    });
    expect(providerPercentageLabel()).toBe("40% remaining");
  });

  it("retires a reset overlay across a reconnect null emission so it cannot reappear", async () => {
    const beforeReset = codexUsage(83);
    const usageAtom = harness.usageAtom as Atom.Writable<
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>,
      AsyncResult.AsyncResult<ServerProviderUsageResult, Error>
    >;
    const originalEmission = AsyncResult.success<ServerProviderUsageResult, Error>(beforeReset);
    registry.set(usageAtom, originalEmission);
    harness.consumeCodexRateLimitReset.mockResolvedValue(
      AsyncResult.success({ outcome: "reset", usage: codexUsage(0) }),
    );
    await renderStatusBar();

    await act(async () => {
      await (
        latestProviderProps().onConsumeCodexRateLimitReset as (
          requestId: string,
        ) => Promise<unknown>
      )("reconnect-reset");
    });
    expect(providerPercentageLabel()).toBe("100% remaining");

    await act(async () => {
      registry.set(usageAtom, AsyncResult.initial(false));
    });
    expect(container.querySelector("[data-provider-usage]")).toBeNull();

    await act(async () => {
      registry.set(usageAtom, originalEmission);
    });
    expect(providerPercentageLabel()).toBe("17% remaining");
  });

  it("remounts provider interaction state per environment and isolates a pending reset result", async () => {
    const remoteUsageAtom = Atom.make(
      AsyncResult.success<ServerProviderUsageResult, Error>(codexUsage(83)),
    );
    const primaryUsageAtom = Atom.make(
      AsyncResult.success<ServerProviderUsageResult, Error>(codexUsage(25)),
    );
    harness.usageAtoms = new Map([
      [remoteEnvironmentId, remoteUsageAtom],
      [primaryEnvironmentId, primaryUsageAtom],
    ]);
    const pendingReset = deferred<ReturnType<typeof AsyncResult.success>>();
    harness.consumeCodexRateLimitReset.mockReturnValue(pendingReset.promise);
    await renderStatusBar();
    const remoteMountId = harness.providerMountIds.at(-1);

    let resetPromise: Promise<unknown> | undefined;
    await act(async () => {
      resetPromise = (
        latestProviderProps().onConsumeCodexRateLimitReset as (
          requestId: string,
        ) => Promise<unknown>
      )("environment-reset");
      await Promise.resolve();
    });

    harness.selectedEnvironmentId = primaryEnvironmentId;
    await renderStatusBar();
    expect(harness.providerMountIds.at(-1)).not.toBe(remoteMountId);
    expect(providerPercentageLabel()).toBe("75% remaining");
    expect(
      container.querySelector<HTMLButtonElement>('[aria-label="Refresh provider usage"]')?.disabled,
    ).toBe(false);

    pendingReset.resolve(
      AsyncResult.success({
        outcome: "reset",
        usage: codexUsage(0, DateTime.makeUnsafe("2026-07-19T18:01:00.000Z")),
      }),
    );
    await act(async () => {
      await resetPromise;
    });
    expect(providerPercentageLabel()).toBe("75% remaining");

    harness.selectedEnvironmentId = remoteEnvironmentId;
    await renderStatusBar();
    expect(providerPercentageLabel()).toBe("100% remaining");
  });

  it("disables refresh immediately and single-flights repeated activation per environment", async () => {
    harness.usageAtom = Atom.make(
      AsyncResult.success<ServerProviderUsageResult, Error>(codexUsage(40)),
    );
    await renderStatusBar();
    await act(async () => {
      await Promise.resolve();
    });
    harness.refreshProviderUsage.mockClear();
    const pendingRefresh = deferred<{ _tag: "Success" }>();
    harness.refreshProviderUsage.mockReturnValue(pendingRefresh.promise);
    const refreshButton = container.querySelector<HTMLButtonElement>(
      '[aria-label="Refresh provider usage"]',
    );
    if (refreshButton === null) throw new Error("Provider refresh button was not rendered.");

    act(() => {
      refreshButton.click();
      refreshButton.click();
    });

    expect(harness.refreshProviderUsage).toHaveBeenCalledTimes(1);
    expect(harness.refreshProviderUsage).toHaveBeenCalledWith({
      environmentId: remoteEnvironmentId,
      input: { providers: ["claude", "codex"], force: true },
    });
    expect(refreshButton.disabled).toBe(true);

    pendingRefresh.resolve({ _tag: "Success" });
    await act(async () => {
      await pendingRefresh.promise;
      await Promise.resolve();
    });
    expect(refreshButton.disabled).toBe(false);
  });

  it("allows one forced manual refresh to overlap an automatic refresh", async () => {
    harness.usageAtom = Atom.make(
      AsyncResult.success<ServerProviderUsageResult, Error>(codexUsage(40)),
    );
    const pendingRefresh = deferred<{ _tag: "Success" }>();
    harness.refreshProviderUsage.mockReturnValue(pendingRefresh.promise);

    await renderStatusBar();
    const refreshButton = container.querySelector<HTMLButtonElement>(
      '[aria-label="Refresh provider usage"]',
    );
    if (refreshButton === null) throw new Error("Provider refresh button was not rendered.");
    expect(harness.refreshProviderUsage).toHaveBeenNthCalledWith(1, {
      environmentId: remoteEnvironmentId,
      input: { providers: ["claude", "codex"], force: false },
    });

    act(() => {
      refreshButton.click();
    });

    expect(harness.refreshProviderUsage).toHaveBeenNthCalledWith(2, {
      environmentId: remoteEnvironmentId,
      input: { providers: ["claude", "codex"], force: true },
    });

    pendingRefresh.resolve({ _tag: "Success" });
    await act(async () => {
      await pendingRefresh.promise;
      await Promise.resolve();
    });
  });

  it("keeps query hook order stable across browser/desktop and remote/local failures", async () => {
    await renderStatusBar();
    expect(latestResourceProps()).toMatchObject({
      diagnostics: {
        diagnostics: expect.objectContaining({ serverPid: 100 }),
        queryError: null,
      },
      localDiagnostics: null,
    });
    expect(harness.processDiagnosticsCalls).toEqual([remoteEnvironmentId]);

    await act(async () => {
      registry.set(
        remoteDiagnosticsAtom,
        AsyncResult.failure(Cause.fail(new Error("Selected diagnostics failed."))),
      );
      registry.set(
        localDiagnosticsAtom,
        AsyncResult.failure(Cause.fail(new Error("Local diagnostics failed."))),
      );
    });
    expect(harness.processDiagnosticsCalls).not.toContain(primaryEnvironmentId);
    harness.localEnvironmentId = primaryEnvironmentId;
    await renderStatusBar();
    expect(latestResourceProps()).toMatchObject({
      diagnostics: { diagnostics: null, queryError: "Selected diagnostics failed." },
      localDiagnostics: { diagnostics: null, queryError: "Local diagnostics failed." },
    });

    const remoteSuccess = AsyncResult.success<ServerProcessDiagnosticsResult, Error>(
      diagnostics(700_000_000),
    );
    const localSuccess = AsyncResult.success<ServerProcessDiagnosticsResult, Error>(
      diagnostics(100_000_000),
    );
    await act(async () => {
      registry.set(
        remoteDiagnosticsAtom,
        AsyncResult.failure(Cause.fail(new Error("Selected connection lost.")), {
          previousSuccess: Option.some(remoteSuccess),
        }),
      );
      registry.set(
        localDiagnosticsAtom,
        AsyncResult.failure(Cause.fail(new Error("Local connection lost.")), {
          previousSuccess: Option.some(localSuccess),
        }),
      );
    });
    await renderStatusBar();
    expect(latestResourceProps()).toMatchObject({
      diagnostics: {
        diagnostics: expect.objectContaining({ serverPid: 100 }),
        queryError: "Selected connection lost.",
      },
      localDiagnostics: {
        diagnostics: expect.objectContaining({ serverPid: 100 }),
        queryError: "Local connection lost.",
      },
    });

    harness.selectedEnvironmentId = primaryEnvironmentId;
    harness.localEnvironmentId = null;
    const callsBeforeSelectingLocal = harness.processDiagnosticsCalls.length;
    await renderStatusBar();
    expect(latestResourceProps()).toMatchObject({
      diagnostics: {
        diagnostics: null,
        queryError: "Local diagnostics failed.",
      },
      localDiagnostics: null,
    });
    // The compiled bar requests each diagnostics atom once per environment, not once per render.
    expect([...new Set(harness.processDiagnosticsCalls.slice(callsBeforeSelectingLocal))]).toEqual([
      primaryEnvironmentId,
    ]);
  });
});
