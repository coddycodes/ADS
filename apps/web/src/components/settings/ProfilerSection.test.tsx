// @vitest-environment happy-dom

import type { ProfilerFinding, ProfilerStatus } from "@bibcode/contracts";
import { EnvironmentId } from "@bibcode/contracts";
import { act, useState, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

type AnyProps = Record<string, unknown>;

interface FakeEnvironment {
  readonly environmentId: string;
  readonly label: string;
  readonly connection: { readonly phase: string };
}

const h = vi.hoisted(() => ({
  desktop: true,
  visible: true,
  settingsByEnvironment: {} as Record<string, boolean>,
  environment: null as FakeEnvironment | null,
  environments: [] as FakeEnvironment[],
  catalogReady: true,
  selections: [] as Array<EnvironmentId | null>,
  statusByEnvironment: {} as Record<string, unknown>,
  updates: [] as Array<{ environmentId: string; patch: unknown }>,
  selectProps: null as null | { onValueChange: (value: string) => void },
  observability: null as { logsDirectoryPath?: string | null } | null,
  preferredEditor: "vscode" as string | null,
  statusQuery: null as unknown as {
    data: unknown;
    error: string | null;
    isPending: boolean;
    refresh: () => void;
    revalidate: () => void;
  },
  openInEditorCalls: [] as unknown[],
  openInEditorResult: { _tag: "Success" } as { _tag: string; error?: unknown },
  downloadCalls: [] as Array<{ frontendLog: string; options: unknown }>,
  downloadError: null as unknown,
  downloadGate: null as Promise<void> | null,
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
  useEnvironmentSettings: (
    environmentId: string,
    select: (settings: { enableProfiler: boolean }) => unknown,
  ) => select({ enableProfiler: h.settingsByEnvironment[environmentId] ?? false }),
  useUpdateEnvironmentSettings: (environmentId: string) => (patch: unknown) => {
    h.updates.push({ environmentId, patch });
    return Promise.resolve({ _tag: "Success" });
  },
}));

vi.mock("../../state/environments", () => ({
  usePrimaryEnvironment: () => h.environment,
  useEnvironments: () => ({ environments: h.environments, isReady: h.catalogReady }),
}));

vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (atom: { __q?: string; args?: { environmentId: string } } | null) =>
    atom === null
      ? { data: null, error: null, isPending: false, refresh: vi.fn(), revalidate: vi.fn() }
      : ((atom.args && h.statusByEnvironment[atom.args.environmentId]) ?? h.statusQuery),
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
    if (h.downloadGate !== null) await h.downloadGate;
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

vi.mock("../ui/select", () => ({
  Select: (props: AnyProps) => {
    h.selectProps = props as unknown as { onValueChange: (value: string) => void };
    return <div data-select>{props.children as ReactNode}</div>;
  },
  SelectTrigger: (props: AnyProps) => (
    <button type="button" aria-label={props["aria-label"] as string}>
      {props.children as ReactNode}
    </button>
  ),
  SelectValue: (props: AnyProps) => <span>{props.children as ReactNode}</span>,
  SelectPopup: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  SelectItem: (props: AnyProps) => (
    <div data-item={props.value as string} data-disabled={Boolean(props.disabled)}>
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

/** Keeps the selection the way the route does: in the caller, with every change reported. */
function Harness({ initial }: { readonly initial: EnvironmentId | null }) {
  // The mocks change between renders without any prop changing, which the React Compiler
  // (on in the package test lane) would otherwise memoize away.
  "use no memo";
  const [selected, setSelected] = useState(initial);
  return (
    <ProfilerSection
      selectedEnvironmentId={selected}
      onSelectEnvironment={(environmentId) => {
        h.selections.push(environmentId);
        setSelected(environmentId);
      }}
    />
  );
}

async function render(
  props: { initialEnvironmentId?: EnvironmentId | null } = {},
): Promise<HTMLDivElement> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  await act(async () => {
    root.render(<Harness initial={props.initialEnvironmentId ?? null} />);
  });
  return container;
}

async function rerender(container: HTMLDivElement) {
  const entry = mounted.find((candidate) => candidate.container === container)!;
  await act(async () => {
    entry.root.render(<Harness initial={null} />);
  });
}

function fakeEnvironment(label: string, phase = "connected"): FakeEnvironment {
  return { environmentId: `environment-${label}`, label, connection: { phase } };
}

/** The primary "Work Server" plus a second environment, "Luna", on its own settings and status. */
function withLuna(luna = fakeEnvironment("Luna")): FakeEnvironment {
  h.environments = [h.environment!, luna];
  return luna;
}

async function pick(environment: FakeEnvironment) {
  await act(async () => {
    h.selectProps!.onValueChange(environment.environmentId);
  });
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
  h.settingsByEnvironment = {};
  h.statusByEnvironment = {};
  h.updates.length = 0;
  h.selectProps = null;
  h.catalogReady = true;
  h.selections.length = 0;
  h.environment = {
    environmentId: ENVIRONMENT_ID,
    label: "Work Server",
    connection: { phase: "connected" },
  };
  h.environments = [h.environment];
  h.observability = { logsDirectoryPath: "/state/logs" };
  h.preferredEditor = "vscode";
  h.openInEditorCalls.length = 0;
  h.openInEditorResult = { _tag: "Success" };
  h.downloadCalls.length = 0;
  h.downloadGate = null;
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

    expect(h.updates).toEqual([{ environmentId: ENVIRONMENT_ID, patch: { enableProfiler: true } }]);
    // The status is re-read right after the setting is saved.
    expect(h.statusQuery.revalidate).toHaveBeenCalledTimes(1);
  });

  it("shows the recording line, counts and the newest findings", async () => {
    h.settingsByEnvironment[ENVIRONMENT_ID] = true;
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
    h.settingsByEnvironment[ENVIRONMENT_ID] = true;
    queryOf(status({ enabled: false, recordingSince: null }));
    const starting = await render();
    expect(starting.textContent).toContain("Starting...");
    expect(starting.textContent).not.toContain("Off. Turn on");

    queryOf(null);
    const loading = await render();
    expect(loading.textContent).toContain("Checking...");
  });

  it("leaves the dropped count out when nothing was dropped", async () => {
    h.settingsByEnvironment[ENVIRONMENT_ID] = true;
    queryOf(status({ droppedEvents: 0 }));

    const container = await render();

    expect(container.textContent).toContain("1,234 events");
    expect(container.textContent).not.toContain("dropped");
  });

  it("shows the empty state and keeps only the newest 20 findings", async () => {
    h.settingsByEnvironment[ENVIRONMENT_ID] = true;
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
    h.settingsByEnvironment[ENVIRONMENT_ID] = true;
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

  it("disables Open folder with a reason until something is recorded", async () => {
    queryOf(status({ diskBytes: 0 }));

    const container = await render();

    expect(buttonByText(container, "Open folder").disabled).toBe(true);
    expect(container.querySelector("[data-tooltip-popup]")!.textContent).toBe(
      "Nothing recorded yet.",
    );
    await click(buttonByText(container, "Open folder"));
    expect(h.openInEditorCalls).toHaveLength(0);
  });

  describe("environment selection", () => {
    const lunaId = "environment-Luna";

    it("hides the selector while only one environment is connected", async () => {
      const luna = withLuna(fakeEnvironment("Luna", "offline"));
      expect(luna.connection.phase).toBe("offline");

      const container = await render();

      expect(container.querySelector("[data-select]")).toBeNull();
    });

    it("offers every environment, defaulting to the primary", async () => {
      withLuna();

      const container = await render();

      expect(container.querySelector("[data-select]")).not.toBeNull();
      expect(
        [...container.querySelectorAll("[data-item]")].map((item) => item.textContent),
      ).toEqual(["Work Server", "Luna"]);
      expect(container.querySelector('button[aria-label="Environment"]')!.textContent).toBe(
        "Work Server",
      );
    });

    it("switches the setting, status and findings to the selected environment", async () => {
      h.settingsByEnvironment[ENVIRONMENT_ID] = false;
      h.settingsByEnvironment[lunaId] = true;
      const luna = withLuna();
      h.statusByEnvironment[lunaId] = {
        data: status({
          eventCount: 42,
          findings: [finding({ title: "Luna finding" })],
        }),
        error: null,
        isPending: false,
        refresh: vi.fn(),
        revalidate: vi.fn(),
      };
      const container = await render();
      expect(container.querySelector('[role="switch"]')!.getAttribute("aria-checked")).toBe(
        "false",
      );
      expect(container.textContent).not.toContain("Luna finding");

      await pick(luna);

      expect(container.querySelector('[role="switch"]')!.getAttribute("aria-checked")).toBe("true");
      expect(container.textContent).toContain("42 events");
      expect(container.textContent).toContain("Luna finding");

      await click(container.querySelector('[role="switch"]') as HTMLElement);
      expect(h.updates).toEqual([{ environmentId: lunaId, patch: { enableProfiler: false } }]);
      expect(
        (h.statusByEnvironment[lunaId] as { revalidate: ReturnType<typeof vi.fn> }).revalidate,
      ).toHaveBeenCalledTimes(1);
      expect(h.statusQuery.revalidate).not.toHaveBeenCalled();
    });

    it("starts on the environment the status bar pointed at", async () => {
      h.settingsByEnvironment[lunaId] = true;
      const luna = withLuna();

      const container = await render({
        initialEnvironmentId: EnvironmentId.make(luna.environmentId),
      });

      expect(container.querySelector('[role="switch"]')!.getAttribute("aria-checked")).toBe("true");
      expect(container.querySelector('button[aria-label="Environment"]')!.textContent).toBe("Luna");
    });

    it("keeps Open folder and Export bundle to the primary environment", async () => {
      const luna = withLuna();
      const container = await render();
      expect(buttonByText(container, "Open folder").disabled).toBe(false);
      expect(buttonByText(container, "Export bundle").disabled).toBe(false);

      await pick(luna);

      expect(buttonByText(container, "Open folder").disabled).toBe(true);
      expect(buttonByText(container, "Export bundle").disabled).toBe(true);
      const reasons = [...container.querySelectorAll("[data-tooltip-popup]")].map(
        (popup) => popup.textContent,
      );
      expect(reasons).toEqual([
        "The profiler folder is on Luna. Use bibcode profiler on that machine.",
        "Export bundle covers Work Server only. Use bibcode profiler on Luna.",
      ]);
      await click(buttonByText(container, "Export bundle"));
      expect(h.downloadCalls).toHaveLength(0);
    });

    it("keeps an export busy after switching away from the primary and back", async () => {
      let release!: () => void;
      h.downloadGate = new Promise<void>((resolve) => {
        release = resolve;
      });
      const luna = withLuna();
      const container = await render();
      await click(buttonByText(container, "Export bundle"));
      expect(h.downloadCalls).toHaveLength(1);

      await pick(luna);
      await act(async () => {
        h.selectProps!.onValueChange(ENVIRONMENT_ID);
      });

      expect(buttonByText(container, "Exporting").disabled).toBe(true);
      await click(buttonByText(container, "Exporting"));
      expect(h.downloadCalls).toHaveLength(1);
      await act(async () => release());
    });

    it("reports a pick so the route can keep it", async () => {
      const luna = withLuna();
      await render();

      await pick(luna);

      expect(h.selections).toEqual([EnvironmentId.make(luna.environmentId)]);
    });

    it("shows the connected environment while the primary is offline", async () => {
      h.settingsByEnvironment[lunaId] = true;
      h.environment = { ...h.environment!, connection: { phase: "offline" } };
      withLuna();

      const container = await render();

      expect(container.querySelector("[data-select]")).toBeNull();
      expect(container.querySelector('[role="switch"]')!.getAttribute("aria-checked")).toBe("true");
      // The page otherwise describes the primary, so the environment in control is named.
      expect(container.textContent).toContain("Environment");
      expect(container.querySelector("[data-settings-row]")!.textContent).toContain("Luna");
      // Luna is not the primary, so the folder and bundle stay with the primary.
      expect(buttonByText(container, "Open folder").disabled).toBe(true);
    });

    it.each(["connecting", "reconnecting"])(
      "keeps a link to an environment that is %s",
      async (phase) => {
        h.settingsByEnvironment[lunaId] = true;
        const luna = withLuna(fakeEnvironment("Luna", phase));
        const container = await render({ initialEnvironmentId: EnvironmentId.make(lunaId) });

        // Meanwhile the primary shows, the page says so, and the link survives.
        expect(h.selections).toEqual([]);
        expect(container.textContent).toContain("Luna is not connected. Showing Work Server.");
        expect(container.querySelector('[role="switch"]')!.getAttribute("aria-checked")).toBe(
          "false",
        );

        h.environments = [h.environment!, { ...luna, connection: { phase: "connected" } }];
        await rerender(container);

        expect(container.querySelector('[role="switch"]')!.getAttribute("aria-checked")).toBe(
          "true",
        );
      },
    );

    it("keeps a choice while the environment catalog is still loading", async () => {
      withLuna();
      h.catalogReady = false;

      await render({ initialEnvironmentId: EnvironmentId.make("environment-Unknown") });

      expect(h.selections).toEqual([]);
    });

    it("falls back to the primary environment when the selected one disconnects", async () => {
      h.settingsByEnvironment[lunaId] = true;
      const luna = withLuna();
      const container = await render();
      await pick(luna);
      expect(container.querySelector('[role="switch"]')!.getAttribute("aria-checked")).toBe("true");

      h.environments = [h.environment!, { ...luna, connection: { phase: "offline" } }];
      await rerender(container);
      expect(container.querySelector('[role="switch"]')!.getAttribute("aria-checked")).toBe(
        "false",
      );
      expect(h.selections.at(-1)).toBeNull();

      // The choice is gone, not parked: coming back online does not select Luna again.
      h.environments = [h.environment!, luna];
      await rerender(container);
      expect(container.querySelector('[role="switch"]')!.getAttribute("aria-checked")).toBe(
        "false",
      );
      expect(container.querySelector('button[aria-label="Environment"]')!.textContent).toBe(
        "Work Server",
      );
    });
  });

  describe("status refresh", () => {
    it("re-reads the status every 5 seconds while recording and the page is visible", async () => {
      vi.useFakeTimers();
      h.settingsByEnvironment[ENVIRONMENT_ID] = true;
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
      h.settingsByEnvironment[ENVIRONMENT_ID] = true;
      queryOf(status(), { isPending: true });

      await render();
      await act(async () => {
        vi.advanceTimersByTime(30_000);
      });

      expect(h.statusQuery.revalidate).not.toHaveBeenCalled();
    });

    it("does not poll while the page is hidden or the profiler is off", async () => {
      vi.useFakeTimers();
      h.settingsByEnvironment[ENVIRONMENT_ID] = true;
      h.visible = false;
      queryOf(status());
      await render();
      await act(async () => {
        vi.advanceTimersByTime(30_000);
      });
      expect(h.statusQuery.revalidate).not.toHaveBeenCalled();

      h.visible = true;
      h.settingsByEnvironment[ENVIRONMENT_ID] = false;
      queryOf(status({ enabled: false }));
      await render();
      await act(async () => {
        vi.advanceTimersByTime(30_000);
      });
      expect(h.statusQuery.revalidate).not.toHaveBeenCalled();
    });
  });
});
