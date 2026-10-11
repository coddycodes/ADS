// @vitest-environment happy-dom

import type { ReactElement, ReactNode } from "react";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import * as Duration from "effect/Duration";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/unstable/reactivity";
import {
  DEFAULT_SERVER_SETTINGS,
  type DesktopUpdateState,
  EnvironmentId,
  ProviderDriverKind,
  type ProviderInstanceConfig,
  ProviderInstanceId,
  type ServerProvider,
  type ProviderSessionDefault,
} from "@bibcode/contracts";
import {
  DEFAULT_DEFAULT_AGENT_SELECTION,
  DEFAULT_UNIFIED_SETTINGS,
  type UnifiedSettings,
} from "@bibcode/contracts/settings";
import * as settingsPanelsLogic from "./SettingsPanels.logic";

type AnyProps = Record<string, unknown>;

interface CapturedControl {
  readonly kind: string;
  readonly label: string;
  readonly props: AnyProps;
}

const h = vi.hoisted(() => {
  const textOf = (node: unknown): string => {
    if (typeof node === "string" || typeof node === "number") return String(node);
    if (Array.isArray(node)) return node.map(textOf).join("");
    if (node !== null && typeof node === "object" && "props" in node) {
      const props = (node as { props: { children?: unknown } }).props;
      return textOf(props.children);
    }
    return "";
  };

  return {
    textOf,
    controls: [] as Array<{ kind: string; label: string; props: Record<string, unknown> }>,
    rows: [] as Array<Record<string, unknown>>,
    modelPickers: [] as Array<Record<string, unknown>>,
    traitsPickers: [] as Array<Record<string, unknown>>,
    instanceCards: [] as Array<Record<string, unknown>>,
    sections: [] as Array<Record<string, unknown>>,
    theme: "system" as string,
    setTheme: vi.fn(),
    settings: null as unknown,
    updateSettings: vi.fn(),
    observability: null as unknown,
    serverProviders: [] as unknown[],
    updateState: null as unknown,
    primaryEnvironment: null as unknown,
    projects: [] as unknown[],
    serverConfigs: new Map<string, unknown>(),
    unarchiveThread: vi.fn(),
    deleteThread: vi.fn(),
    confirmAndDeleteThread: vi.fn(),
    requestWorktreeRemoval: vi.fn(),
    closeWorktreeRemovalDialog: vi.fn(),
    completeWorktreeRemoval: vi.fn(),
    archive: {
      snapshots: [] as unknown[],
      error: null as string | null,
      isLoading: false,
      refresh: vi.fn(),
    },
    localApi: undefined as unknown,
    previewSupported: true,
    refreshProvidersCommand: vi.fn(),
    refreshProviderUsageCommand: vi.fn(),
    providerUsageQueryRefresh: vi.fn(),
    updateProviderCommand: vi.fn(),
    genericCommand: vi.fn(),
    toastAdd: vi.fn(),
    navigate: vi.fn(),
    dispatchStateUpdates: false,
    atoms: {
      observability: Symbol("primaryServerObservabilityAtom"),
      providers: Symbol("primaryServerProvidersAtom"),
      providerUsage: Symbol("serverEnvironment.providerUsage"),
      refreshProviders: Symbol("serverEnvironment.refreshProviders"),
      refreshProviderUsage: Symbol("serverEnvironment.refreshProviderUsage"),
      updateProvider: Symbol("serverEnvironment.updateProvider"),
    },
  };
});

// Execute functional state updaters synchronously so handler bodies that set
// "started"/toggle flags inside setState callbacks are exercised. React's
// server renderer otherwise ignores dispatches issued after render.
vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  const useState = (initial: unknown) => {
    const [value, dispatch] = (
      actual.useState as (input: unknown) => [unknown, (next: unknown) => void]
    )(initial);
    const setState = (next: unknown) => {
      const resolved =
        typeof next === "function" ? (next as (previous: unknown) => unknown)(value) : next;
      if (h.dispatchStateUpdates) dispatch(resolved);
    };
    return [value, setState];
  };
  return { ...actual, useState };
});

vi.mock("../../hooks/useTheme", () => ({
  useTheme: () => ({ theme: h.theme, setTheme: h.setTheme, resolvedTheme: "light" }),
}));

vi.mock("../../hooks/useSettings", () => ({
  usePrimarySettings: () => h.settings,
  useUpdatePrimarySettings: () => h.updateSettings,
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: (atom: unknown) => {
    if (atom === h.atoms.observability) return h.observability;
    if (atom === h.atoms.providers) return h.serverProviders;
    throw new Error("Unexpected atom read in test");
  },
}));

vi.mock("../../state/server", () => ({
  primaryServerObservabilityAtom: h.atoms.observability,
  primaryServerProvidersAtom: h.atoms.providers,
  serverEnvironment: {
    providerUsage: () => h.atoms.providerUsage,
    refreshProviders: h.atoms.refreshProviders,
    refreshProviderUsage: h.atoms.refreshProviderUsage,
    updateProvider: h.atoms.updateProvider,
  },
}));

vi.mock("../../state/query", () => ({
  useEnvironmentQuery: () => ({
    data: null,
    emission: { _tag: "Initial", waiting: false },
    error: null,
    isPending: false,
    refresh: h.providerUsageQueryRefresh,
  }),
}));

vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: (atom: unknown) => {
    if (atom === h.atoms.refreshProviders) return h.refreshProvidersCommand;
    if (atom === h.atoms.refreshProviderUsage) return h.refreshProviderUsageCommand;
    if (atom === h.atoms.updateProvider) return h.updateProviderCommand;
    return h.genericCommand;
  },
}));

vi.mock("../../state/desktopUpdate", () => ({
  useDesktopUpdateState: () => h.updateState,
}));

vi.mock("../../state/environments", () => ({
  usePrimaryEnvironment: () => h.primaryEnvironment,
}));

vi.mock("../../state/entities", () => ({
  useProjects: () => h.projects,
  useServerConfigs: () => h.serverConfigs,
}));

vi.mock("../../hooks/useThreadActions", () => ({
  useThreadActions: () => ({
    unarchiveThread: h.unarchiveThread,
    deleteThread: h.deleteThread,
    confirmAndDeleteThread: h.confirmAndDeleteThread,
    worktreeRemovalTarget: null,
    requestWorktreeRemoval: h.requestWorktreeRemoval,
    closeWorktreeRemovalDialog: h.closeWorktreeRemovalDialog,
    completeWorktreeRemoval: h.completeWorktreeRemoval,
  }),
}));

vi.mock("../WorktreeRemovalDialog", () => ({
  WorktreeRemovalDialog: (props: AnyProps) => {
    h.controls.push({ kind: "dialog", label: "worktree-removal", props });
    return null;
  },
}));

vi.mock("../../lib/archivedThreadsState", () => ({
  useArchivedThreadSnapshots: () => h.archive,
}));

vi.mock("../../localApi", () => ({
  readLocalApi: () => h.localApi,
  ensureLocalApi: () => {
    if (h.localApi === undefined) {
      throw new Error("Local API not found");
    }
    return h.localApi;
  },
}));

vi.mock("../../previewStateStore", () => ({
  isPreviewSupportedInRuntime: () => h.previewSupported,
  canPreviewAnyUrlInRuntime: () => h.previewSupported,
}));

vi.mock("../../branding", () => ({
  APP_VERSION: "9.9.9-test",
  HOSTED_APP_CHANNEL: "latest",
  HOSTED_APP_CHANNEL_LABEL: "Latest",
}));

vi.mock("@tanstack/react-router", () => ({
  Link: (props: AnyProps) => (
    <a data-testid="router-link" data-to={String(props.to)}>
      {props.children as ReactNode}
    </a>
  ),
  useNavigate: () => h.navigate,
}));

vi.mock("@bibcode/client-runtime/environment", () => ({
  scopeThreadRef: (environmentId: unknown, threadId: unknown) => ({ environmentId, threadId }),
}));

vi.mock("@bibcode/client-runtime/errors", () => ({
  safeErrorLogAttributes: (error: unknown) => ({ error: String(error) }),
}));

vi.mock("@bibcode/client-runtime/state/runtime", () => ({
  isAtomCommandInterrupted: (result: unknown) =>
    (result as { interrupted?: boolean }).interrupted === true,
  squashAtomCommandFailure: (result: unknown) =>
    (result as { cause?: unknown }).cause ?? new Error("Command failed."),
  settlePromise: async (run: () => Promise<unknown>) => {
    try {
      return { _tag: "Success", value: await run() };
    } catch (cause) {
      return { _tag: "Failure", cause };
    }
  },
}));

vi.mock("../chat/ProviderModelPicker", () => ({
  ProviderModelPicker: (props: AnyProps) => {
    h.modelPickers.push(props);
    return <div data-testid="provider-model-picker" />;
  },
}));

vi.mock("../chat/TraitsPicker", () => ({
  findProviderEffortDescriptor: (descriptors: ReadonlyArray<{ id: string; type: string }>) =>
    descriptors.find((descriptor) => descriptor.type === "select" && descriptor.id === "effort") ??
    null,
  TraitsPicker: (props: AnyProps) => {
    h.traitsPickers.push(props);
    return <div data-testid="traits-picker" />;
  },
}));

vi.mock("./AddProviderInstanceDialog", () => ({
  AddProviderInstanceDialog: (props: AnyProps) => (
    <div data-testid="add-provider-instance-dialog" data-open={String(props.open)} />
  ),
}));

vi.mock("./ProviderInstanceCard", () => ({
  ProviderInstanceCard: (props: AnyProps) => {
    h.instanceCards.push(props);
    return (
      <div data-testid="provider-instance-card" data-instance-id={String(props.instanceId)}>
        {props.headerAction as ReactNode}
      </div>
    );
  },
}));

vi.mock("./WorktreeWorkspaceSetting", () => ({
  WorktreeWorkspaceSetting: () => <div data-testid="worktree-workspace-setting">Workspace</div>,
}));

vi.mock("../ProjectFavicon", () => ({
  ProjectFavicon: () => <span data-testid="project-favicon" />,
}));

vi.mock("./settingsLayout", () => ({
  useRelativeTimeTick: () => Date.now(),
  SettingsPageContainer: (props: AnyProps) => (
    <div data-testid="settings-page">{props.children as ReactNode}</div>
  ),
  SettingsSection: (props: AnyProps) => {
    h.sections.push(props);
    return (
      <section data-section-title={typeof props.title === "string" ? props.title : "custom"}>
        {props.icon as ReactNode}
        {props.title as ReactNode}
        {props.headerAction as ReactNode}
        {props.children as ReactNode}
      </section>
    );
  },
  SettingsRow: (props: AnyProps) => {
    h.rows.push(props);
    return (
      <div data-testid="settings-row">
        {props.title as ReactNode}
        {props.titleTag as ReactNode}
        {props.resetAction as ReactNode}
        {props.description as ReactNode}
        {props.status as ReactNode}
        {props.control as ReactNode}
        {props.children as ReactNode}
      </div>
    );
  },
  SettingResetButton: (props: AnyProps) => {
    h.controls.push({
      kind: "button",
      label: `Reset ${String(props.label)} to default`,
      props,
    });
    return <button type="button" data-reset-label={String(props.label)} />;
  },
}));

