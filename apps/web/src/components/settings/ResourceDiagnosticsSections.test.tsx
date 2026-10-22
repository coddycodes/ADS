import type { ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type {
  ServerProcessDiagnosticsEntry,
  ServerProcessDiagnosticsResult,
  ServerProcessResourceHistoryResult,
  ServerProcessResourceHistorySummary,
} from "@bibcode/contracts";
import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

type AnyProps = Record<string, unknown>;

const h = vi.hoisted(() => ({
  stateSeeds: [] as Array<{ match: (initial: unknown) => boolean; value: unknown }>,
  stateCalls: [] as Array<unknown>,
  buttons: [] as Array<Record<string, unknown>>,
  reset() {
    h.stateSeeds.length = 0;
    h.stateCalls.length = 0;
    h.buttons.length = 0;
  },
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  const useState = (initial?: unknown) => {
    const resolved = typeof initial === "function" ? (initial as () => unknown)() : initial;
    const seedIndex = h.stateSeeds.findIndex((seed) => seed.match(resolved));
    const value = seedIndex >= 0 ? h.stateSeeds.splice(seedIndex, 1)[0]!.value : resolved;
    const setValue = (next: unknown) => {
      h.stateCalls.push(
        typeof next === "function" ? (next as (value: unknown) => unknown)(value) : next,
      );
    };
    return [value, setValue];
  };
  return { ...actual, useState: useState as typeof actual.useState };
});

vi.mock("./settingsLayout", () => ({
  SettingsSection: (props: AnyProps) => (
    <section data-section-title={props.title as string}>
      {props.headerAction as ReactNode}
      {props.children as ReactNode}
    </section>
  ),
}));

vi.mock("../ui/scroll-area", () => ({
  ScrollArea: (props: AnyProps) => <div data-scroll-area>{props.children as ReactNode}</div>,
}));

vi.mock("../ui/button", () => ({
  Button: (props: AnyProps) => {
    h.buttons.push(props);
    return (
      <button
        type="button"
        aria-label={props["aria-label"] as string | undefined}
        aria-pressed={props["aria-pressed"] as boolean | undefined}
        disabled={Boolean(props.disabled)}
      >
        {props.children as ReactNode}
      </button>
    );
  },
}));

vi.mock("../ui/tooltip", () => ({
  Tooltip: (props: AnyProps) => <>{props.children as ReactNode}</>,
  TooltipTrigger: (props: AnyProps) => props.render as ReactNode,
  TooltipPopup: (props: AnyProps) => <span data-tooltip>{props.children as ReactNode}</span>,
}));

const T0 = DateTime.makeUnsafe("2026-07-19T15:00:00.000Z");
const T1 = DateTime.makeUnsafe("2026-07-19T15:01:00.000Z");

function processEntry(
  patch: Partial<ServerProcessDiagnosticsEntry> &
    Pick<ServerProcessDiagnosticsEntry, "pid" | "processKey" | "scope" | "kind" | "label">,
): ServerProcessDiagnosticsEntry {
  return {
    ppid: 100,
    pgid: Option.none(),
    status: "Run",
    cpuPercent: 1,
    rssBytes: 10 * 1024 ** 2,
    elapsed: "00:00:01",
    command: `command-for-${patch.pid}`,
    depth: 1,
    childPids: [],
    confidence: "exact",
    ...patch,
  };
}

function liveFixture(
  patch: Partial<ServerProcessDiagnosticsResult> = {},
): ServerProcessDiagnosticsResult {
  return {
    serverPid: 100,
    readAt: T0,
    totals: {
      combined: { processCount: 4, rssBytes: 650 * 1024 ** 2, cpuPercent: 16 },
      core: { processCount: 2, rssBytes: 320 * 1024 ** 2, cpuPercent: 3 },
      external: { processCount: 2, rssBytes: 330 * 1024 ** 2, cpuPercent: 13 },
    },
    uiCoverage: { status: "available", message: Option.none() },
    processes: [
      processEntry({
        pid: 100,
        ppid: 1,
        processKey: "100:1",
        scope: "core",
        kind: "server",
        label: "BiBCode Server",
        command: "bibcode server",
        rssBytes: 200 * 1024 ** 2,
        cpuPercent: 2,
        depth: 0,
        childPids: [200, 300],
      }),
      processEntry({
        pid: 200,
        processKey: "200:1",
        scope: "external",
        kind: "provider",
        label: "Codex",
        command: "codex app-server",
        rssBytes: 330 * 1024 ** 2,
        cpuPercent: 13,
      }),
      processEntry({
        pid: 300,
        processKey: "300:1",
        scope: "core",
        kind: "ui",
        label: "BiBCode UI",
        command: "BiBCode WebContent",
        rssBytes: 120 * 1024 ** 2,
      }),
      processEntry({
        pid: 400,
        ppid: 1,
        processKey: "400:1",
        scope: "external",
        kind: "helper",
        label: "Detached helper",
        command: "helper --serve",
        rssBytes: 1,
        depth: 0,
      }),
    ],
    error: Option.none(),
    ...patch,
  };
}

function historySummary(
  patch: Partial<ServerProcessResourceHistorySummary> = {},
): ServerProcessResourceHistorySummary {
  return {
    processKey: "100:1",
    pid: 100,
    ppid: 1,
    command: "bibcode server",
    depth: 0,
    scope: "core",
    kind: "server",
    label: "BiBCode Server",
    confidence: "exact",
    firstSeenAt: T0,
    lastSeenAt: T1,
    currentCpuPercent: 2,
    avgCpuPercent: 1.5,
    maxCpuPercent: 8,
    cpuSecondsApprox: 65,
    currentRssBytes: 100 * 1024 ** 2,
    maxRssBytes: 200 * 1024 ** 2,
    sampleCount: 10,
    ...patch,
  };
}

function historyFixture(
  patch: Partial<ServerProcessResourceHistoryResult> = {},
): ServerProcessResourceHistoryResult {
  return {
    readAt: T1,
    windowMs: 15 * 60_000,
    bucketMs: 60_000,
    sampleIntervalMs: 2_000,
    retainedSampleCount: 120,
    cpuSecondsApprox: { combined: 75, core: 65, external: 10 },
    uiCoverage: { status: "available", message: Option.none() },
    buckets: [
      {
        startedAt: T0,
        endedAt: T1,
        cpuPercent: {
          average: { combined: 10, core: 6, external: 4 },
          peak: { combined: 40, core: 25, external: 15 },
        },
        rssBytes: {
          average: {
            combined: 800 * 1024 ** 2,
            core: 500 * 1024 ** 2,
            external: 300 * 1024 ** 2,
          },
          peak: {
            combined: 1024 * 1024 ** 2,
            core: 600 * 1024 ** 2,
            external: 424 * 1024 ** 2,
          },
        },
        maxProcessCount: { combined: 4, core: 2, external: 2 },
      },
    ],
    processes: [
      historySummary(),
      historySummary({
        processKey: "200:1",
        pid: 200,
        ppid: 100,
        scope: "external",
        kind: "provider",
        label: "Codex",
        command: "codex app-server",
        depth: 1,
        cpuSecondsApprox: 10,
        currentCpuPercent: 13,
        avgCpuPercent: 7,
        maxCpuPercent: 40,
        currentRssBytes: 330 * 1024 ** 2,
        maxRssBytes: 400 * 1024 ** 2,
      }),
    ],
    error: Option.none(),
    ...patch,
  };
}

import { ResourceDiagnosticsSections } from "./ResourceDiagnosticsSections";

function render(
  options: {
    readonly processData?: ServerProcessDiagnosticsResult | null;
    readonly processError?: string | null;
    readonly isProcessPending?: boolean;
    readonly resourceData?: ServerProcessResourceHistoryResult | null;
    readonly resourceError?: string | null;
    readonly isResourcePending?: boolean;
    readonly supportsInterrupt?: boolean;
    readonly onSignal?: (pid: number, processKey: string, signal: "SIGINT" | "SIGKILL") => void;
    readonly onSelectResourceWindow?: (windowMs: number) => void;
  } = {},
): string {
  h.buttons.length = 0;
  h.stateCalls.length = 0;
  return renderToStaticMarkup(
    <ResourceDiagnosticsSections
      processData={options.processData === undefined ? liveFixture() : options.processData}
      processError={options.processError ?? null}
      isProcessPending={options.isProcessPending ?? false}
      signalingPid={null}
      supportsInterrupt={options.supportsInterrupt ?? true}
      onSignal={options.onSignal ?? vi.fn()}
      liveHeaderAction={<button type="button">Refresh live</button>}
      resourceData={options.resourceData === undefined ? historyFixture() : options.resourceData}
      resourceError={options.resourceError ?? null}
      isResourcePending={options.isResourcePending ?? false}
      resourceWindowMs={15 * 60_000}
      onSelectResourceWindow={options.onSelectResourceWindow ?? vi.fn()}
      historyHeaderAction={<button type="button">Refresh history</button>}
    />,
  );
}

function captureButtons(markup: string): string {
  return markup;
}

beforeEach(() => {
  h.reset();
});

describe("ResourceDiagnosticsSections", () => {
  it("renders a Combined headline above equal BiBCode Core and External Tooling cards", () => {
    const markup = render();

    expect(markup).toContain('data-section-title="Live Processes"');
    expect(markup).toContain('data-section-title="Resource History"');
    expect(markup).toContain('data-resource-card="combined"');
    expect(markup).toContain('data-resource-card="core"');
    expect(markup).toContain('data-resource-card="external"');
    expect(markup.indexOf('data-resource-card="combined"')).toBeLessThan(
      markup.indexOf('data-resource-card="core"'),
    );
    expect(markup).toContain('data-resource-card-pair="true"');
    expect(markup).toContain("BiBCode Core");
    expect(markup).toContain("External Tooling");
    expect(markup).toContain("Server PID");
    // #159: the memory figure says what it measures on each server OS.
    expect(markup).toContain("physical footprint on macOS, as in Activity Monitor");
    expect(markup).toContain("resident memory on Linux and Windows");
  });

  it("renders memory-first rows and applies deterministic Name sorting after interaction", () => {
    let markup = render();
    expect(markup.indexOf(">Codex<")).toBeLessThan(markup.indexOf(">BiBCode Server<"));

    const sortByName = h.buttons.find((button) => button["aria-label"] === "Sort by Name");
    if (!sortByName) throw new Error("Sort by Name button was not rendered");
    (sortByName.onClick as () => void)();
    expect(h.stateCalls).toContainEqual({ key: "name", direction: "asc" });

    h.stateSeeds.push({
      match: (initial) =>
        typeof initial === "object" &&
        initial !== null &&
        "key" in initial &&
        (initial as { key: string }).key === "memory",
      value: { key: "name", direction: "asc" },
    });
    markup = render();
    expect(markup.indexOf(">BiBCode Server<")).toBeLessThan(markup.indexOf(">Codex<"));
    expect(markup.indexOf(">Codex<")).toBeLessThan(markup.indexOf(">Detached helper<"));
  });

  it("toggles the chart from stacked Memory averages to CPU averages", () => {
    let markup = render();
    expect(markup).toContain('aria-label="Memory history" aria-pressed="true"');
    expect(markup).toContain("Combined average 800.0 MB");
    expect(markup).toContain('data-history-stack="core"');
    expect(markup).toContain('data-history-stack="external"');

    const cpuToggle = h.buttons.find((button) => button["aria-label"] === "CPU history");
    if (!cpuToggle) throw new Error("CPU history button was not rendered");
    (cpuToggle.onClick as () => void)();
    expect(h.stateCalls).toContain("cpu");

    h.stateSeeds.push({ match: (initial) => initial === "memory", value: "cpu" });
    markup = render();
    expect(markup).toContain('aria-label="CPU history" aria-pressed="true"');
    expect(markup).toContain("Combined average 10.0%");
    expect(markup).toContain("Same-sample peak 40.0%");
  });

  it("preserves negative diagnostics while keeping chart dimensions nonnegative", () => {
    const bucket = historyFixture().buckets[0]!;
    h.stateSeeds.push({ match: (initial) => initial === "memory", value: "cpu" });

    const markup = render({
      resourceData: historyFixture({
        buckets: [
          {
            ...bucket,
            cpuPercent: {
              average: { combined: 10, core: -6, external: 16 },
              peak: { combined: 40, core: -5, external: 45 },
            },
          },
        ],
      }),
    });

    expect(markup).toContain(
      "Combined average 10.0%. Same-sample peak 40.0%: Core -5.0%, External 45.0%.",
    );
    const heights = [...markup.matchAll(/style="height:([^"]+)"/g)].map((match) =>
      Number.parseFloat(match[1] ?? ""),
    );
    expect(heights.length).toBeGreaterThan(0);
    for (const height of heights) {
      expect(Number.isFinite(height)).toBe(true);
      expect(height).toBeGreaterThanOrEqual(0);
    }
  });

  it("sorts complete history rows from accessible attributed metric headers", () => {
    let markup = render();
    let historyMarkup = markup.slice(markup.indexOf('data-section-title="Resource History"'));

    expect(historyMarkup.indexOf(">Codex<")).toBeLessThan(
      historyMarkup.indexOf(">BiBCode Server<"),
    );
    expect(historyMarkup).toMatch(
      /<th aria-sort="descending"[^>]*><button[^>]*aria-label="Sort history by Max Memory"/,
    );

    for (const label of [
      "Scope",
      "Kind",
      "Label",
      "CPU Time",
      "Current CPU",
      "Average CPU",
      "Peak CPU",
      "Max Memory",
    ]) {
      expect(h.buttons.some((button) => button["aria-label"] === `Sort history by ${label}`)).toBe(
        true,
      );
    }

    const sortByCpuTime = h.buttons.find(
      (button) => button["aria-label"] === "Sort history by CPU Time",
    );
    if (!sortByCpuTime) throw new Error("Sort history by CPU Time button was not rendered");
    (sortByCpuTime.onClick as () => void)();
    expect(h.stateCalls).toContainEqual({ key: "cpuTime", direction: "desc" });

    h.stateSeeds.push({
      match: (initial) =>
        typeof initial === "object" &&
        initial !== null &&
        "key" in initial &&
        (initial as { key: string }).key === "maxMemory",
      value: { key: "cpuTime", direction: "desc" },
    });
    markup = render();
    historyMarkup = markup.slice(markup.indexOf('data-section-title="Resource History"'));
    expect(historyMarkup.indexOf(">BiBCode Server<")).toBeLessThan(
      historyMarkup.indexOf(">Codex<"),
    );
    expect(historyMarkup).toMatch(
      /<th aria-sort="descending"[^>]*><button[^>]*aria-label="Sort history by CPU Time"/,
    );
  });

  it("renders bounded coverage warnings and keeps not-applicable coverage neutral", () => {
    let markup = render({
      processData: liveFixture({
        uiCoverage: {
          status: "partial",
          message: Option.some("Some UI processes could not be sampled."),
        },
      }),
      resourceData: historyFixture({
        uiCoverage: {
          status: "unavailable",
          message: Option.some("UI accounting is unavailable."),
        },
      }),
    });
    expect(markup).toContain("Partial UI coverage");
    expect(markup).toContain("UI coverage unavailable");
    expect(markup).toContain("Core and Combined totals omit unobserved UI resource usage");

    markup = render({
      processData: liveFixture({
        uiCoverage: { status: "notApplicable", message: Option.none() },
      }),
      resourceData: historyFixture({
        uiCoverage: { status: "notApplicable", message: Option.none() },
      }),
    });
    expect(markup).not.toContain("UI coverage unavailable");
    expect(markup).not.toContain("Partial UI coverage");
  });

  it("shows signal controls only for eligible current External descendants", () => {
    captureButtons(render());
    const signalLabels = h.buttons
      .map((button) => button["aria-label"])
      .filter(
        (label): label is string => typeof label === "string" && label.startsWith("Send SIG"),
      );

    expect(signalLabels).toEqual(["Send SIGINT to Codex", "Send SIGKILL to Codex"]);
    expect(signalLabels.some((label) => label.includes("BiBCode"))).toBe(false);
    expect(signalLabels.some((label) => label.includes("Detached"))).toBe(false);
  });

  it("renders full live and attributed history columns", () => {
    const markup = render();
    const liveMarkup = markup.slice(0, markup.indexOf('data-section-title="Resource History"'));

    for (const column of ["Scope", "Kind", "Label", "Command", "CPU", "Memory", "PID"]) {
      expect(liveMarkup).toContain(`>${column}<`);
    }
    for (const column of ["CPU Time", "Current CPU", "Average CPU", "Peak CPU", "Max Memory"]) {
      expect(markup).toContain(`>${column}<`);
    }
    expect(markup).not.toContain(">Current Memory<");
  });
});
