// @vitest-environment happy-dom

import type { ProfilerFinding, ProfilerStatus } from "@bibcode/contracts";
import { EnvironmentId } from "@bibcode/contracts";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

type AnyProps = Record<string, unknown>;

const h = vi.hoisted(() => ({
  desktop: true,
  visible: true,
  settingEnabled: false,
  environment: null as { environmentId: string; label: string } | null,
  observability: null as { logsDirectoryPath?: string | null } | null,
  preferredEditor: "vscode" as string | null,
  statusQuery: null as unknown as {
    data: unknown;
    error: string | null;
    isPending: boolean;
    refresh: () => void;
    revalidate: () => void;
  },
  updateSettings: vi.fn(),
  openInEditorCalls: [] as unknown[],
  openInEditorResult: { _tag: "Success" } as { _tag: string; error?: unknown },
  downloadCalls: [] as Array<{ frontendLog: string; options: unknown }>,
  downloadError: null as unknown,
  toasts: [] as unknown[],
}));

vi.mock("~/env", () => ({
  get isDesktopHost() {
    return h.desktop;
  },
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: (atom: { __atom?: string } | null | undefined) => {
    if (atom?.__atom === "observability") return h.observability;
    if (atom?.__atom === "editors") return ["vscode"];
    return undefined;
  },
}));

vi.mock("@bibcode/client-runtime/state/runtime", () => ({
  isAtomCommandInterrupted: (result: { _tag?: string }) => result._tag === "Interrupted",
  squashAtomCommandFailure: (result: { error?: unknown }) => result.error,
}));

vi.mock("../../hooks/useSettings", () => ({
  usePrimarySettings: (select: (settings: { enableProfiler: boolean }) => unknown) =>
    select({ enableProfiler: h.settingEnabled }),
  useUpdatePrimarySettings: () => h.updateSettings,
}));

vi.mock("../../state/environments", () => ({
  usePrimaryEnvironment: () => h.environment,
}));

vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (atom: { __q?: string } | null) =>
    atom === null
      ? { data: null, error: null, isPending: false, refresh: vi.fn(), revalidate: vi.fn() }
      : h.statusQuery,
}));

vi.mock("../../state/server", () => ({
  serverEnvironment: {
    profilerStatus: (args: unknown) => ({ __q: "profiler", args }),
  },
  primaryServerAvailableEditorsAtom: { __atom: "editors" },
  primaryServerObservabilityAtom: { __atom: "observability" },
}));

vi.mock("../../state/shell", () => ({
  shellEnvironment: { openInEditor: { __cmd: "openInEditor" } },
}));

vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: () => (input: unknown) => {
    h.openInEditorCalls.push(input);
    return Promise.resolve(h.openInEditorResult);
  },
}));

vi.mock("../../editorPreferences", () => ({
  resolveAndPersistPreferredEditor: () => h.preferredEditor,
}));

vi.mock("../../timestampFormat", () => ({
  formatRelativeTimeLabel: (iso: string) => `rel(${iso})`,
}));

vi.mock("../../diagnostics/frontendLogCapture", () => ({
  readFrontendLogSnapshot: () => "frontend snapshot\n",
}));

vi.mock("../../diagnostics/downloadDiagnosticLogs", () => ({
  downloadDiagnosticLogs: async (frontendLog: string, options: unknown) => {
    h.downloadCalls.push({ frontendLog, options });
    if (h.downloadError !== null) throw h.downloadError;
    return { status: "downloaded", filename: "bundle.zip" };
  },
}));

vi.mock("../pullRequests/useDocumentVisible", () => ({
  useDocumentVisible: () => h.visible,
}));

vi.mock("../ui/toast", () => ({
  toastManager: { add: (toast: unknown) => h.toasts.push(toast) },
}));

vi.mock("../ui/tooltip", () => ({
  Tooltip: (props: AnyProps) => <>{props.children as ReactNode}</>,
  TooltipTrigger: (props: AnyProps) => {
    const render = props.render as { props: AnyProps; type: string } | undefined;
    return (
      <span data-tooltip-trigger {...(render?.props ?? {})}>
        {props.children as ReactNode}
      </span>
    );
  },
  TooltipPopup: (props: AnyProps) => <div data-tooltip-popup>{props.children as ReactNode}</div>,
}));