vi.mock("../ui/button", () => ({
  Button: (props: AnyProps) => {
    h.controls.push({
      kind: "button",
      label: (props["aria-label"] as string | undefined) ?? h.textOf(props.children),
      props,
    });
    return (
      <button type="button" data-variant={String(props.variant)} disabled={Boolean(props.disabled)}>
        {props.children as ReactNode}
      </button>
    );
  },
}));

vi.mock("../ui/select", () => ({
  Select: (props: AnyProps) => {
    h.controls.push({ kind: "select", label: String(props.value), props });
    return (
      <div data-testid="select" data-value={String(props.value)}>
        {props.children as ReactNode}
      </div>
    );
  },
  SelectTrigger: (props: AnyProps) => (
    <div data-select-trigger aria-label={props["aria-label"] as string | undefined}>
      {props.children as ReactNode}
    </div>
  ),
  SelectValue: (props: AnyProps) => <span>{props.children as ReactNode}</span>,
  SelectPopup: (props: AnyProps) => <div data-select-popup>{props.children as ReactNode}</div>,
  SelectItem: (props: AnyProps) => (
    <div data-select-item data-value={String(props.value)}>
      {props.children as ReactNode}
    </div>
  ),
}));

vi.mock("../ui/switch", () => ({
  Switch: (props: AnyProps) => {
    h.controls.push({
      kind: "switch",
      label: (props["aria-label"] as string | undefined) ?? "",
      props,
    });
    return (
      <span
        data-testid="switch"
        aria-label={props["aria-label"] as string | undefined}
        data-checked={String(props.checked)}
        data-disabled={String(Boolean(props.disabled))}
      />
    );
  },
}));

vi.mock("../ui/toggle-group", () => ({
  ToggleGroup: (props: AnyProps) => {
    h.controls.push({
      kind: "toggle-group",
      label: (props["aria-label"] as string | undefined) ?? "",
      props,
    });
    return <div data-testid="toggle-group">{props.children as ReactNode}</div>;
  },
  Toggle: (props: AnyProps) => <button type="button">{props.children as ReactNode}</button>,
}));

vi.mock("../ui/toast", () => ({
  toastManager: { add: h.toastAdd },
  stackedThreadToast: (options: unknown) => options,
}));

vi.mock("../ui/tooltip", () => ({
  Tooltip: (props: AnyProps) => <>{props.children as ReactNode}</>,
  TooltipTrigger: (props: AnyProps) => (
    <span data-tooltip-trigger>
      {props.render as ReactNode}
      {props.children as ReactNode}
    </span>
  ),
  TooltipPopup: (props: AnyProps) => <div data-tooltip-popup>{props.children as ReactNode}</div>,
}));

vi.mock("../ui/draft-input", () => ({
  DraftInput: (props: AnyProps) => {
    h.controls.push({
      kind: "draft-input",
      label: (props["aria-label"] as string | undefined) ?? "",
      props,
    });
    return (
      <input
        aria-label={props["aria-label"] as string | undefined}
        defaultValue={props.value as string | undefined}
        readOnly
      />
    );
  },
}));

import {
  AboutSettingsPanel,
  AgentsSettingsPanel,
  ArchivedThreadsPanel,
  GeneralSettingsPanel,
  ProviderSettingsPanel,
  StatusBarSettingsPanel,
  TerminalSettingsPanel,
  useGeneralSettingsRestore,
} from "./SettingsPanels";

const CODEX_INSTANCE_ID = ProviderInstanceId.make("codex");
const CODEX_DRIVER = ProviderDriverKind.make("codex");
const CLAUDE_DRIVER = ProviderDriverKind.make("claudeAgent");
const CURSOR_DRIVER = ProviderDriverKind.make("cursor");
const ENVIRONMENT_ID = EnvironmentId.make("environment-test");

function clearRegistries(): void {
  h.controls.length = 0;
  h.rows.length = 0;
  h.modelPickers.length = 0;
  h.traitsPickers.length = 0;
  h.instanceCards.length = 0;
  h.sections.length = 0;
}

function render(node: ReactElement): string {
  clearRegistries();
  return renderToStaticMarkup(node);
}

function renderedText(): string {
  return render(<GeneralSettingsPanel />).replaceAll("&quot;", '"');
}

function findControls(kind: string, label: string): CapturedControl[] {
  const exact = h.controls.filter((entry) => entry.kind === kind && entry.label === label);
  if (exact.length > 0) {
    return exact;
  }
  return h.controls.filter((entry) => entry.kind === kind && entry.label.includes(label));
}

function control(kind: string, label: string): CapturedControl {
  const found = findControls(kind, label);
  if (found.length === 0) {
    throw new Error(`No ${kind} control labelled ${label}`);
  }
  return found[0]!;
}

function invoke(entry: CapturedControl, handlerName: string, ...args: unknown[]): unknown {
  const handler = entry.props[handlerName];
  if (typeof handler !== "function") {
    throw new Error(`Control ${entry.label} has no handler ${handlerName}`);
  }
  return (handler as (...input: unknown[]) => unknown)(...args);
}

async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function makeUpdateState(overrides: Partial<DesktopUpdateState>): DesktopUpdateState {
  return {
    enabled: true,
    status: "idle",
    currentVersion: "9.9.9-test",
    hostArch: "x64",
    appArch: "x64",
    runningUnderArm64Translation: false,
    availableVersion: null,
    downloadedVersion: null,
    downloadPercent: null,
    checkedAt: null,
    message: null,
    errorContext: null,
    canRetry: true,
    ...overrides,
  };
}

function makeServerProvider(overrides: {
  readonly instanceId: string;
  readonly driver: ProviderDriverKind;
  readonly behindLatest?: boolean;
  readonly canUpdate?: boolean;
  readonly updateRunning?: boolean;
  readonly checkedAt?: string;
}): ServerProvider {
  const behindLatest = overrides.behindLatest ?? false;
  const provider: ServerProvider = {
    instanceId: ProviderInstanceId.make(overrides.instanceId),
    driver: overrides.driver,
    enabled: true,
    installed: true,
    version: "1.0.0",
    status: "ready",
    auth: { status: "authenticated" },
    checkedAt: overrides.checkedAt ?? "2026-06-26T12:00:00.000Z",
    models: [
      { slug: "model-alpha", name: "Model Alpha" },
      { slug: "model-beta", name: "Model Beta" },
    ],
    slashCommands: [],
    skills: [],
    versionAdvisory: {
      status: behindLatest ? "behind_latest" : "current",
      currentVersion: "1.0.0",
      latestVersion: behindLatest ? "1.1.0" : "1.0.0",
      updateCommand: "npm install -g provider@latest",
      canUpdate: overrides.canUpdate ?? true,
      checkedAt: "2026-06-26T12:00:00.000Z",
      message: behindLatest ? "Update available." : "Up to date.",
    },
  } as unknown as ServerProvider;

  if (overrides.updateRunning) {
    return {
      ...provider,
      updateState: {
        status: "running",
        startedAt: "2026-06-26T12:00:00.000Z",
        finishedAt: null,
        message: null,
        output: null,
      },
    } as unknown as ServerProvider;
  }
  return provider;
}

function changedSettings(): UnifiedSettings {
  return {
    ...DEFAULT_UNIFIED_SETTINGS,
    terminalFontPreference: { mode: "custom", family: "Iosevka" },
    timestampFormat: "24-hour",
    sidebarThreadPreviewCount: DEFAULT_UNIFIED_SETTINGS.sidebarThreadPreviewCount + 3,
    wordWrap: !DEFAULT_UNIFIED_SETTINGS.wordWrap,
    diffIgnoreWhitespace: !DEFAULT_UNIFIED_SETTINGS.diffIgnoreWhitespace,
    autoOpenPlanSidebar: !DEFAULT_UNIFIED_SETTINGS.autoOpenPlanSidebar,
    enableAssistantStreaming: !DEFAULT_UNIFIED_SETTINGS.enableAssistantStreaming,
    enableProviderUpdateChecks: !DEFAULT_UNIFIED_SETTINGS.enableProviderUpdateChecks,
    automaticGitFetchInterval: Duration.sum(
      DEFAULT_UNIFIED_SETTINGS.automaticGitFetchInterval,
      Duration.minutes(5),
    ),
    defaultThreadEnvMode: "worktree",
    newWorktreesStartFromOrigin: !DEFAULT_UNIFIED_SETTINGS.newWorktreesStartFromOrigin,
    worktreeBaseDirectory: "~/worktrees",
    addProjectBaseDirectory: "~/code",
    confirmThreadArchive: !DEFAULT_UNIFIED_SETTINGS.confirmThreadArchive,
    confirmThreadDelete: !DEFAULT_UNIFIED_SETTINGS.confirmThreadDelete,
    terminal: {
      webglEnabled: !DEFAULT_UNIFIED_SETTINGS.terminal.webglEnabled,
    },
    textGenerationModelSelection: { instanceId: CODEX_INSTANCE_ID, model: "model-beta" },
    usagePercentageDisplay: "used",
    statusBarUsageMode: "compact",
    statusBarItems: ["codex"],
  };
}

beforeEach(() => {
  vi.unstubAllGlobals();
  clearRegistries();
  h.theme = "system";
  h.setTheme.mockReset();
  h.settings = { ...DEFAULT_UNIFIED_SETTINGS };
  h.updateSettings.mockReset();
  h.observability = null;
  h.serverProviders = [];
  h.updateState = null;
  h.primaryEnvironment = { environmentId: ENVIRONMENT_ID };
  h.projects = [];
  h.unarchiveThread.mockReset();
  h.unarchiveThread.mockResolvedValue({ _tag: "Success", value: undefined });
  h.confirmAndDeleteThread.mockReset();
  h.confirmAndDeleteThread.mockResolvedValue({ _tag: "Success", value: undefined });
  h.requestWorktreeRemoval.mockReset();
  h.closeWorktreeRemovalDialog.mockReset();
  h.completeWorktreeRemoval.mockReset();
  h.completeWorktreeRemoval.mockResolvedValue({ _tag: "Success", value: undefined });
  h.archive.snapshots = [];
  h.archive.error = null;
  h.archive.isLoading = false;
  h.archive.refresh.mockReset();
  h.localApi = undefined;
  h.previewSupported = true;
  h.refreshProvidersCommand.mockReset();
  h.refreshProvidersCommand.mockResolvedValue({ _tag: "Success", value: { providers: [] } });
  h.refreshProviderUsageCommand.mockReset();
  h.refreshProviderUsageCommand.mockResolvedValue({ _tag: "Success", value: { providers: [] } });
  h.providerUsageQueryRefresh.mockReset();
  h.updateProviderCommand.mockReset();
  h.updateProviderCommand.mockResolvedValue({ _tag: "Success", value: { providers: [] } });
  h.genericCommand.mockReset();
  h.toastAdd.mockReset();
  h.dispatchStateUpdates = false;
});

