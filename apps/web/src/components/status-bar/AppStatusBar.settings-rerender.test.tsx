// @vitest-environment happy-dom

import { RegistryContext } from "@effect/atom-react";
import type { ServerProviderUsageResult } from "@bibcode/contracts";
import { DEFAULT_CLIENT_SETTINGS } from "@bibcode/contracts/settings";
import * as DateTime from "effect/DateTime";
import { AtomRegistry } from "effect/unstable/reactivity";
import { act, useLayoutEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  providerDerivationCount: 0,
  providerProps: [] as Array<Record<string, unknown>>,
  statusBarRenderCount: 0,
}));

vi.mock("./ProfilerRecordingSegment", () => ({ ProfilerRecordingSegment: () => null }));

vi.mock("~/localApi", () => ({
  ensureLocalApi: () => ({
    persistence: {
      getClientSettings: vi.fn(async () => null),
      setClientSettings: vi.fn(async () => undefined),
    },
  }),
}));

vi.mock("../../state/entities", () => ({
  useActiveEnvironmentId: () => null,
}));

vi.mock("../../state/environments", () => ({
  useEnvironment: () => null,
  usePrimaryLocalEnvironmentForSelected: () => null,
}));

vi.mock("../../state/query", () => ({
  useEnvironmentQuery: () => ({
    data: null,
    emission: { _tag: "Initial" },
    error: null,
    isPending: false,
    refresh: vi.fn(),
    revalidate: vi.fn(),
    requiresRetry: false,
  }),
}));

vi.mock("../../state/server", () => ({
  primaryServerSettingsAtom: {},
  serverEnvironment: {
    providerUsage: vi.fn(),
    processDiagnostics: vi.fn(),
    refreshProviderUsage: {},
    consumeCodexRateLimitReset: {},
    settingsValueAtom: vi.fn(),
    updateSettings: {},
  },
}));

vi.mock("../../state/terminalSessions", () => ({
  // AppStatusBar calls this hook once per render; the compiler never memoizes hook calls.
  useKnownTerminalSessions: () => {
    harness.statusBarRenderCount += 1;
    return [];
  },
}));

vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: () => vi.fn(async () => ({ _tag: "Success" })),
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => vi.fn(),
}));

vi.mock("./ProviderUsageControl", () => ({
  ProviderUsageControl: (props: Record<string, unknown>) => {
    harness.providerProps.push(props);
    return <span data-provider-usage />;
  },
}));

vi.mock("./providerUsagePresentation", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./providerUsagePresentation")>();
  return {
    ...actual,
    buildProviderUsageViewModels: (
      ...args: Parameters<typeof actual.buildProviderUsageViewModels>
    ) => {
      harness.providerDerivationCount += 1;
      return actual.buildProviderUsageViewModels(...args);
    },
  };
});

vi.mock("./ResourceUsageSegment", () => ({
  ResourceUsageSegment: () => <span data-resource-usage />,
}));

import {
  __resetClientSettingsPersistenceForTests,
  __setClientSettingsForTests,
  useUpdateClientSettings,
} from "../../hooks/useSettings";
import { AppStatusBar, AppStatusBarView } from "./AppStatusBar";

