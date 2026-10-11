import type { ServerProcessDiagnosticsResult, ServerProviderUsageResult } from "@bibcode/contracts";
import { EnvironmentId, PRIMARY_LOCAL_ENVIRONMENT_ID } from "@bibcode/contracts";
import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vite-plus/test";

import * as environmentSelectors from "../../state/environments";
import {
  AppStatusBarView,
  STATUS_BAR_RESOURCE_REFRESH_INTERVAL_MS,
  STATUS_BAR_USAGE_REFRESH_INTERVAL_MS,
  createStatusBarRefreshHandler,
  createStatusBarResourceRefreshHandler,
  startStatusBarUsageAutoRefresh,
} from "./AppStatusBar";

const profilerEnvironments = vi.hoisted(() => ({ list: [] as unknown[] }));

vi.mock("../../state/environments", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../state/environments")>()),
  useEnvironments: () => ({ environments: profilerEnvironments.list }),
}));

vi.mock("@tanstack/react-router", () => ({ useNavigate: () => vi.fn() }));

vi.mock("../ui/popover", () => ({
  Popover: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  PopoverTrigger: ({
    children,
    ...props
  }: React.ComponentProps<"button"> & { children: React.ReactNode }) => (
    <button type="button" {...props}>
      {children}
    </button>
  ),
  PopoverPopup: ({
    children,
    viewportClassName: _viewportClassName,
    ...props
  }: React.ComponentProps<"div"> & { viewportClassName?: string }) => (
    <div {...props}>{children}</div>
  ),
  PopoverContent: ({ children, ...props }: React.ComponentProps<"div">) => (
    <div {...props}>{children}</div>
  ),
}));

const readAt = DateTime.makeUnsafe("2026-07-07T18:00:00.000Z");

const usage: ServerProviderUsageResult = {
  readAt,
  isFetching: false,
  providers: [
    {
      provider: "claude",
      status: "ok",
      session: {
        usedPercent: 0,
        windowMinutes: 300,
        resetsAt: null,
        resetDescription: null,
      },
      weekly: {
        usedPercent: 56,
        windowMinutes: 10080,
        resetsAt: null,
        resetDescription: null,
      },
      fableWeekly: null,
      planType: null,
      rateLimitResetCredits: null,
      updatedAt: readAt,
      error: null,
      metadata: {},
    },
    {
      provider: "codex",
      status: "ok",
      session: {
        usedPercent: 11,
        windowMinutes: 300,
        resetsAt: null,
        resetDescription: null,
      },
      weekly: {
        usedPercent: 83,
        windowMinutes: 10080,
        resetsAt: null,
        resetDescription: null,
      },
      fableWeekly: null,
      planType: null,
      rateLimitResetCredits: null,
      updatedAt: readAt,
      error: null,
      metadata: {},
    },
  ],
};

const diagnostics: ServerProcessDiagnosticsResult = {
  serverPid: 100,
  readAt,
  totals: {
    combined: { processCount: 2, rssBytes: 736_300_000, cpuPercent: 4.2 },
    core: { processCount: 1, rssBytes: 700_000_000, cpuPercent: 3.2 },
    external: { processCount: 1, rssBytes: 36_300_000, cpuPercent: 1 },
  },
  uiCoverage: { status: "notApplicable", message: Option.none() },
  processes: [
    {
      processKey: "100:1",
      pid: 100,
      ppid: 1,
      pgid: Option.none(),
      status: "Run",
      cpuPercent: 3.2,
      rssBytes: 700_000_000,
      elapsed: "00:00:01",
      command: "bibcode server",
      depth: 0,
      childPids: [],
      scope: "core",
      kind: "server",
      label: "BiBCode Server",
      confidence: "exact",
    },
    {
      processKey: "200:1",
      pid: 200,
      ppid: 100,
      pgid: Option.none(),
      status: "Run",
      cpuPercent: 1,
      rssBytes: 36_300_000,
      elapsed: "00:00:01",
      command: "codex app-server",
      depth: 1,
      childPids: [],
      scope: "external",
      kind: "provider",
      label: "Codex",
      confidence: "exact",
    },
  ],
  error: Option.none(),
};