describe("GeneralSettingsPanel", () => {
  it("places Workspace after Start from origin and before Add project starts in", () => {
    h.settings = changedSettings();

    const markup = render(<GeneralSettingsPanel />);
    const startFromOrigin = markup.indexOf("Start from origin");
    const workspace = markup.indexOf('data-testid="worktree-workspace-setting"');
    const addProject = markup.indexOf("Add project starts in");

    expect(startFromOrigin).toBeGreaterThanOrEqual(0);
    expect(workspace).toBeGreaterThan(startFromOrigin);
    expect(addProject).toBeGreaterThan(workspace);
  });

  it("keeps focused settings out of General and renders each focused panel", () => {
    const markup = render(<GeneralSettingsPanel />);

    expect(markup).toContain('data-section-title="General"');
    expect(markup).not.toContain('data-section-title="Agents"');
    expect(markup).not.toContain('data-section-title="Status bar"');
    expect(markup).not.toContain('data-section-title="Terminal"');
    expect(markup).not.toContain('data-section-title="About"');
    expect(markup).not.toContain("Text generation model");

    expect(render(<AgentsSettingsPanel />)).toContain('data-section-title="Agents"');
    expect(render(<StatusBarSettingsPanel />)).toContain('data-section-title="Status bar"');
    expect(render(<TerminalSettingsPanel />)).toContain('data-section-title="Terminal"');
    expect(render(<AboutSettingsPanel />)).toContain('data-section-title="About"');
  });
});

describe("TerminalSettingsPanel", () => {
  it("renders Follow app theme as the default terminal theme", () => {
    render(<TerminalSettingsPanel />);

    expect(control("select", "app").props.value).toBe("app");
  });

  it("resets a changed terminal theme to Follow app theme", () => {
    h.settings = {
      ...DEFAULT_UNIFIED_SETTINGS,
      terminalThemePreference: "light",
    };

    render(<TerminalSettingsPanel />);

    invoke(control("button", "Reset terminal theme to default"), "onClick");
    expect(h.updateSettings).toHaveBeenCalledWith({
      terminalThemePreference: "app",
    });
  });

  it("changes the device-local terminal font preset", () => {
    render(<TerminalSettingsPanel />);

    const fontPreset = control("select", "bundled");
    invoke(fontPreset, "onValueChange", "system");
    expect(h.updateSettings).toHaveBeenCalledWith({
      terminalFontPreference: { mode: "system" },
    });

    invoke(fontPreset, "onValueChange", "custom");
    expect(h.updateSettings).toHaveBeenCalledWith({
      terminalFontPreference: { mode: "custom", family: "JetBrains Mono" },
    });
  });

  it("validates custom terminal fonts and reports device availability", () => {
    const check = vi.fn(() => false);
    vi.stubGlobal("document", { fonts: { check } });
    h.settings = {
      ...DEFAULT_UNIFIED_SETTINGS,
      terminalFontPreference: { mode: "custom", family: "Missing Mono" },
    };

    const markup = render(<TerminalSettingsPanel />);

    expect(markup).toContain("This font is not available on this device.");
    expect(check).toHaveBeenCalledWith('12px "Missing Mono"');

    const customFont = control("draft-input", "Custom terminal font family");
    invoke(customFont, "onCommit", "  Maple Mono  ");
    expect(h.updateSettings).toHaveBeenCalledWith({
      terminalFontPreference: { mode: "custom", family: "Maple Mono" },
    });

    invoke(customFont, "onCommit", "Maple Mono, monospace");
    expect(h.updateSettings).toHaveBeenCalledTimes(1);
  });

  it("renders a WebGL renderer toggle bound to terminal.webglEnabled", () => {
    h.settings = {
      ...DEFAULT_UNIFIED_SETTINGS,
      terminal: { webglEnabled: true },
    };

    render(<TerminalSettingsPanel />);

    const webglSwitch = control("switch", "Use WebGL terminal renderer");
    expect(webglSwitch.props.checked).toBe(true);

    invoke(webglSwitch, "onCheckedChange", false);
    expect(h.updateSettings).toHaveBeenCalledWith({
      terminal: { webglEnabled: false },
    });
  });

  it("resets a changed WebGL renderer setting to its default", () => {
    h.settings = {
      ...DEFAULT_UNIFIED_SETTINGS,
      terminal: { webglEnabled: false },
    };

    render(<TerminalSettingsPanel />);

    invoke(control("button", "Reset WebGL renderer to default"), "onClick");
    expect(h.updateSettings).toHaveBeenCalledWith({
      terminal: {
        webglEnabled: DEFAULT_UNIFIED_SETTINGS.terminal.webglEnabled,
      },
    });
  });

  it("resets a changed terminal font setting to its default", () => {
    h.settings = {
      ...DEFAULT_UNIFIED_SETTINGS,
      terminalFontPreference: { mode: "custom", family: "Iosevka" },
    };

    render(<TerminalSettingsPanel />);

    invoke(control("button", "Reset terminal font to default"), "onClick");
    expect(h.updateSettings).toHaveBeenCalledWith({
      terminalFontPreference: DEFAULT_UNIFIED_SETTINGS.terminalFontPreference,
    });
  });
});

describe("GeneralSettingsPanel", () => {
  it("renders default settings with no reset affordances and routes updates", () => {
    const assignSpy = vi.fn();
    vi.stubGlobal("window", {
      desktopBridge: undefined,
      confirm: vi.fn(() => true),
      location: { assign: assignSpy },
    });

    const markup = renderedText();

    expect(markup).toContain("Theme");
    expect(markup).toContain("Time format");
    expect(renderedText()).toContain(
      'Leave empty to use "~/" for Clone and Create in Add Project.',
    );
    expect(findControls("button", "Reset ")).toHaveLength(0);
    expect(findControls("switch", "Start new worktrees from origin by default")).toHaveLength(0);

    invoke(control("select", "system"), "onValueChange", "dark");
    expect(h.setTheme).toHaveBeenCalledWith("dark");
    invoke(control("select", "system"), "onValueChange", "bogus");
    expect(h.setTheme).toHaveBeenCalledTimes(1);

    invoke(control("select", "locale"), "onValueChange", "12-hour");
    expect(h.updateSettings).toHaveBeenCalledWith({ timestampFormat: "12-hour" });
    invoke(control("select", "locale"), "onValueChange", "bogus");

    invoke(
      control("switch", "Wrap code, tables, diffs, and file previews by default"),
      "onCheckedChange",
      true,
    );
    expect(h.updateSettings).toHaveBeenCalledWith({ wordWrap: true });

    invoke(control("switch", "Hide whitespace changes by default"), "onCheckedChange", true);
    expect(h.updateSettings).toHaveBeenCalledWith({ diffIgnoreWhitespace: true });

    invoke(control("switch", "Stream assistant messages"), "onCheckedChange", true);
    expect(h.updateSettings).toHaveBeenCalledWith({ enableAssistantStreaming: true });

    invoke(control("switch", "Check provider versions"), "onCheckedChange", false);
    expect(h.updateSettings).toHaveBeenCalledWith({ enableProviderUpdateChecks: false });

    invoke(control("switch", "Open the task panel automatically"), "onCheckedChange", true);
    expect(h.updateSettings).toHaveBeenCalledWith({ autoOpenPlanSidebar: true });

    invoke(control("switch", "Confirm thread archiving"), "onCheckedChange", false);
    expect(h.updateSettings).toHaveBeenCalledWith({ confirmThreadArchive: false });

    invoke(control("switch", "Confirm thread deletion"), "onCheckedChange", false);
    expect(h.updateSettings).toHaveBeenCalledWith({ confirmThreadDelete: false });

    invoke(control("select", "local"), "onValueChange", "worktree");
    expect(h.updateSettings).toHaveBeenCalledWith({ defaultThreadEnvMode: "worktree" });
    invoke(control("select", "local"), "onValueChange", "bogus");

    invoke(control("draft-input", "Add project base directory"), "onCommit", "~/somewhere");
    expect(h.updateSettings).toHaveBeenCalledWith({ addProjectBaseDirectory: "~/somewhere" });

    expect(assignSpy).not.toHaveBeenCalled();
  });

  it("offers the link target choice and routes selections and resets", () => {
    const markup = renderedText();

    expect(markup).toContain("Open links in");
    expect(markup).toContain("BiBCode browser");
    expect(markup).toContain("System browser");
    expect(findControls("button", "Reset link target")).toHaveLength(0);

    invoke(control("select", "app"), "onValueChange", "system");
    expect(h.updateSettings).toHaveBeenCalledWith({ browserLinkTarget: "system" });
    invoke(control("select", "app"), "onValueChange", "bogus");
    expect(h.updateSettings).toHaveBeenCalledTimes(1);

    h.updateSettings.mockReset();
    h.settings = { ...DEFAULT_UNIFIED_SETTINGS, browserLinkTarget: "system" };
    render(<GeneralSettingsPanel />);
    invoke(control("button", "Reset link target to default"), "onClick");
    expect(h.updateSettings).toHaveBeenCalledWith({ browserLinkTarget: "app" });
  });

  it("hides the link target where the BiBCode browser is unavailable", () => {
    h.previewSupported = false;
    h.settings = { ...DEFAULT_UNIFIED_SETTINGS, browserLinkTarget: "system" };

    const markup = renderedText();

    expect(markup).not.toContain("Open links in");
    expect(findControls("button", "Reset link target to default")).toHaveLength(0);
  });

  it("renders changed general settings with reset actions", () => {
    h.theme = "dark";
    h.settings = changedSettings();

    const markup = render(<GeneralSettingsPanel />);

    expect(markup).toContain("Start from origin");
    expect(findControls("switch", "Start new worktrees from origin by default")).toHaveLength(1);

    invoke(control("button", "Reset theme to default"), "onClick");
    expect(h.setTheme).toHaveBeenCalledWith("system");

    invoke(control("button", "Reset time format to default"), "onClick");
    expect(h.updateSettings).toHaveBeenCalledWith({
      timestampFormat: DEFAULT_UNIFIED_SETTINGS.timestampFormat,
    });

    invoke(control("button", "Reset word wrapping to default"), "onClick");
    invoke(control("button", "Reset diff whitespace changes to default"), "onClick");
    invoke(control("button", "Reset assistant output to default"), "onClick");
    invoke(control("button", "Reset provider update checks to default"), "onClick");
    invoke(control("button", "Reset auto-open task panel to default"), "onClick");
    invoke(control("button", "Reset new threads to default"), "onClick");
    invoke(control("button", "Reset new worktrees start from origin to default"), "onClick");
    invoke(control("button", "Reset add project base directory to default"), "onClick");
    invoke(control("button", "Reset archive confirmation to default"), "onClick");
    invoke(control("button", "Reset delete confirmation to default"), "onClick");
    expect(h.updateSettings).toHaveBeenCalledTimes(11);

    invoke(
      control("switch", "Start new worktrees from origin by default"),
      "onCheckedChange",
      false,
    );
    expect(h.updateSettings).toHaveBeenCalledWith({ newWorktreesStartFromOrigin: false });
  });
});