vi.mock("./settingsLayout", () => ({
  SettingsSection: (props: AnyProps) => (
    <section aria-label={props.title as string}>{props.children as ReactNode}</section>
  ),
  SettingsRow: (props: AnyProps) => (
    <div data-settings-row>
      <h3>{props.title as ReactNode}</h3>
      <p>{props.description as ReactNode}</p>
      <div data-status>{props.status as ReactNode}</div>
      {props.control as ReactNode}
      {props.children as ReactNode}
    </div>
  ),
}));

vi.mock("../ui/switch", () => ({
  Switch: (props: AnyProps) => (
    <button
      type="button"
      role="switch"
      aria-label={props["aria-label"] as string}
      aria-checked={Boolean(props.checked)}
      onClick={() => (props.onCheckedChange as (checked: boolean) => void)(!props.checked)}
    />
  ),
}));

vi.mock("../ui/checkbox", () => ({
  Checkbox: (props: AnyProps) => (
    <input
      type="checkbox"
      checked={Boolean(props.checked)}
      onChange={(event) =>
        (props.onCheckedChange as (checked: boolean) => void)(event.currentTarget.checked)
      }
    />
  ),
}));

vi.mock("../ui/button", () => ({
  Button: (props: AnyProps) => (
    <button
      type="button"
      disabled={Boolean(props.disabled)}
      onClick={props.onClick as () => void}
      aria-describedby={props["aria-describedby"] as string | undefined}
    >
      {props.children as ReactNode}
    </button>
  ),
}));

import { ProfilerSection } from "./ProfilerSection";

const mounted: Array<{ root: Root; container: HTMLDivElement }> = [];

const ENVIRONMENT_ID = EnvironmentId.make("environment-1");

function finding(overrides: Partial<ProfilerFinding> = {}): ProfilerFinding {
  return {
    id: "f-1",
    fingerprint: "0123456789ab",
    kind: "rpc.slow",
    severity: "warn",
    firstSeen: "2026-10-09T12:00:00.000Z",
    lastSeen: "2026-10-09T12:01:00.000Z",
    count: 1,
    source: "server",
    title: "orchestration.dispatch took 12.3 s",
    summary: "server 11.9 s",
    state: "open",
    ...overrides,
  };
}

function status(overrides: Partial<ProfilerStatus> = {}): ProfilerStatus {
  return {
    enabled: true,
    recordingSince: "2026-10-09T11:00:00.000Z",
    diskBytes: 3 * 1024 * 1024,
    eventCount: 1234,
    droppedEvents: 0,
    lastCrashAt: null,
    capabilities: { nativeDumps: "available", osCrashRecords: "available", storage: "ok" },
    findings: [],
    ...overrides,
  };
}

function queryOf(data: ProfilerStatus | null, overrides: Partial<typeof h.statusQuery> = {}) {
  h.statusQuery = {
    data,
    error: null,
    isPending: false,
    refresh: vi.fn(),
    revalidate: vi.fn(),
    ...overrides,
  };
}

async function render(): Promise<HTMLDivElement> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  await act(async () => {
    root.render(<ProfilerSection />);
  });
  return container;
}

function buttonByText(container: HTMLElement, text: string): HTMLButtonElement {
  const found = [...container.querySelectorAll("button")].find((button) =>
    button.textContent?.includes(text),
  );
  if (!found) throw new Error(`No button containing "${text}"`);
  return found;
}