describe("AppStatusBarView", () => {
  it("renders the combined headline, parallel scope cards, consumers, and local Core separately", () => {
    const markup = renderToStaticMarkup(
      <AppStatusBarView
        usage={usage}
        diagnostics={{ diagnostics, queryError: null }}
        localDiagnostics={{
          diagnostics: {
            ...diagnostics,
            totals: {
              combined: { processCount: 2, rssBytes: 141_157_600, cpuPercent: 1.5 },
              core: { processCount: 1, rssBytes: 104_857_600, cpuPercent: 0.5 },
              external: { processCount: 1, rssBytes: 36_300_000, cpuPercent: 1 },
            },
            uiCoverage: {
              status: "partial",
              message: Option.some("One local UI process could not be sampled."),
            },
          },
          queryError: null,
        }}
        terminalCount={11}
        onRefresh={() => {}}
      />,
    );

    expect(markup).toContain("Claude");
    expect(markup).toContain("Session: 100% remaining");
    expect(markup).toContain("Weekly: 44% remaining");
    expect(markup).toContain("Codex");
    expect(markup).toContain("Session: 89% remaining");
    expect(markup).toContain("Weekly: 17% remaining");
    expect(markup).toContain("702.2 MB");
    expect(markup).toContain("11");
    expect(markup).toContain('aria-label="Combined monitored resources');
    expect(markup).toContain('title="Combined monitored resources');
    expect(markup).toContain("4.2% CPU");
    expect(markup).toContain("2 processes");
    expect(markup).toContain("BiBCode Core");
    expect(markup).toContain("External Tooling");
    expect(markup).toContain("Highest consumers");
    expect(markup).toContain("BiBCode Server");
    expect(markup).toContain("bibcode server");
    expect(markup).toContain("Core");
    expect(markup).toContain("Codex");
    expect(markup).toContain("codex app-server");
    expect(markup).toContain("External");
    expect(markup).toContain("This device");
    expect(markup).toContain("100.0 MB");
    expect(markup).toContain("UI coverage partial");
    expect(markup).toContain("grid-cols-2");

    expect(markup.indexOf("BiBCode Core")).toBeLessThan(markup.indexOf("Highest consumers"));
    expect(markup.indexOf("Highest consumers")).toBeLessThan(markup.indexOf("This device"));
  });

  it("shows the profiler marker only while an environment records", () => {
    const view = (
      <AppStatusBarView
        usage={null}
        diagnostics={{ diagnostics: null, queryError: null }}
        localDiagnostics={null}
        terminalCount={0}
        onRefresh={() => {}}
      />
    );
    expect(renderToStaticMarkup(view)).not.toContain("Profiler");

    profilerEnvironments.list = [
      {
        environmentId: "primary",
        label: "Local",
        connection: { phase: "connected" },
        serverConfig: {
          environment: { capabilities: { profiler: true } },
          settings: { enableProfiler: true },
        },
      },
    ];
    try {
      expect(renderToStaticMarkup(view)).toContain(
        'aria-label="Profiler recording, open diagnostics settings"',
      );
    } finally {
      profilerEnvironments.list = [];
    }
  });

  it("renders unavailable marks instead of healthy zeroes without a sample", () => {
    const markup = renderToStaticMarkup(
      <AppStatusBarView
        usage={null}
        diagnostics={{ diagnostics: null, queryError: null }}
        localDiagnostics={null}
        terminalCount={0}
        onRefresh={() => {}}
      />,
    );

    expect(markup).toContain("Combined monitored resources unavailable");
    expect(markup).toContain("Unavailable");
    expect(markup).not.toContain("0 B");
    expect(markup).not.toContain("0.0%");
  });

  it("adds selected stale state to the trigger title and accessible label", () => {
    const markup = renderToStaticMarkup(
      <AppStatusBarView
        usage={null}
        diagnostics={{
          diagnostics,
          queryError: "Selected diagnostics connection was lost.",
        }}
        localDiagnostics={null}
        terminalCount={0}
        onRefresh={() => {}}
      />,
    );

    expect(markup).toContain(
      'aria-label="Combined monitored resources: 702.2 MB memory, 4.2% CPU, 2 processes; 0 terminals; Showing stale resource data"',
    );
    expect(markup).toContain(
      'title="Combined monitored resources: 702.2 MB memory · 4.2% CPU · 2 processes · Showing stale resource data"',
    );
  });

  it("keeps local-only warning copy out of the selected trigger", () => {
    const markup = renderToStaticMarkup(
      <AppStatusBarView
        usage={null}
        diagnostics={{ diagnostics, queryError: null }}
        localDiagnostics={{
          diagnostics: null,
          queryError: "This device diagnostics request failed.",
        }}
        terminalCount={0}
        onRefresh={() => {}}
      />,
    );

    const trigger = markup.slice(markup.indexOf("<button"), markup.indexOf("</button>"));
    expect(trigger).not.toContain("This device diagnostics request failed.");
    expect(trigger).not.toContain("Resource data unavailable");
    expect(markup).toContain("This device");
    expect(markup).toContain("Unavailable");
    expect(markup).toContain("This device diagnostics request failed.");
  });

  it("refreshes provider usage and selected/local live diagnostics", async () => {
    const refreshProviderUsage = vi.fn().mockResolvedValue({ _tag: "Success" });
    const refreshUsageQuery = vi.fn();
    const refreshProcessDiagnostics = vi.fn();
    const refreshLocalProcessDiagnostics = vi.fn();
    const refreshResourceHistory = vi.fn();
    const environmentId = EnvironmentId.make("environment-test");
    const input = {
      environmentId,
      refreshProviderUsage,
      refreshUsageQuery,
      refreshProcessDiagnostics,
      refreshLocalProcessDiagnostics,
      refreshResourceHistory,
    };
    const handler = createStatusBarRefreshHandler(input);

    await handler(true);
    await handler(false);

    expect(refreshProviderUsage).toHaveBeenNthCalledWith(1, {
      environmentId,
      input: { providers: ["claude", "codex"], force: true },
    });
    expect(refreshProviderUsage).toHaveBeenNthCalledWith(2, {
      environmentId,
      input: { providers: ["claude", "codex"], force: false },
    });
    expect(refreshUsageQuery).toHaveBeenCalledTimes(2);
    expect(refreshProcessDiagnostics).toHaveBeenCalledTimes(2);
    expect(refreshLocalProcessDiagnostics).toHaveBeenCalledTimes(2);
    expect(refreshResourceHistory).not.toHaveBeenCalled();
  });

  it("reads provider usage after the native refresh finishes", async () => {
    let finishProviderRefresh: () => void = () => {
      throw new Error("Provider refresh resolver was not initialized.");
    };
    const refreshProviderUsage = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          finishProviderRefresh = resolve;
        }),
    );
    const refreshUsageQuery = vi.fn();
    const input = {
      environmentId: EnvironmentId.make("environment-test"),
      refreshProviderUsage,
      refreshUsageQuery,
      refreshProcessDiagnostics: vi.fn(),
      refreshLocalProcessDiagnostics: null,
      refreshResourceHistory: vi.fn(),
    };
    const handler = createStatusBarRefreshHandler(input);

    const pending = handler(false);
    expect(refreshProviderUsage).toHaveBeenCalledTimes(1);
    expect(refreshUsageQuery).not.toHaveBeenCalled();

    finishProviderRefresh();
    await pending;
    expect(refreshUsageQuery).toHaveBeenCalledTimes(1);
  });

  it("refreshes resource queries even when provider refresh fails", async () => {
    const refreshUsageQuery = vi.fn();
    const refreshProcessDiagnostics = vi.fn();
    const refreshLocalProcessDiagnostics = vi.fn();
    const refreshResourceHistory = vi.fn();
    const input = {
      environmentId: EnvironmentId.make("environment-test"),
      refreshProviderUsage: vi.fn().mockRejectedValue(new Error("provider unavailable")),
      refreshUsageQuery,
      refreshProcessDiagnostics,
      refreshLocalProcessDiagnostics,
      refreshResourceHistory,
    };
    const handler = createStatusBarRefreshHandler(input);

    await expect(handler(false)).rejects.toThrow("provider unavailable");
    expect(refreshUsageQuery).toHaveBeenCalledTimes(1);
    expect(refreshProcessDiagnostics).toHaveBeenCalledTimes(1);
    expect(refreshLocalProcessDiagnostics).toHaveBeenCalledTimes(1);
    expect(refreshResourceHistory).not.toHaveBeenCalled();
  });

  it("builds an environment-scoped live-resource refresh handler", () => {
    const refreshProcessDiagnostics = vi.fn();
    const refreshLocalProcessDiagnostics = vi.fn();
    const refreshResourceHistory = vi.fn();
    const input = {
      environmentId: EnvironmentId.make("environment-test"),
      refreshProcessDiagnostics,
      refreshLocalProcessDiagnostics,
      refreshResourceHistory,
    };
    const handler = createStatusBarResourceRefreshHandler(input);

    handler();

    expect(refreshProcessDiagnostics).toHaveBeenCalledTimes(1);
    expect(refreshLocalProcessDiagnostics).toHaveBeenCalledTimes(1);
    expect(refreshResourceHistory).not.toHaveBeenCalled();
  });
});