describe("AgentsSettingsPanel", () => {
  it("renders independent experimental activity settings", () => {
    const markup = render(<AgentsSettingsPanel />);

    expect(markup).toContain("Chat agent activity");
    expect(markup).toContain("AI Terminal agent activity");
    expect(markup.match(/Experimental/g)).toHaveLength(2);
    expect(markup).toContain(
      "Show live agent and background-task activity in the Chat panel. Disabling this stops Chat activity monitoring and collection.",
    );
    expect(markup).toContain(
      "Show live agent and background-task activity in AI Terminals. Disabling this stops AI Terminal activity monitoring and collection.",
    );
  });

  it("uses independent defaults", () => {
    render(<AgentsSettingsPanel />);

    expect(control("switch", "Chat agent activity").props.checked).toBe(true);
    expect(control("switch", "AI Terminal agent activity").props.checked).toBe(false);
    expect(findControls("button", "Reset chat agent activity to default")).toHaveLength(0);
    expect(findControls("button", "Reset AI Terminal agent activity to default")).toHaveLength(0);
  });

  it("updates each activity setting through only its matching field", () => {
    render(<AgentsSettingsPanel />);

    invoke(control("switch", "Chat agent activity"), "onCheckedChange", false);
    invoke(control("switch", "AI Terminal agent activity"), "onCheckedChange", true);
    expect(h.updateSettings).toHaveBeenNthCalledWith(1, { enableChatAgentActivity: false });
    expect(h.updateSettings).toHaveBeenNthCalledWith(2, { enableTerminalAgentActivity: true });
  });

  it("resets each activity setting to its independent server default", () => {
    h.settings = {
      ...DEFAULT_UNIFIED_SETTINGS,
      enableChatAgentActivity: false,
      enableTerminalAgentActivity: true,
    };
    render(<AgentsSettingsPanel />);

    expect(control("switch", "Chat agent activity").props.checked).toBe(false);
    expect(control("switch", "AI Terminal agent activity").props.checked).toBe(true);
    invoke(control("button", "Reset chat agent activity to default"), "onClick");
    invoke(control("button", "Reset AI Terminal agent activity to default"), "onClick");
    expect(h.updateSettings).toHaveBeenNthCalledWith(1, {
      enableChatAgentActivity: DEFAULT_SERVER_SETTINGS.enableChatAgentActivity,
    });
    expect(h.updateSettings).toHaveBeenNthCalledWith(2, {
      enableTerminalAgentActivity: DEFAULT_SERVER_SETTINGS.enableTerminalAgentActivity,
    });
  });

  it("lists selectable default agents before the text generation model and saves a selection", () => {
    h.serverProviders = [
      makeServerProvider({ instanceId: "codex", driver: CODEX_DRIVER }),
      makeServerProvider({ instanceId: "claudeAgent", driver: CLAUDE_DRIVER }),
    ];

    const markup = render(<AgentsSettingsPanel />);

    expect(markup.indexOf("Default Agent")).toBeLessThan(markup.indexOf("Text generation model"));
    expect(markup).toContain("Codex");
    expect(markup).toContain("Claude");
    expect(markup).toContain("Codex Terminal");
    expect(markup).toContain("Claude Terminal");
    expect(markup).not.toContain("Open Terminal");
    expect(markup).not.toContain("Add custom action");

    invoke(control("select", "chat:codex"), "onValueChange", "terminal:claudeAgent");
    expect(h.updateSettings).toHaveBeenCalledWith({
      defaultAgent: {
        kind: "terminal",
        instanceId: ProviderInstanceId.make("claudeAgent"),
      },
    });
  });

  it("resets a changed default agent to the shared default", () => {
    h.settings = {
      ...DEFAULT_UNIFIED_SETTINGS,
      defaultAgent: { kind: "terminal", instanceId: ProviderInstanceId.make("claudeAgent") },
    };

    render(<AgentsSettingsPanel />);

    invoke(control("button", "Reset default agent to default"), "onClick");
    expect(h.updateSettings).toHaveBeenCalledWith({
      defaultAgent: DEFAULT_DEFAULT_AGENT_SELECTION,
    });
  });

  it("shows an empty default-agent state when no providers are ready", () => {
    const markup = render(<AgentsSettingsPanel />);

    expect(markup).toContain("No enabled agents available");
  });

  it("updates the text generation model through the pickers", () => {
    h.serverProviders = [
      makeServerProvider({ instanceId: "codex", driver: CODEX_DRIVER }),
      makeServerProvider({ instanceId: "claudeAgent", driver: CLAUDE_DRIVER }),
    ];
    render(<AgentsSettingsPanel />);

    expect(h.modelPickers).toHaveLength(1);
    expect(h.traitsPickers).toHaveLength(1);

    const picker = h.modelPickers[0]!;
    (picker.onInstanceModelChange as (instanceId: unknown, model: string) => void)(
      CODEX_INSTANCE_ID,
      "model-beta",
    );
    expect(h.updateSettings).toHaveBeenCalledTimes(1);
    const patch = h.updateSettings.mock.calls[0]?.[0] as {
      textGenerationModelSelection?: { model?: string };
    };
    expect(patch.textGenerationModelSelection?.model).toBe("model-beta");

    const traits = h.traitsPickers[0]!;
    (traits.onPromptChange as () => void)();
    (traits.onModelOptionsChange as (options: unknown) => void)([
      { id: "reasoning", value: "high" },
    ]);
    expect(h.updateSettings).toHaveBeenCalledTimes(2);
  });

  it("resets a changed text generation model to its default", () => {
    h.settings = changedSettings();

    render(<AgentsSettingsPanel />);

    invoke(control("button", "Reset text generation model to default"), "onClick");
    expect(h.updateSettings).toHaveBeenCalledWith({
      textGenerationModelSelection: DEFAULT_UNIFIED_SETTINGS.textGenerationModelSelection,
    });
  });
});

describe("AboutSettingsPanel", () => {
  it("renders diagnostics with its tracing description", () => {
    h.observability = { localTracingEnabled: true };

    const markup = render(<AboutSettingsPanel />);

    expect(markup).toContain("Local trace file.");
    expect(markup).toContain("View diagnostics");
  });

  it("changes the hosted update track", () => {
    const assignSpy = vi.fn();
    vi.stubGlobal("window", {
      desktopBridge: undefined,
      confirm: vi.fn(() => true),
      location: { assign: assignSpy },
    });

    render(<AboutSettingsPanel />);

    // Hosted update-track select: picking the current channel is a no-op,
    // picking the other channel navigates to the channel-selection URL.
    invoke(control("select", "latest"), "onValueChange", "latest");
    expect(assignSpy).not.toHaveBeenCalled();
    invoke(control("select", "latest"), "onValueChange", "nightly");
    expect(assignSpy).toHaveBeenCalledTimes(1);
    expect(String(assignSpy.mock.calls[0]?.[0])).toContain("channel=nightly");
  });

  describe("desktop update section", () => {
    function stubDesktopWindow(bridge: Record<string, unknown>, confirmResult = true) {
      const confirmSpy = vi.fn(() => confirmResult);
      vi.stubGlobal("window", {
        desktopBridge: bridge,
        confirm: confirmSpy,
        location: { assign: vi.fn() },
      });
      return { confirmSpy };
    }

    it("checks for updates and reports failures", async () => {
      const bridge = {
        checkForUpdate: vi
          .fn()
          .mockResolvedValueOnce({ checked: true, state: {} })
          .mockResolvedValueOnce({
            checked: false,
            state: { status: "checking", message: "Checking for updates." },
          })
          .mockResolvedValueOnce({
            checked: false,
            state: { status: "error", message: "No feed available." },
          })
          .mockRejectedValueOnce(new Error("network down")),
      };
      stubDesktopWindow(bridge);
      h.updateState = makeUpdateState({ status: "idle" });

      const markup = render(<AboutSettingsPanel />);
      expect(markup).not.toContain("Update track");
      expect(markup).not.toContain("Nightly");
      expect(markup).toContain("Check for Updates");
      expect("setUpdateChannel" in bridge).toBe(false);

      const button = control("button", "Check for Updates");
      invoke(button, "onClick");
      await flush();
      expect(h.toastAdd).not.toHaveBeenCalled();

      invoke(button, "onClick");
      await flush();
      expect(h.toastAdd).not.toHaveBeenCalled();

      invoke(button, "onClick");
      await flush();
      expect(h.toastAdd).toHaveBeenCalledWith(
        expect.objectContaining({ title: "Could not check for updates" }),
      );

      invoke(button, "onClick");
      await flush();
      expect(h.toastAdd).toHaveBeenCalledTimes(2);
      expect(bridge.checkForUpdate).toHaveBeenCalledTimes(4);
    });

    it("disables update checks when the packaged build has no updater state", () => {
      stubDesktopWindow({ checkForUpdate: vi.fn() });
      h.updateState = null;

      render(<AboutSettingsPanel />);

      expect(control("button", "Check for Updates").props.disabled).toBe(true);
    });

    it("downloads available updates and surfaces download failures", async () => {
      const bridge = {
        downloadUpdate: vi
          .fn()
          .mockResolvedValueOnce(undefined)
          .mockRejectedValueOnce(new Error("disk full")),
        checkForUpdate: vi.fn(),
      };
      stubDesktopWindow(bridge);
      h.updateState = makeUpdateState({ status: "available", availableVersion: "10.0.0" });

      const markup = render(<AboutSettingsPanel />);
      expect(markup).toContain("9.9.9-test → 10.0.0");
      expect(markup).toContain("Update available.");
      expect(markup).toContain("Update 10.0.0 ready to download");

      const button = control("button", "Download");
      invoke(button, "onClick");
      await flush();
      expect(h.toastAdd).not.toHaveBeenCalled();

      invoke(button, "onClick");
      await flush();
      expect(h.toastAdd).toHaveBeenCalledWith(
        expect.objectContaining({ title: "Could not download update" }),
      );
      expect(bridge.downloadUpdate).toHaveBeenCalledTimes(2);
    });

    it("opens typed protection before installing a downloaded update", () => {
      const bridge = {
        installUpdate: vi.fn().mockRejectedValue(new Error("install failed")),
        checkForUpdate: vi.fn(),
      };
      const { confirmSpy } = stubDesktopWindow(bridge, false);
      h.updateState = makeUpdateState({
        status: "downloaded",
        availableVersion: "10.0.0",
        downloadedVersion: "10.0.0",
      });

      render(<AboutSettingsPanel />);
      const button = control("button", "Install");
      invoke(button, "onClick");
      expect(confirmSpy).not.toHaveBeenCalled();
      expect(bridge.installUpdate).not.toHaveBeenCalled();
    });

    it("shows the downloading state as disabled", () => {
      stubDesktopWindow({ checkForUpdate: vi.fn() });
      h.updateState = makeUpdateState({
        status: "downloading",
        availableVersion: "10.0.0",
        downloadPercent: 42,
      });

      const markup = render(<AboutSettingsPanel />);
      expect(markup).toContain("Downloading…");
      expect(markup).toContain("Downloading update (42%)");
    });
  });
});