async function click(element: HTMLElement) {
  await act(async () => {
    element.click();
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.desktop = true;
  h.visible = true;
  h.settingEnabled = false;
  h.environment = { environmentId: ENVIRONMENT_ID, label: "Work Server" };
  h.observability = { logsDirectoryPath: "/state/logs" };
  h.preferredEditor = "vscode";
  h.updateSettings.mockReset();
  h.updateSettings.mockResolvedValue({ _tag: "Success" });
  h.openInEditorCalls.length = 0;
  h.openInEditorResult = { _tag: "Success" };
  h.downloadCalls.length = 0;
  h.downloadError = null;
  h.toasts.length = 0;
  queryOf(status({ enabled: false, recordingSince: null }));
});

afterEach(async () => {
  for (const entry of mounted.splice(0)) {
    await act(async () => entry.root.unmount());
    entry.container.remove();
  }
  vi.useRealTimers();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("ProfilerSection", () => {
  it("shows the off state and turns the profiler on from the toggle", async () => {
    const container = await render();

    const toggle = container.querySelector('[role="switch"]')!;
    expect(toggle.getAttribute("aria-label")).toBe("Record profiling data");
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    expect(container.textContent).toContain(
      "Records crashes, memory and CPU trends, slow requests and worktree cleanup problems for this environment. Data stays on this machine. Uses about 1% CPU.",
    );
    expect(container.textContent).toContain(
      "Off. Turn on to start recording. Crash checks for the previous run still happen.",
    );

    await click(toggle as HTMLElement);

    expect(h.updateSettings).toHaveBeenCalledWith({ enableProfiler: true });
    // The status is re-read right after the setting is saved.
    expect(h.statusQuery.revalidate).toHaveBeenCalledTimes(1);
  });

  it("shows the recording line, counts and the newest findings", async () => {
    h.settingEnabled = true;
    queryOf(
      status({
        droppedEvents: 7,
        findings: [
          finding({ id: "a", title: "Older finding", lastSeen: "2026-10-09T12:00:00.000Z" }),
          finding({
            id: "b",
            title: "Newest finding",
            count: 3,
            severity: "error",
            lastSeen: "2026-10-09T12:05:00.000Z",
          }),
        ],
      }),
    );

    const container = await render();

    expect(container.querySelector('[role="switch"]')!.getAttribute("aria-checked")).toBe("true");
    expect(container.textContent).toContain(
      "Recording since rel(2026-10-09T11:00:00.000Z) · 3 MB · 1,234 events · 7 dropped",
    );
    const titles = [...container.querySelectorAll("li")].map((item) => item.textContent ?? "");
    expect(titles).toHaveLength(2);
    expect(titles[0]).toContain("Newest finding");
    expect(titles[0]).toContain("×3");
    expect(titles[0]).toContain("rel(2026-10-09T12:05:00.000Z)");
    expect(titles[1]).toContain("Older finding");
  });

  it("says the profiler is starting until the server reports it recording", async () => {
    h.settingEnabled = true;
    queryOf(status({ enabled: false, recordingSince: null }));
    const starting = await render();
    expect(starting.textContent).toContain("Starting...");
    expect(starting.textContent).not.toContain("Off. Turn on");

    queryOf(null);
    const loading = await render();
    expect(loading.textContent).toContain("Checking...");
  });

  it("leaves the dropped count out when nothing was dropped", async () => {
    h.settingEnabled = true;
    queryOf(status({ droppedEvents: 0 }));

    const container = await render();

    expect(container.textContent).toContain("1,234 events");
    expect(container.textContent).not.toContain("dropped");
  });

  it("shows the empty state and keeps only the newest 20 findings", async () => {
    h.settingEnabled = true;
    const container = await render();
    expect(container.textContent).toContain("No findings yet.");

    queryOf(
      status({
        findings: Array.from({ length: 25 }, (_, index) =>
          finding({
            id: `f-${index}`,
            title: `Finding ${index}`,
            lastSeen: new Date(Date.UTC(2026, 9, 9, 12, index)).toISOString(),
          }),
        ),
      }),
    );
    const full = await render();
    const items = [...full.querySelectorAll("li")];
    expect(items).toHaveLength(20);
    expect(items[0]!.textContent).toContain("Finding 24");
    expect(items[19]!.textContent).toContain("Finding 5");
  });

  it("explains why Open folder is disabled for a remote environment", async () => {
    h.desktop = false;

    const container = await render();

    expect(buttonByText(container, "Open folder").disabled).toBe(true);
    expect(container.querySelector("[data-tooltip-popup]")!.textContent).toBe(
      "The profiler folder is on Work Server. Use Export bundle or bibcode profiler on that machine.",
    );
    expect(h.openInEditorCalls).toHaveLength(0);
  });

  it("opens the profiler folder next to the logs folder on desktop", async () => {
    const container = await render();

    expect(buttonByText(container, "Open folder").disabled).toBe(false);
    await click(buttonByText(container, "Open folder"));

    expect(h.openInEditorCalls).toEqual([
      { environmentId: ENVIRONMENT_ID, input: { cwd: "/state/profiler", editor: "vscode" } },
    ]);
  });

  it("derives the profiler folder from a Windows logs path", async () => {
    h.observability = { logsDirectoryPath: "C:\\Users\\me\\.bibcode\\userdata\\logs" };

    const container = await render();
    await click(buttonByText(container, "Open folder"));

    expect(h.openInEditorCalls).toEqual([
      {
        environmentId: ENVIRONMENT_ID,
        input: { cwd: "C:\\Users\\me\\.bibcode\\userdata\\profiler", editor: "vscode" },
      },
    ]);
  });

  it("disables Open folder when the profiler folder cannot be derived", async () => {
    h.observability = { logsDirectoryPath: "/state/custom-logging" };

    const container = await render();

    expect(buttonByText(container, "Open folder").disabled).toBe(true);
  });

  it("shows why the folder could not be opened", async () => {
    h.openInEditorResult = { _tag: "Failure", error: new Error("editor missing") };

    const container = await render();
    await click(buttonByText(container, "Open folder"));

    expect(container.textContent).toContain("editor missing");
  });

  it("exports the bundle without crash dumps by default", async () => {
    const container = await render();

    await click(buttonByText(container, "Export bundle"));

    expect(h.downloadCalls).toEqual([
      { frontendLog: "frontend snapshot\n", options: { includeCrashDumps: false } },
    ]);
    expect(h.toasts).toContainEqual(expect.objectContaining({ type: "success" }));
  });

  it("exports crash dumps only after the checkbox is checked", async () => {
    const container = await render();
    expect(container.textContent).toContain(
      "Crash dumps can contain fragments of memory, including prompts or tokens. Only share them with people you trust.",
    );
    const checkbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(checkbox.checked).toBe(false);
    expect(checkbox.closest("label")!.textContent).toContain("Include crash dumps");

    await click(checkbox);
    await click(buttonByText(container, "Export bundle"));

    expect(h.downloadCalls).toEqual([
      { frontendLog: "frontend snapshot\n", options: { includeCrashDumps: true } },
    ]);
  });

  it("warns when the profiler cannot write its data", async () => {
    h.settingEnabled = true;
    queryOf(
      status({
        capabilities: { nativeDumps: "available", osCrashRecords: "available", storage: "failing" },
      }),
    );

    const container = await render();

    expect(container.textContent).toContain(
      "Could not write profiling data. Check free disk space and folder permissions.",
    );
  });

  it("shows the status error with a way to retry instead of an empty list", async () => {
    queryOf(null, { error: "The connection dropped before the result arrived." });

    const container = await render();

    expect(container.textContent).toContain("The connection dropped before the result arrived.");
    expect(container.textContent).not.toContain("No findings yet.");

    await click(buttonByText(container, "Retry"));

    expect(h.statusQuery.refresh).toHaveBeenCalledTimes(1);
  });

  describe("status refresh", () => {
    it("re-reads the status every 5 seconds while recording and the page is visible", async () => {
      vi.useFakeTimers();
      h.settingEnabled = true;
      queryOf(status());

      await render();
      expect(h.statusQuery.revalidate).not.toHaveBeenCalled();

      await act(async () => {
        vi.advanceTimersByTime(15_000);
      });

      expect(h.statusQuery.revalidate).toHaveBeenCalledTimes(3);
    });

    it("leaves a status read that is still in flight alone", async () => {
      vi.useFakeTimers();
      h.settingEnabled = true;
      queryOf(status(), { isPending: true });

      await render();
      await act(async () => {
        vi.advanceTimersByTime(30_000);
      });

      expect(h.statusQuery.revalidate).not.toHaveBeenCalled();
    });

    it("does not poll while the page is hidden or the profiler is off", async () => {
      vi.useFakeTimers();
      h.settingEnabled = true;
      h.visible = false;
      queryOf(status());
      await render();
      await act(async () => {
        vi.advanceTimersByTime(30_000);
      });
      expect(h.statusQuery.revalidate).not.toHaveBeenCalled();

      h.visible = true;
      h.settingEnabled = false;
      queryOf(status({ enabled: false }));
      await render();
      await act(async () => {
        vi.advanceTimersByTime(30_000);
      });
      expect(h.statusQuery.revalidate).not.toHaveBeenCalled();
    });
  });
});