describe("selectPrimaryLocalEnvironmentId", () => {
  it("selects the desktop primary local environment only for a different selected host", () => {
    const selector = (
      environmentSelectors as typeof environmentSelectors & {
        selectPrimaryLocalEnvironmentId?: (input: {
          readonly client: "desktop" | "web";
          readonly selectedEnvironmentId: EnvironmentId | null;
        }) => EnvironmentId | null;
      }
    ).selectPrimaryLocalEnvironmentId;
    expect(selector).toBeTypeOf("function");
    if (!selector) return;

    const primaryLocalId = EnvironmentId.make(PRIMARY_LOCAL_ENVIRONMENT_ID);
    const remoteId = EnvironmentId.make("remote");
    expect(selector({ client: "desktop", selectedEnvironmentId: remoteId })).toBe(primaryLocalId);
    expect(selector({ client: "desktop", selectedEnvironmentId: primaryLocalId })).toBeNull();
    expect(selector({ client: "desktop", selectedEnvironmentId: null })).toBeNull();
    expect(selector({ client: "web", selectedEnvironmentId: remoteId })).toBeNull();
  });
});

describe("startStatusBarUsageAutoRefresh", () => {
  it("refreshes immediately and every 30 seconds until cleanup", () => {
    vi.useFakeTimers();
    try {
      const refresh = vi.fn();
      const cleanup = startStatusBarUsageAutoRefresh({ refresh });

      expect(refresh).toHaveBeenCalledTimes(1);

      vi.advanceTimersByTime(STATUS_BAR_USAGE_REFRESH_INTERVAL_MS - 1);
      expect(refresh).toHaveBeenCalledTimes(1);

      vi.advanceTimersByTime(1);
      expect(refresh).toHaveBeenCalledTimes(2);

      vi.advanceTimersByTime(STATUS_BAR_USAGE_REFRESH_INTERVAL_MS);
      expect(refresh).toHaveBeenCalledTimes(3);

      cleanup();
      vi.advanceTimersByTime(STATUS_BAR_USAGE_REFRESH_INTERVAL_MS);
      expect(refresh).toHaveBeenCalledTimes(3);
    } finally {
      vi.useRealTimers();
    }
  });

  it("supports the two-second resource refresh cadence", () => {
    vi.useFakeTimers();
    try {
      const refresh = vi.fn();
      const cleanup = startStatusBarUsageAutoRefresh({
        refresh,
        intervalMs: STATUS_BAR_RESOURCE_REFRESH_INTERVAL_MS,
      });

      expect(refresh).toHaveBeenCalledTimes(1);
      vi.advanceTimersByTime(STATUS_BAR_RESOURCE_REFRESH_INTERVAL_MS);
      expect(refresh).toHaveBeenCalledTimes(2);

      cleanup();
    } finally {
      vi.useRealTimers();
    }
  });
});