describe("useGeneralSettingsRestore", () => {
  interface RestoreProbeResult {
    changedSettingLabels: ReadonlyArray<string>;
    restoreDefaults: () => Promise<void>;
  }

  function captureRestore(onRestored?: () => void): RestoreProbeResult {
    let captured: RestoreProbeResult | null = null;
    function Probe() {
      captured = useGeneralSettingsRestore(onRestored) as RestoreProbeResult;
      return null;
    }
    render(<Probe />);
    if (captured === null) {
      throw new Error("useGeneralSettingsRestore probe did not render");
    }
    return captured;
  }

  it("reports no changed settings for defaults and skips the confirm dialog", async () => {
    const confirmSpy = vi.fn(async () => true);
    h.localApi = { dialogs: { confirm: confirmSpy } };
    const restore = captureRestore();

    expect(restore.changedSettingLabels).toEqual([]);
    await restore.restoreDefaults();
    expect(confirmSpy).not.toHaveBeenCalled();
    expect(h.updateSettings).not.toHaveBeenCalled();
  });

  it("lists only changed General setting labels", () => {
    h.theme = "dark";
    h.settings = changedSettings();
    const restore = captureRestore();

    expect(restore.changedSettingLabels).toEqual([
      "Theme",
      "Time format",
      "Word wrap",
      "Diff whitespace changes",
      "Assistant output",
      "Provider update checks",
      "Auto-open task panel",
      "New thread mode",
      "New worktrees start from origin",
      "Workspace",
      "Add project base directory",
      "Archive confirmation",
      "Delete confirmation",
    ]);
  });

  it("lists and resets the link target when it is not the default", async () => {
    h.settings = { ...DEFAULT_UNIFIED_SETTINGS, browserLinkTarget: "system" };
    h.localApi = { dialogs: { confirm: vi.fn(async () => true) } };
    const restore = captureRestore();

    expect(restore.changedSettingLabels).toEqual(["Open links in"]);
    await restore.restoreDefaults();
    expect(h.updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ browserLinkTarget: "app" }),
    );
  });

  it("leaves a hidden link target out of the restore list and patch", async () => {
    h.previewSupported = false;
    h.settings = { ...DEFAULT_UNIFIED_SETTINGS, browserLinkTarget: "system" };
    h.theme = "dark";
    h.localApi = { dialogs: { confirm: vi.fn(async () => true) } };
    const restore = captureRestore();

    expect(restore.changedSettingLabels).toEqual(["Theme"]);
    await restore.restoreDefaults();
    expect(h.updateSettings).toHaveBeenCalledWith(
      expect.not.objectContaining({ browserLinkTarget: expect.anything() }),
    );
  });

  it("restores defaults after confirmation and notifies the caller", async () => {
    h.theme = "dark";
    h.settings = changedSettings();
    const confirmSpy = vi.fn(async (_message: string) => true);
    h.localApi = { dialogs: { confirm: confirmSpy } };
    const onRestored = vi.fn();
    const restore = captureRestore(onRestored);

    await restore.restoreDefaults();

    expect(confirmSpy).toHaveBeenCalledTimes(1);
    expect(confirmSpy).toHaveBeenCalledWith(
      [
        "Restore General defaults?",
        "This will reset: Theme, Time format, Word wrap, Diff whitespace changes, Assistant output, Provider update checks, Auto-open task panel, New thread mode, New worktrees start from origin, Workspace, Add project base directory, Archive confirmation, Delete confirmation.",
      ].join("\n"),
    );
    expect(h.setTheme).toHaveBeenCalledWith("system");
    expect(h.updateSettings).toHaveBeenCalledWith({
      timestampFormat: DEFAULT_UNIFIED_SETTINGS.timestampFormat,
      browserLinkTarget: DEFAULT_UNIFIED_SETTINGS.browserLinkTarget,
      wordWrap: DEFAULT_UNIFIED_SETTINGS.wordWrap,
      diffIgnoreWhitespace: DEFAULT_UNIFIED_SETTINGS.diffIgnoreWhitespace,
      enableAssistantStreaming: DEFAULT_UNIFIED_SETTINGS.enableAssistantStreaming,
      enableProviderUpdateChecks: DEFAULT_UNIFIED_SETTINGS.enableProviderUpdateChecks,
      autoOpenPlanSidebar: DEFAULT_UNIFIED_SETTINGS.autoOpenPlanSidebar,
      defaultThreadEnvMode: DEFAULT_UNIFIED_SETTINGS.defaultThreadEnvMode,
      newWorktreesStartFromOrigin: DEFAULT_UNIFIED_SETTINGS.newWorktreesStartFromOrigin,
      worktreeBaseDirectory: "",
      addProjectBaseDirectory: DEFAULT_UNIFIED_SETTINGS.addProjectBaseDirectory,
      confirmThreadArchive: DEFAULT_UNIFIED_SETTINGS.confirmThreadArchive,
      confirmThreadDelete: DEFAULT_UNIFIED_SETTINGS.confirmThreadDelete,
    });
    expect(onRestored).toHaveBeenCalledTimes(1);
  });

  it("does nothing when the user cancels the confirmation", async () => {
    h.theme = "dark";
    h.settings = changedSettings();
    const confirmSpy = vi.fn(async () => false);
    h.localApi = { dialogs: { confirm: confirmSpy } };
    const restore = captureRestore();

    await restore.restoreDefaults();

    expect(confirmSpy).toHaveBeenCalledTimes(1);
    expect(h.setTheme).not.toHaveBeenCalled();
    expect(h.updateSettings).not.toHaveBeenCalled();
  });

  it("fails when no local API is available", async () => {
    h.theme = "dark";
    h.settings = changedSettings();
    h.localApi = undefined;
    const restore = captureRestore();

    await expect(restore.restoreDefaults()).rejects.toThrow("Local API not found");
  });
});