let root: Root;
let container: HTMLDivElement;
let registry: AtomRegistry.AtomRegistry;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  __resetClientSettingsPersistenceForTests();
  __setClientSettingsForTests(DEFAULT_CLIENT_SETTINGS);
  harness.providerDerivationCount = 0;
  harness.providerProps.length = 0;
  harness.statusBarRenderCount = 0;
  registry = AtomRegistry.make();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  registry.dispose();
  container.remove();
  __resetClientSettingsPersistenceForTests();
  vi.unstubAllGlobals();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("AppStatusBar client-settings subscription", () => {
  it("does not rerender for unrelated client settings and rerenders for its selected slice", async () => {
    let updateSettings: ReturnType<typeof useUpdateClientSettings> | null = null;

    function SettingsUpdater() {
      const update = useUpdateClientSettings();
      useLayoutEffect(() => {
        updateSettings = update;
      }, [update]);
      return null;
    }

    await act(async () => {
      root.render(
        <RegistryContext.Provider value={registry}>
          <AppStatusBar />
          <SettingsUpdater />
        </RegistryContext.Provider>,
      );
    });
    expect(harness.statusBarRenderCount).toBe(1);

    await act(async () => updateSettings?.({ wordWrap: false }));
    expect(harness.statusBarRenderCount).toBe(1);

    await act(async () => updateSettings?.({ statusBarUsageMode: "compact" }));
    expect(harness.statusBarRenderCount).toBe(2);
  });

  it("does not repeat provider derivation for resource-only rerenders", async () => {
    const now = DateTime.makeUnsafe("2026-07-22T12:00:00.000Z");
    const usage = usageFixture(now);
    const renderView = (queryError: string | null) => (
      <AppStatusBarView
        usage={usage}
        diagnostics={{ diagnostics: null, queryError }}
        localDiagnostics={null}
        presentationNow={now}
        terminalCount={0}
        onRefresh={vi.fn()}
      />
    );

    await act(async () => root.render(renderView(null)));
    expect(harness.providerDerivationCount).toBe(1);

    await act(async () => root.render(renderView("resource refresh failed")));
    expect(harness.providerDerivationCount).toBe(1);
  });

  it("rederives when the countdown label bucket or structural inputs change", async () => {
    const start = DateTime.makeUnsafe("2026-07-22T12:00:00.000Z");
    const sameBucket = DateTime.makeUnsafe("2026-07-22T12:00:30.000Z");
    const laterBucket = DateTime.makeUnsafe("2026-07-22T13:31:00.000Z");
    const usage = usageFixture(start);
    const renderView = (
      presentationNow: DateTime.Utc,
      currentUsage: ServerProviderUsageResult = usage,
      usagePercentageDisplay: "remaining" | "used" = "remaining",
    ) => (
      <AppStatusBarView
        usage={currentUsage}
        diagnostics={{ diagnostics: null, queryError: null }}
        localDiagnostics={null}
        presentationNow={presentationNow}
        terminalCount={0}
        usagePercentageDisplay={usagePercentageDisplay}
        onRefresh={vi.fn()}
      />
    );

    await act(async () => root.render(renderView(start)));
    expect(latestProviderLabels()).toEqual({
      percentage: "75% remaining",
      reset: "Resets in 2h",
      creditExpiry: "Resets in 2h",
    });

    await act(async () => root.render(renderView(sameBucket)));
    expect(harness.providerDerivationCount).toBe(1);

    await act(async () => root.render(renderView(laterBucket)));
    expect(harness.providerDerivationCount).toBe(2);
    expect(latestProviderLabels()).toEqual({
      percentage: "75% remaining",
      reset: "Resets in 59m",
      creditExpiry: "Resets in 1h",
    });

    const changedUsage = usageFixture(start, 80);
    await act(async () => root.render(renderView(laterBucket, changedUsage)));
    expect(harness.providerDerivationCount).toBe(3);
    expect(latestProviderLabels().percentage).toBe("20% remaining");

    await act(async () => root.render(renderView(laterBucket, changedUsage, "used")));
    expect(harness.providerDerivationCount).toBe(4);
    expect(latestProviderLabels().percentage).toBe("80% used");
  });
  it("reads the clock on every render when no presentation clock is passed", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    try {
      vi.setSystemTime(new Date("2026-07-22T12:00:00.000Z"));
      const usage = usageFixture(DateTime.makeUnsafe("2026-07-22T12:00:00.000Z"));
      const renderView = (queryError: string | null) => (
        <AppStatusBarView
          usage={usage}
          diagnostics={{ diagnostics: null, queryError }}
          localDiagnostics={null}
          terminalCount={0}
          onRefresh={vi.fn()}
        />
      );

      await act(async () => root.render(renderView(null)));
      expect(latestProviderLabels().reset).toBe("Resets in 2h");

      // A resource poll re-renders the bar after the countdown crossed a label bucket.
      vi.setSystemTime(new Date("2026-07-22T13:31:00.000Z"));
      await act(async () => root.render(renderView("resource refresh failed")));
      expect(latestProviderLabels()).toMatchObject({
        reset: "Resets in 59m",
        creditExpiry: "Resets in 1h",
      });
    } finally {
      vi.useRealTimers();
    }
  });
});

function usageFixture(now: DateTime.Utc, usedPercent = 25): ServerProviderUsageResult {
  return {
    readAt: now,
    isFetching: false,
    providers: [
      {
        provider: "codex",
        status: "ok",
        session: {
          usedPercent,
          windowMinutes: 300,
          resetsAt: DateTime.makeUnsafe("2026-07-22T14:30:00.000Z"),
          resetDescription: null,
        },
        weekly: null,
        fableWeekly: null,
        planType: "plus",
        rateLimitResetCredits: {
          availableCount: 2,
          totalEarnedCount: 5,
          nextExpiresAt: DateTime.makeUnsafe("2026-07-22T14:45:00.000Z"),
        },
        updatedAt: now,
        error: null,
        metadata: {},
      },
    ],
  };
}

function latestProviderLabels() {
  const props = harness.providerProps.at(-1);
  if (props === undefined) throw new Error("ProviderUsageControl was not rendered.");
  const provider = props.viewModel as {
    windows: Array<{ percentageLabel: string; resetLabel: string | null }>;
    credits: { nextExpiresLabel: string | null } | null;
  };
  return {
    percentage: provider.windows[0]?.percentageLabel,
    reset: provider.windows[0]?.resetLabel,
    creditExpiry: provider.credits?.nextExpiresLabel,
  };
}