describe("ProviderSettingsPanel", () => {
  function baseProviderSettings(): UnifiedSettings {
    const futureDriver = ProviderDriverKind.make("futureDriver");
    return {
      ...DEFAULT_UNIFIED_SETTINGS,
      providerSessionDefaults: {
        [CODEX_DRIVER]: { model: "codex-default", options: [{ id: "fastMode", value: false }] },
        [CLAUDE_DRIVER]: { model: "claude-default", options: [{ id: "reasoning", value: "high" }] },
        [futureDriver]: { model: "future-default" },
      },
      providerInstances: {
        [ProviderInstanceId.make("codex")]: {
          driver: CODEX_DRIVER,
          enabled: true,
          config: { enabled: true },
        },
        [ProviderInstanceId.make("codex_personal")]: {
          driver: CODEX_DRIVER,
          displayName: "Personal Codex",
          enabled: true,
          config: { enabled: true },
        },
        [ProviderInstanceId.make("cursor_alt")]: {
          driver: CURSOR_DRIVER,
          displayName: "Cursor Sandbox",
          enabled: true,
          config: { enabled: true },
        },
      },
      providerModelPreferences: {
        [ProviderInstanceId.make("codex")]: {
          hiddenModels: ["model-hidden"],
          modelOrder: ["model-beta", "model-alpha"],
        },
      },
      favorites: [
        { provider: ProviderInstanceId.make("codex"), model: "model-alpha" },
        { provider: ProviderInstanceId.make("codex_personal"), model: "model-beta" },
      ],
    };
  }

  it("renders provider rows including custom and orphaned instances", () => {
    const settings = baseProviderSettings();
    h.settings = settings;
    h.serverProviders = [
      makeServerProvider({
        instanceId: "codex",
        driver: CODEX_DRIVER,
        behindLatest: true,
        checkedAt: "2026-06-27T09:00:00.000Z",
      }),
      makeServerProvider({ instanceId: "claudeAgent", driver: CLAUDE_DRIVER }),
    ];

    const markup = render(<ProviderSettingsPanel />);
    expect(markup).toContain("Providers");
    expect(markup).toContain("Checked");
    expect(h.sections.find((section) => section.title === "Providers")?.contentVariant).toBe(
      "stack",
    );
    expect(control("button", "Add provider instance").props.hidden).toBe(true);

    const cardIds = h.instanceCards.map((card) => String(card.instanceId));
    expect(cardIds).toEqual([
      "codex",
      "codex_personal",
      "claudeAgent",
      "cursor",
      "cursor_alt",
      "opencode",
    ]);

    const cursorCard = h.instanceCards.find((card) => String(card.instanceId) === "cursor")!;
    const opencodeCard = h.instanceCards.find((card) => String(card.instanceId) === "opencode")!;
    expect((cursorCard.driverOption as { badgeLabel?: string }).badgeLabel).toBeUndefined();
    expect((opencodeCard.driverOption as { badgeLabel?: string }).badgeLabel).toBeUndefined();

    const codexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
    expect(codexCard.headerAction).not.toBeNull();
    expect(codexCard.hiddenModels).toEqual(["model-hidden"]);
    expect(codexCard.favoriteModels).toEqual(["model-alpha"]);
    expect(codexCard.sessionDefaults).toEqual(settings.providerSessionDefaults[CODEX_DRIVER]);
    expect(typeof codexCard.onSessionDefaultsChange).toBe("function");

    const personalCard = h.instanceCards.find(
      (card) => String(card.instanceId) === "codex_personal",
    )!;
    expect(typeof personalCard.onDelete).toBe("function");
    expect(personalCard).not.toHaveProperty("sessionDefaults");
    expect(personalCard).not.toHaveProperty("onSessionDefaultsChange");
  });

  it("hides default and custom Grok provider rows", () => {
    const grokDriver = ProviderDriverKind.make("grok");
    const settings = baseProviderSettings();
    h.settings = {
      ...settings,
      providerInstances: {
        ...settings.providerInstances,
        [ProviderInstanceId.make("grok_work")]: {
          driver: grokDriver,
          displayName: "Work Grok",
          enabled: true,
          config: { enabled: true },
        },
      },
    };

    render(<ProviderSettingsPanel />);

    const cardIds = h.instanceCards.map((card) => String(card.instanceId));
    expect(cardIds).not.toContain("grok");
    expect(cardIds).not.toContain("grok_work");
  });

  it("replaces only the edited driver session default", () => {
    const settings = baseProviderSettings();
    h.settings = settings;

    render(<ProviderSettingsPanel />);
    const codexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
    const nextCodexDefault = {
      model: "codex-next",
      options: [{ id: "reasoning", value: "medium" }],
    };

    (codexCard.onSessionDefaultsChange as (next: typeof nextCodexDefault) => void)(
      nextCodexDefault,
    );

    expect(h.updateSettings).toHaveBeenCalledWith({
      providerSessionDefaults: {
        ...settings.providerSessionDefaults,
        [CODEX_DRIVER]: nextCodexDefault,
      },
    });
  });

  it("composes rapid session-default edits across providers before settings acknowledgment", () => {
    const settings = baseProviderSettings();
    h.settings = settings;

    render(<ProviderSettingsPanel />);
    const codexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
    const claudeCard = h.instanceCards.find((card) => String(card.instanceId) === "claudeAgent")!;
    const nextCodexDefault = {
      model: "codex-next",
      options: [{ id: "reasoningEffort", value: "xhigh" }],
    };
    const nextClaudeDefault = {
      model: "claude-next",
      options: [{ id: "effort", value: "ultrathink" }],
    };

    (codexCard.onSessionDefaultsChange as (next: typeof nextCodexDefault) => void)(
      nextCodexDefault,
    );
    (claudeCard.onSessionDefaultsChange as (next: typeof nextClaudeDefault) => void)(
      nextClaudeDefault,
    );

    expect(h.updateSettings).toHaveBeenNthCalledWith(1, {
      providerSessionDefaults: {
        ...settings.providerSessionDefaults,
        [CODEX_DRIVER]: nextCodexDefault,
      },
    });
    expect(h.updateSettings).toHaveBeenNthCalledWith(2, {
      providerSessionDefaults: {
        ...settings.providerSessionDefaults,
        [CODEX_DRIVER]: nextCodexDefault,
        [CLAUDE_DRIVER]: nextClaudeDefault,
      },
    });
  });

  it("keeps pending provider edits over stale acknowledgments and clears them at convergence", () => {
    const createDraft = (
      settingsPanelsLogic as typeof settingsPanelsLogic & {
        createProviderSessionDefaultsDraft?: (
          initial: UnifiedSettings["providerSessionDefaults"],
        ) => {
          readonly submit: (
            driver: ProviderDriverKind,
            next: UnifiedSettings["providerSessionDefaults"][ProviderDriverKind],
          ) => {
            readonly revision: object;
            readonly defaults: UnifiedSettings["providerSessionDefaults"];
          };
          readonly reconcile: (
            authoritative: UnifiedSettings["providerSessionDefaults"],
          ) => UnifiedSettings["providerSessionDefaults"];
        };
      }
    ).createProviderSessionDefaultsDraft;
    expect(createDraft).toBeTypeOf("function");
    if (!createDraft) return;

    const initial = baseProviderSettings().providerSessionDefaults;
    const draft = createDraft(initial);
    const nextCodex = { model: "codex-next" };
    const nextClaude = { model: "claude-next" };
    const firstSubmission = draft.submit(CODEX_DRIVER, nextCodex);
    const latestSubmission = draft.submit(CLAUDE_DRIVER, nextClaude);

    expect(draft.reconcile(firstSubmission.defaults)).toEqual(latestSubmission.defaults);
    expect(draft.reconcile(latestSubmission.defaults)).toEqual(latestSubmission.defaults);
    expect(
      draft.reconcile({
        ...latestSubmission.defaults,
        [ProviderDriverKind.make("futureDriver")]: { model: "future-external" },
      }),
    ).toEqual({
      ...latestSubmission.defaults,
      [ProviderDriverKind.make("futureDriver")]: { model: "future-external" },
    });
  });

  it("resets displayed provider defaults when the primary environment changes", async () => {
    const previousActEnvironment = (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean })
      .IS_REACT_ACT_ENVIRONMENT;
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const firstEnvironmentId = EnvironmentId.make("environment-first");
    const secondEnvironmentId = EnvironmentId.make("environment-second");
    const firstSettings = baseProviderSettings();
    const secondSettings: UnifiedSettings = {
      ...baseProviderSettings(),
      providerSessionDefaults: {
        ...baseProviderSettings().providerSessionDefaults,
        [CODEX_DRIVER]: { model: "codex-second-environment" },
      },
    };
    h.primaryEnvironment = { environmentId: firstEnvironmentId };
    h.settings = firstSettings;
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);

    try {
      await act(async () => root.render(<ProviderSettingsPanel />));
      const firstCodexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
      (firstCodexCard.onSessionDefaultsChange as (next: ProviderSessionDefault) => void)({
        model: "codex-pending-first-environment",
      });

      h.primaryEnvironment = { environmentId: secondEnvironmentId };
      h.settings = secondSettings;
      clearRegistries();
      await act(async () => root.render(<ProviderSettingsPanel />));

      const secondCodexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
      expect(secondCodexCard.sessionDefaults).toEqual(
        secondSettings.providerSessionDefaults[CODEX_DRIVER],
      );
      expect(secondCodexCard.sessionDefaults).not.toEqual({
        model: "codex-pending-first-environment",
      });
    } finally {
      await act(async () => root.unmount());
      container.remove();
      if (previousActEnvironment === undefined) {
        Reflect.deleteProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT");
      } else {
        (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
          previousActEnvironment;
      }
    }
  });

  it("rolls a failed latest provider-default mutation back to its acknowledged predecessor", async () => {
    const settings = baseProviderSettings();
    h.settings = settings;
    const firstResult = deferred<unknown>();
    const secondResult = deferred<unknown>();
    h.updateSettings
      .mockReturnValueOnce(firstResult.promise)
      .mockReturnValueOnce(secondResult.promise);

    render(<ProviderSettingsPanel />);
    const codexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
    const claudeCard = h.instanceCards.find((card) => String(card.instanceId) === "claudeAgent")!;
    const submittedS1 = { model: "codex-s1" };
    const submittedS2 = { model: "codex-s2" };
    (codexCard.onSessionDefaultsChange as (next: ProviderSessionDefault) => void)(submittedS1);
    (codexCard.onSessionDefaultsChange as (next: ProviderSessionDefault) => void)(submittedS2);

    firstResult.resolve(
      AsyncResult.success({
        ...settings,
        providerSessionDefaults: {
          ...settings.providerSessionDefaults,
          [CODEX_DRIVER]: submittedS1,
        },
      }),
    );
    await flush();
    secondResult.resolve(AsyncResult.failure(Cause.fail(new Error("S2 rejected"))));
    await flush();

    (claudeCard.onSessionDefaultsChange as (next: ProviderSessionDefault) => void)({
      model: "claude-after-failure",
    });
    expect(h.updateSettings).toHaveBeenNthCalledWith(3, {
      providerSessionDefaults: {
        ...settings.providerSessionDefaults,
        [CODEX_DRIVER]: submittedS1,
        [CLAUDE_DRIVER]: { model: "claude-after-failure" },
      },
    });
  });

  it("keeps the latest provider default when an older mutation fails", async () => {
    const settings = baseProviderSettings();
    h.settings = settings;
    const firstResult = deferred<unknown>();
    const secondResult = deferred<unknown>();
    h.updateSettings
      .mockReturnValueOnce(firstResult.promise)
      .mockReturnValueOnce(secondResult.promise);

    render(<ProviderSettingsPanel />);
    const codexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
    const claudeCard = h.instanceCards.find((card) => String(card.instanceId) === "claudeAgent")!;
    const submittedS2 = { model: "codex-s2" };
    (codexCard.onSessionDefaultsChange as (next: ProviderSessionDefault) => void)({
      model: "codex-s1",
    });
    (codexCard.onSessionDefaultsChange as (next: ProviderSessionDefault) => void)(submittedS2);

    firstResult.resolve(AsyncResult.failure(Cause.fail(new Error("S1 rejected"))));
    await flush();
    (claudeCard.onSessionDefaultsChange as (next: ProviderSessionDefault) => void)({
      model: "claude-after-failure",
    });

    expect(h.updateSettings).toHaveBeenNthCalledWith(3, {
      providerSessionDefaults: {
        ...settings.providerSessionDefaults,
        [CODEX_DRIVER]: submittedS2,
        [CLAUDE_DRIVER]: { model: "claude-after-failure" },
      },
    });
  });

  it("does not let an older Success response overwrite newer stream authority", async () => {
    const previousActEnvironment = (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean })
      .IS_REACT_ACT_ENVIRONMENT;
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    h.dispatchStateUpdates = true;
    const settings = baseProviderSettings();
    h.settings = settings;
    const mutationResult = deferred<unknown>();
    h.updateSettings.mockReturnValueOnce(mutationResult.promise);
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);

    try {
      await act(async () => root.render(<ProviderSettingsPanel />));
      const codexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
      const submitted = { model: "codex-submitted" };
      await act(async () => {
        (codexCard.onSessionDefaultsChange as (next: ProviderSessionDefault) => void)(submitted);
      });

      const external = { model: "codex-newer-stream-authority" };
      const rerenderCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
      h.settings = {
        ...settings,
        providerSessionDefaults: {
          ...settings.providerSessionDefaults,
          [CODEX_DRIVER]: external,
        },
      };
      clearRegistries();
      await act(async () => {
        (rerenderCard.onExpandedChange as (open: boolean) => void)(true);
      });
      const claudeCard = h.instanceCards.find((card) => String(card.instanceId) === "claudeAgent")!;

      await act(async () => {
        mutationResult.resolve(
          AsyncResult.success({
            ...settings,
            providerSessionDefaults: {
              ...settings.providerSessionDefaults,
              [CODEX_DRIVER]: submitted,
            },
          }),
        );
        await flush();
      });
      await act(async () => {
        (claudeCard.onSessionDefaultsChange as (next: ProviderSessionDefault) => void)({
          model: "claude-after-success",
        });
      });

      expect(h.updateSettings).toHaveBeenNthCalledWith(2, {
        providerSessionDefaults: {
          ...settings.providerSessionDefaults,
          [CODEX_DRIVER]: external,
          [CLAUDE_DRIVER]: { model: "claude-after-success" },
        },
      });
    } finally {
      await act(async () => root.unmount());
      container.remove();
      h.dispatchStateUpdates = false;
      if (previousActEnvironment === undefined) {
        Reflect.deleteProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT");
      } else {
        (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
          previousActEnvironment;
      }
    }
  });

  it("shows provider enabled edits before the settings stream acknowledges them", async () => {
    const previousActEnvironment = (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean })
      .IS_REACT_ACT_ENVIRONMENT;
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    h.dispatchStateUpdates = true;
    h.settings = baseProviderSettings();
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);

    try {
      await act(async () => root.render(<ProviderSettingsPanel />));
      const codexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;

      await act(async () => {
        (codexCard.onUpdate as (next: unknown) => void)({
          ...(codexCard.instance as ProviderInstanceConfig),
          enabled: false,
        });
      });

      const latestCodexCard = h.instanceCards.findLast(
        (card) => String(card.instanceId) === "codex",
      )!;
      expect(latestCodexCard.instance).toEqual(expect.objectContaining({ enabled: false }));
    } finally {
      await act(async () => root.unmount());
      container.remove();
      h.dispatchStateUpdates = false;
      if (previousActEnvironment === undefined) {
        Reflect.deleteProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT");
      } else {
        (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
          previousActEnvironment;
      }
    }
  });

  it("refreshes Claude usage only after a disabled Claude instance is enabled successfully", async () => {
    const settings = baseProviderSettings();
    const disabledClaude: ProviderInstanceConfig = {
      driver: CLAUDE_DRIVER,
      enabled: false,
      config: { enabled: false },
    };
    h.settings = {
      ...settings,
      providerInstances: {
        ...settings.providerInstances,
        [ProviderInstanceId.make("claudeAgent")]: disabledClaude,
      },
    };
    const mutation = deferred<unknown>();
    const usageRefresh = deferred<unknown>();
    h.updateSettings.mockReturnValue(mutation.promise);
    h.refreshProviderUsageCommand.mockReturnValue(usageRefresh.promise);

    render(<ProviderSettingsPanel />);
    const claudeCard = h.instanceCards.find((card) => String(card.instanceId) === "claudeAgent")!;
    (claudeCard.onUpdate as (next: ProviderInstanceConfig) => void)({
      ...disabledClaude,
      enabled: true,
      config: { enabled: true },
    });

    expect(h.refreshProviderUsageCommand).not.toHaveBeenCalled();
    expect(h.providerUsageQueryRefresh).not.toHaveBeenCalled();

    mutation.resolve(AsyncResult.success(h.settings as UnifiedSettings));
    await flush();

    expect(h.refreshProviderUsageCommand).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { providers: ["claude"], force: true },
    });
    expect(h.providerUsageQueryRefresh).not.toHaveBeenCalled();

    usageRefresh.resolve(AsyncResult.success({ providers: [] }));
    await flush();

    expect(h.providerUsageQueryRefresh).toHaveBeenCalledTimes(1);
  });

  it("does not refresh Claude usage for failed or unrelated provider transitions", async () => {
    const settings = baseProviderSettings();
    const exercise = async ({
      instanceId,
      current,
      next,
      result,
    }: {
      readonly instanceId: string;
      readonly current: ProviderInstanceConfig;
      readonly next: ProviderInstanceConfig;
      readonly result: unknown;
    }) => {
      h.settings = {
        ...settings,
        providerInstances: {
          ...settings.providerInstances,
          [ProviderInstanceId.make(instanceId)]: current,
        },
      };
      h.updateSettings.mockReset().mockResolvedValue(result);
      h.refreshProviderUsageCommand.mockClear();
      h.providerUsageQueryRefresh.mockClear();
      render(<ProviderSettingsPanel />);
      const card = h.instanceCards.find(
        (candidate) => String(candidate.instanceId) === instanceId,
      )!;
      (card.onUpdate as (updated: ProviderInstanceConfig) => void)(next);
      await flush();
      expect(h.refreshProviderUsageCommand).not.toHaveBeenCalled();
      expect(h.providerUsageQueryRefresh).not.toHaveBeenCalled();
    };
    const disabledClaude: ProviderInstanceConfig = {
      driver: CLAUDE_DRIVER,
      enabled: false,
      config: { enabled: false },
    };
    const enabledClaude: ProviderInstanceConfig = {
      driver: CLAUDE_DRIVER,
      enabled: true,
      config: { enabled: true },
    };

    await exercise({
      instanceId: "claudeAgent",
      current: disabledClaude,
      next: enabledClaude,
      result: AsyncResult.failure(Cause.fail(new Error("settings rejected"))),
    });
    await exercise({
      instanceId: "claudeAgent",
      current: enabledClaude,
      next: disabledClaude,
      result: AsyncResult.success(h.settings as UnifiedSettings),
    });
    await exercise({
      instanceId: "claudeAgent",
      current: enabledClaude,
      next: { ...enabledClaude, displayName: "Claude Enterprise" },
      result: AsyncResult.success(h.settings as UnifiedSettings),
    });
    await exercise({
      instanceId: "codex",
      current: { driver: CODEX_DRIVER, enabled: false, config: { enabled: false } },
      next: { driver: CODEX_DRIVER, enabled: true, config: { enabled: true } },
      result: AsyncResult.success(h.settings as UnifiedSettings),
    });
  });

  it("composes provider instance edits before the settings stream acknowledges them", () => {
    h.settings = baseProviderSettings();

    render(<ProviderSettingsPanel />);
    const codexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
    const claudeCard = h.instanceCards.find((card) => String(card.instanceId) === "claudeAgent")!;

    (codexCard.onUpdate as (next: unknown) => void)({
      ...(codexCard.instance as ProviderInstanceConfig),
      enabled: false,
    });
    (claudeCard.onUpdate as (next: unknown) => void)({
      ...(claudeCard.instance as ProviderInstanceConfig),
      enabled: false,
    });

    const latestPatch = h.updateSettings.mock.calls[1]?.[0] as UnifiedSettings;
    expect(latestPatch.providerInstances[ProviderInstanceId.make("codex")]?.enabled).toBe(false);
    expect(latestPatch.providerInstances[ProviderInstanceId.make("claudeAgent")]?.enabled).toBe(
      false,
    );
  });

  it("routes instance card callbacks into settings patches", () => {
    h.settings = baseProviderSettings();
    h.serverProviders = [
      makeServerProvider({ instanceId: "codex", driver: CODEX_DRIVER, behindLatest: true }),
    ];

    render(<ProviderSettingsPanel />);
    const codexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
    const personalCard = h.instanceCards.find(
      (card) => String(card.instanceId) === "codex_personal",
    )!;

    // Disabling the instance that currently backs text generation clears the
    // text-generation model selection back to its default.
    (codexCard.onUpdate as (next: unknown) => void)({
      driver: CODEX_DRIVER,
      enabled: false,
      config: { enabled: false },
    });
    const disablePatch = h.updateSettings.mock.calls[0]?.[0] as Record<string, unknown>;
    expect(disablePatch.textGenerationModelSelection).toEqual(
      DEFAULT_UNIFIED_SETTINGS.textGenerationModelSelection,
    );
    expect(disablePatch.providerInstances).toBeDefined();

    (codexCard.onUpdate as (next: unknown) => void)({
      driver: CODEX_DRIVER,
      enabled: true,
      config: { enabled: true },
    });
    const enablePatch = h.updateSettings.mock.calls[1]?.[0] as Record<string, unknown>;
    expect(enablePatch.textGenerationModelSelection).toBeUndefined();

    (personalCard.onDelete as () => void)();
    const deletePatch = h.updateSettings.mock.calls[2]?.[0] as {
      providerInstances: Record<string, unknown>;
      favorites: ReadonlyArray<{ provider: string }>;
    };
    expect(Object.keys(deletePatch.providerInstances)).not.toContain("codex_personal");
    expect(deletePatch.favorites.every((entry) => entry.provider !== "codex_personal")).toBe(true);

    (codexCard.onHiddenModelsChange as (models: ReadonlyArray<string>) => void)([
      "model-x",
      "model-x",
      " ",
    ]);
    const hiddenPatch = h.updateSettings.mock.calls[3]?.[0] as {
      providerModelPreferences: Record<string, { hiddenModels: ReadonlyArray<string> }>;
    };
    expect(hiddenPatch.providerModelPreferences["codex"]?.hiddenModels).toEqual(["model-x"]);

    (codexCard.onModelOrderChange as (order: ReadonlyArray<string>) => void)([]);
    const orderPatch = h.updateSettings.mock.calls[4]?.[0] as {
      providerModelPreferences: Record<string, unknown>;
    };
    expect(orderPatch.providerModelPreferences["codex"]).toBeDefined();

    (codexCard.onFavoriteModelsChange as (models: ReadonlyArray<string>) => void)([
      "model-y",
      "",
      "model-y",
    ]);
    const favoritesPatch = h.updateSettings.mock.calls[5]?.[0] as {
      favorites: ReadonlyArray<{ provider: string; model: string }>;
    };
    expect(favoritesPatch.favorites).toContainEqual({ provider: "codex", model: "model-y" });
    expect(favoritesPatch.favorites).toContainEqual({
      provider: "codex_personal",
      model: "model-beta",
    });

    // The default-slot reset action restores the legacy provider defaults.
    const headerAction = codexCard.headerAction as ReactElement<{ onClick: () => void }>;
    headerAction.props.onClick();
    const resetPatch = h.updateSettings.mock.calls[6]?.[0] as Record<string, unknown>;
    expect(resetPatch.providers).toBeDefined();
    expect(Object.keys(resetPatch.providerInstances as Record<string, unknown>)).not.toContain(
      "codex",
    );

    (codexCard.onExpandedChange as (open: boolean) => void)(true);
  });

  it("runs one-click provider updates and reports failures", async () => {
    h.settings = baseProviderSettings();
    h.serverProviders = [
      makeServerProvider({ instanceId: "codex", driver: CODEX_DRIVER, behindLatest: true }),
    ];

    render(<ProviderSettingsPanel />);
    const codexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
    expect(typeof codexCard.onRunUpdate).toBe("function");
    expect(codexCard.isUpdating).toBe(false);

    (codexCard.onRunUpdate as () => void)();
    await flush();
    expect(h.updateProviderCommand).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { provider: CODEX_DRIVER, instanceId: ProviderInstanceId.make("codex") },
    });
    expect(h.toastAdd).not.toHaveBeenCalled();

    h.updateProviderCommand.mockResolvedValueOnce({
      _tag: "Failure",
      cause: new Error("update exploded"),
    });
    (codexCard.onRunUpdate as () => void)();
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Could not update Codex",
        description: "update exploded",
      }),
    );

    h.updateProviderCommand.mockResolvedValueOnce({ _tag: "Failure", interrupted: true });
    (codexCard.onRunUpdate as () => void)();
    await flush();
    expect(h.toastAdd).toHaveBeenCalledTimes(1);
  });

  it("marks a provider as updating while its update command is active", () => {
    h.settings = baseProviderSettings();
    h.serverProviders = [
      makeServerProvider({
        instanceId: "codex",
        driver: CODEX_DRIVER,
        behindLatest: true,
        updateRunning: true,
      }),
    ];

    render(<ProviderSettingsPanel />);
    const codexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
    expect(codexCard.isUpdating).toBe(true);
    // The one-click update guard refuses to start while an update is running.
    (codexCard.onRunUpdate as () => void)();
    expect(h.updateProviderCommand).not.toHaveBeenCalled();
  });

  it("refreshes provider status once at a time", async () => {
    h.settings = baseProviderSettings();
    h.serverProviders = [makeServerProvider({ instanceId: "codex", driver: CODEX_DRIVER })];
    let resolveRefresh: (value: unknown) => void = () => {};
    h.refreshProvidersCommand.mockReturnValue(
      new Promise((resolve) => {
        resolveRefresh = resolve;
      }),
    );

    render(<ProviderSettingsPanel />);
    const refreshButton = control("button", "Refresh provider status");
    invoke(refreshButton, "onClick");
    invoke(refreshButton, "onClick");
    expect(h.refreshProvidersCommand).toHaveBeenCalledTimes(1);

    resolveRefresh({ _tag: "Success", value: { providers: [] } });
    await flush();

    invoke(control("button", "Add provider instance"), "onClick");
  });

  it("logs refresh failures without toasting", async () => {
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    h.settings = baseProviderSettings();
    h.serverProviders = [makeServerProvider({ instanceId: "codex", driver: CODEX_DRIVER })];
    h.refreshProvidersCommand.mockResolvedValue({
      _tag: "Failure",
      cause: new Error("refresh failed"),
    });

    render(<ProviderSettingsPanel />);
    invoke(control("button", "Refresh provider status"), "onClick");
    await flush();
    expect(warnSpy).toHaveBeenCalledWith("Failed to refresh providers", expect.anything());
    expect(h.toastAdd).not.toHaveBeenCalled();
    warnSpy.mockRestore();
  });

  it("skips refresh and updates when no primary environment is connected", async () => {
    h.primaryEnvironment = null;
    h.settings = baseProviderSettings();
    h.serverProviders = [
      makeServerProvider({ instanceId: "codex", driver: CODEX_DRIVER, behindLatest: true }),
    ];

    render(<ProviderSettingsPanel />);
    invoke(control("button", "Refresh provider status"), "onClick");
    await flush();
    expect(h.refreshProvidersCommand).not.toHaveBeenCalled();

    const codexCard = h.instanceCards.find((card) => String(card.instanceId) === "codex")!;
    (codexCard.onRunUpdate as () => void)();
    await flush();
    expect(h.updateProviderCommand).not.toHaveBeenCalled();
  });
});

describe("ArchivedThreadsPanel", () => {
  const env1 = EnvironmentId.make("environment-one");
  const env2 = EnvironmentId.make("environment-two");
  const removedTarget = {
    environmentId: env1,
    projectId: "project-a",
    threadId: "thread-new",
    title: "Newer thread",
    path: "/work/alpha-worktrees/feature",
    branch: "feature/archived",
    availability: "missing-registered",
    registrationState: "prunable",
    locked: false,
  } as const;
  const removedResult = {
    threadRemoved: true,
    gitOutcome: "removed",
    orphanCleanupPending: false,
  } as const;

  function reportRemoval(): void {
    invoke(control("dialog", "worktree-removal"), "onRemoved", removedTarget, removedResult);
  }

  function archivedSnapshots() {
    return [
      {
        environmentId: env1,
        snapshot: {
          projects: [{ id: "project-a", title: "Alpha Project", workspaceRoot: "/work/alpha" }],
          threads: [
            {
              id: "thread-old",
              projectId: "project-a",
              title: "Older thread",
              archivedAt: "2026-06-01T00:00:00.000Z",
              createdAt: "2026-05-01T00:00:00.000Z",
            },
            {
              id: "thread-new",
              projectId: "project-a",
              title: "Newer thread",
              archivedAt: null,
              createdAt: "2026-06-20T00:00:00.000Z",
            },
            {
              id: "panel-closed",
              projectId: "project-a",
              title: "Panel — Closed chat",
              kind: "panel",
              hostThreadId: "thread-new",
              archivedAt: "2026-06-21T00:00:00.000Z",
              createdAt: "2026-06-20T00:00:00.000Z",
            },
          ],
        },
      },
      {
        environmentId: env2,
        snapshot: {
          projects: [
            { id: "project-b", title: "Beta Project", workspaceRoot: "/work/beta" },
            { id: "project-empty", title: "No Threads", workspaceRoot: "/work/none" },
          ],
          threads: [
            {
              id: "thread-b",
              projectId: "project-b",
              title: "Beta thread",
              archivedAt: "2026-06-10T00:00:00.000Z",
              createdAt: "2026-06-05T00:00:00.000Z",
            },
          ],
        },
      },
    ];
  }

  beforeEach(() => {
    h.projects = [{ environmentId: env1 }, { environmentId: env2 }];
    h.serverConfigs = new Map([
      [env1, { environment: { capabilities: { worktreeCatalog: true } } }],
      [env2, { environment: { capabilities: { worktreeCatalog: true } } }],
    ]);
  });

  it("shows the loading state", () => {
    h.archive.isLoading = true;
    const markup = render(<ArchivedThreadsPanel />);
    expect(markup).toContain("Loading archived threads");
    expect(markup).toContain("Checking connected environments.");
  });

  it("shows the error state", () => {
    h.archive.error = "connection lost";
    const markup = render(<ArchivedThreadsPanel />);
    expect(markup).toContain("Could not load archived threads");
    expect(markup).toContain("connection lost");
  });

  it("shows the empty state", () => {
    const markup = render(<ArchivedThreadsPanel />);
    expect(markup).toContain("No archived threads");
    expect(markup).toContain("Archived threads will appear here.");
  });

  it("groups archived threads by project, newest first", () => {
    h.archive.snapshots = archivedSnapshots();
    const markup = render(<ArchivedThreadsPanel />);

    expect(markup).toContain("Alpha Project");
    expect(markup).toContain("Beta Project");
    expect(markup).not.toContain("No Threads");
    // Newest archive key (createdAt fallback) sorts first.
    expect(markup.indexOf("Newer thread")).toBeLessThan(markup.indexOf("Older thread"));
    // Closed chat panels are reopened from their host's + menu, not listed here.
    expect(markup).not.toContain("Closed chat");
    expect(markup).toContain("Archived");
    expect(markup).toContain("Created");
  });

  it("unarchives a thread and refreshes on success", async () => {
    h.archive.snapshots = archivedSnapshots();
    render(<ArchivedThreadsPanel />);

    const unarchiveButtons = findControls("button", "Unarchive");
    expect(unarchiveButtons.length).toBe(3);
    invoke(unarchiveButtons[0]!, "onClick");
    await flush();
    expect(h.unarchiveThread).toHaveBeenCalledWith({
      environmentId: env1,
      threadId: "thread-new",
    });
    expect(h.archive.refresh).toHaveBeenCalledTimes(1);
  });

  it("surfaces unarchive failures as toasts", async () => {
    h.archive.snapshots = archivedSnapshots();
    h.unarchiveThread.mockResolvedValue({ _tag: "Failure", cause: new Error("nope") });
    render(<ArchivedThreadsPanel />);

    invoke(findControls("button", "Unarchive")[0]!, "onClick");
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Failed to unarchive thread", description: "nope" }),
    );
    expect(h.archive.refresh).not.toHaveBeenCalled();

    h.toastAdd.mockClear();
    h.unarchiveThread.mockResolvedValue({ _tag: "Failure", interrupted: true });
    invoke(findControls("button", "Unarchive")[1]!, "onClick");
    await flush();
    expect(h.toastAdd).not.toHaveBeenCalled();
  });

  it("drives the context menu actions", async () => {
    h.archive.snapshots = archivedSnapshots();
    const showSpy = vi.fn();
    h.localApi = { contextMenu: { show: showSpy } };
    render(<ArchivedThreadsPanel />);

    const row = h.rows.find((entry) => typeof entry.onContextMenu === "function");
    expect(row).toBeDefined();
    const fireContextMenu = () =>
      (row!.onContextMenu as (event: unknown) => void)({
        preventDefault: () => {},
        clientX: 11,
        clientY: 22,
      });

    // Unarchive via context menu.
    showSpy.mockResolvedValueOnce("unarchive");
    fireContextMenu();
    await flush();
    expect(showSpy).toHaveBeenCalledWith(
      [
        { id: "unarchive", label: "Unarchive" },
        { id: "delete", label: "Delete", destructive: true },
      ],
      { x: 11, y: 22 },
    );
    expect(h.unarchiveThread).toHaveBeenCalledTimes(1);
    expect(h.archive.refresh).toHaveBeenCalledTimes(1);

    // Unarchive failure toasts.
    showSpy.mockResolvedValueOnce("unarchive");
    h.unarchiveThread.mockResolvedValueOnce({ _tag: "Failure", cause: new Error("locked") });
    fireContextMenu();
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Failed to unarchive thread", description: "locked" }),
    );

    // Delete via context menu.
    showSpy.mockResolvedValueOnce("delete");
    fireContextMenu();
    await flush();
    expect(h.confirmAndDeleteThread).toHaveBeenCalledTimes(1);
    expect(h.archive.refresh).toHaveBeenCalledTimes(2);

    // Delete failure toasts.
    showSpy.mockResolvedValueOnce("delete");
    h.confirmAndDeleteThread.mockResolvedValueOnce({
      _tag: "Failure",
      cause: new Error("cannot delete"),
    });
    fireContextMenu();
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Failed to delete thread", description: "cannot delete" }),
    );

    // Dismissing the menu does nothing.
    showSpy.mockResolvedValueOnce(null);
    fireContextMenu();
    await flush();
    expect(h.unarchiveThread).toHaveBeenCalledTimes(2);
    expect(h.confirmAndDeleteThread).toHaveBeenCalledTimes(2);

    // The context-menu handler itself failing surfaces a toast.
    showSpy.mockRejectedValueOnce(new Error("menu exploded"));
    fireContextMenu();
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Archived thread action failed",
        description: "menu exploded",
      }),
    );
  });

  it("opens the reusable removal dialog for an archived worktree without deleting by path", async () => {
    const snapshots = archivedSnapshots() as any[];
    const archivedThread = snapshots[0].snapshot.threads[1];
    archivedThread.worktreePath = "/work/alpha-worktrees/feature";
    archivedThread.branch = "feature/archived";
    h.archive.snapshots = snapshots;
    const showSpy = vi.fn().mockResolvedValue("delete");
    h.localApi = { contextMenu: { show: showSpy } };
    render(<ArchivedThreadsPanel />);

    const row = h.rows.find(
      (entry) => entry.title === "Newer thread" && typeof entry.onContextMenu === "function",
    );
    expect(row).toBeDefined();
    (row!.onContextMenu as (event: unknown) => void)({
      preventDefault: () => {},
      clientX: 3,
      clientY: 4,
    });
    await flush();

    expect(h.requestWorktreeRemoval).toHaveBeenCalledWith({
      environmentId: env1,
      projectId: "project-a",
      threadId: "thread-new",
      title: "Newer thread",
      path: "/work/alpha-worktrees/feature",
      branch: "feature/archived",
      availability: "verification-unavailable",
      registrationState: null,
      locked: false,
    });
    expect(h.confirmAndDeleteThread).not.toHaveBeenCalled();
  });

  it("uses generic detach-only deletion for an archived worktree on an unsupported server", async () => {
    const snapshots = archivedSnapshots() as any[];
    const archivedThread = snapshots[0].snapshot.threads[1];
    archivedThread.worktreePath = "/work/alpha-worktrees/feature";
    archivedThread.branch = "feature/archived";
    h.archive.snapshots = snapshots;
    h.serverConfigs = new Map([
      [env1, { environment: { capabilities: { worktreeCatalog: false } } }],
      [env2, { environment: { capabilities: { worktreeCatalog: true } } }],
    ]);
    const confirm = vi.fn().mockResolvedValue(true);
    h.localApi = {
      contextMenu: { show: vi.fn().mockResolvedValue("delete") },
      dialogs: { confirm },
    };
    render(<ArchivedThreadsPanel />);

    const row = h.rows.find(
      (entry) => entry.title === "Newer thread" && typeof entry.onContextMenu === "function",
    );
    (row!.onContextMenu as (event: unknown) => void)({
      preventDefault: () => {},
      clientX: 3,
      clientY: 4,
    });
    await flush();

    expect(confirm).toHaveBeenCalledWith(expect.stringContaining("files will be left untouched"));
    expect(h.deleteThread).toHaveBeenCalledWith({
      environmentId: env1,
      threadId: "thread-new",
    });
    expect(h.confirmAndDeleteThread).not.toHaveBeenCalled();
    expect(h.requestWorktreeRemoval).not.toHaveBeenCalled();
  });

  it("surfaces a post-removal navigation failure without refreshing the archive", async () => {
    h.completeWorktreeRemoval.mockResolvedValueOnce({
      _tag: "Failure",
      cause: new Error("route unavailable"),
    });
    render(<ArchivedThreadsPanel />);

    reportRemoval();
    await flush();

    expect(h.toastAdd).toHaveBeenCalledTimes(1);
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        title: "Worktree removed, but navigation failed",
        description: "route unavailable",
      }),
    );
    expect(h.archive.refresh).not.toHaveBeenCalled();
  });

  it("refreshes once after successful post-removal cleanup", async () => {
    render(<ArchivedThreadsPanel />);

    reportRemoval();
    await flush();

    expect(h.archive.refresh).toHaveBeenCalledTimes(1);
    expect(h.toastAdd).not.toHaveBeenCalled();
  });

  it("does not refresh or toast for interrupted cleanup after unmount", async () => {
    const previousActEnvironment = (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean })
      .IS_REACT_ACT_ENVIRONMENT;
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const completion = deferred<unknown>();
    h.completeWorktreeRemoval.mockReturnValueOnce(completion.promise);
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);

    try {
      await act(async () => root.render(<ArchivedThreadsPanel />));
      reportRemoval();
      await act(async () => root.unmount());
      completion.resolve({ _tag: "Failure", interrupted: true });
      await flush();

      expect(h.archive.refresh).not.toHaveBeenCalled();
      expect(h.toastAdd).not.toHaveBeenCalled();
    } finally {
      container.remove();
      if (previousActEnvironment === undefined) {
        Reflect.deleteProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT");
      } else {
        (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
          previousActEnvironment;
      }
    }
  });

  it("ignores context menus when no local API is present", async () => {
    h.archive.snapshots = archivedSnapshots();
    h.localApi = undefined;
    render(<ArchivedThreadsPanel />);

    const row = h.rows.find((entry) => typeof entry.onContextMenu === "function");
    (row!.onContextMenu as (event: unknown) => void)({
      preventDefault: () => {},
      clientX: 0,
      clientY: 0,
    });
    await flush();
    expect(h.unarchiveThread).not.toHaveBeenCalled();
  });
});
