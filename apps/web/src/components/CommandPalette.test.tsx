/**
 * Unit tests for the CommandPalette mega-component.
 *
 * Strategy (same harness as GitActionsControl.test.tsx): React hooks are
 * partially mocked with a slot-based store so component state survives
 * repeated `renderToStaticMarkup` passes, while leaf UI primitives are
 * replaced by capture-mocks that record every props object. Tests drive the
 * palette by invoking the captured handlers (onExecuteItem, onValueChange,
 * onKeyDown, button onClick) directly and re-rendering.
 */
import { EnvironmentId, ProjectId, ProviderInstanceId, ThreadId } from "@bibcode/contracts";
import { DEFAULT_CLIENT_SETTINGS } from "@bibcode/contracts/settings";
import type {
  EnvironmentProject,
  EnvironmentThreadShell,
} from "@bibcode/client-runtime/state/shell";
import { createModelSelection } from "@bibcode/shared/model";
import type { Dispatch, ReactNode, SetStateAction } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type {
  CommandPaletteActionItem,
  CommandPaletteGroup,
  CommandPaletteSubmenuItem,
} from "./CommandPalette.logic";

type EffectCallback = () => void | (() => void);

const hooks = vi.hoisted(() => {
  let cursor = 0;
  let stateSlots = new Map<number, unknown>();
  let refSlots = new Map<number, { current: unknown }>();
  let cacheSlots = new Map<number, unknown[]>();

  return {
    effects: [] as EffectCallback[],
    beginRender() {
      cursor = 0;
      this.effects = [];
    },
    reset() {
      cursor = 0;
      stateSlots = new Map();
      refSlots = new Map();
      cacheSlots = new Map();
      this.effects = [];
    },
    runEffects(): Array<() => void> {
      const cleanups: Array<() => void> = [];
      for (const effect of this.effects) {
        const cleanup = effect();
        if (typeof cleanup === "function") {
          cleanups.push(cleanup);
        }
      }
      return cleanups;
    },
    useCallback<T>(callback: T): T {
      cursor += 1;
      return callback;
    },
    useMemo<T>(factory: () => T): T {
      cursor += 1;
      return factory();
    },
    useMemoCache(size: number): unknown[] {
      const index = cursor;
      cursor += 1;
      const existing = cacheSlots.get(index);
      if (existing) {
        return existing;
      }
      const slots = Array.from({ length: size }, () => Symbol.for("react.memo_cache_sentinel"));
      cacheSlots.set(index, slots);
      return slots;
    },
    useRef<T>(initialValue: T): { current: T } {
      const index = cursor;
      cursor += 1;
      const existing = refSlots.get(index);
      if (existing) {
        return existing as { current: T };
      }
      const ref = { current: initialValue };
      refSlots.set(index, ref);
      return ref;
    },
    useState<T>(initialValue: T | (() => T)): [T, Dispatch<SetStateAction<T>>] {
      const index = cursor;
      cursor += 1;
      if (!stateSlots.has(index)) {
        stateSlots.set(
          index,
          typeof initialValue === "function" ? (initialValue as () => T)() : initialValue,
        );
      }
      const setValue: Dispatch<SetStateAction<T>> = (nextValue) => {
        const previous = stateSlots.get(index) as T;
        stateSlots.set(
          index,
          typeof nextValue === "function" ? (nextValue as (value: T) => T)(previous) : nextValue,
        );
      };
      return [stateSlots.get(index) as T, setValue];
    },
    useReducer<S, A>(
      reducer: (state: S, action: A) => S,
      initialState: S,
    ): [S, (action: A) => void] {
      const index = cursor;
      cursor += 1;
      if (!stateSlots.has(index)) {
        stateSlots.set(index, initialState);
      }
      const dispatch = (action: A) => {
        stateSlots.set(index, reducer(stateSlots.get(index) as S, action));
      };
      return [stateSlots.get(index) as S, dispatch];
    },
    useEffect(effect: EffectCallback): void {
      cursor += 1;
      this.effects.push(effect);
    },
    useLayoutEffect(effect: EffectCallback): void {
      cursor += 1;
      this.effects.push(effect);
    },
    useDeferredValue<T>(value: T): T {
      cursor += 1;
      return value;
    },
  };
});

const captured = vi.hoisted(() => ({
  providers: [] as unknown[],
  commandDialogs: [] as unknown[],
  commandPopups: [] as unknown[],
  commands: [] as unknown[],
  commandInputs: [] as unknown[],
  results: [] as unknown[],
  addProjectDialogs: [] as unknown[],
  clear() {
    this.providers = [];
    this.commandDialogs = [];
    this.commandPopups = [];
    this.commands = [];
    this.commandInputs = [];
    this.results = [];
    this.addProjectDialogs = [];
  },
}));

const testState = vi.hoisted(() => ({
  navigate: vi.fn(),
  keybindings: { kind: "keybindings" } as unknown,
  shortcutCommand: null as string | null,
  shortcutCalls: [] as unknown[][],
  routeParams: {} as Record<string, string>,
  terminalSurfaceOpen: false,
  projects: [] as unknown[],
  threads: [] as unknown[],
  environments: [] as unknown[],
  activeEnvironmentId: null as EnvironmentId | null,
  activeThread: null as unknown,
  deviceAvailability: { known: true, supported: true, enabled: false },
  deviceAvailabilityFor: [] as unknown[],
  activeDraftThread: null as unknown,
  defaultProjectRef: null as unknown,
  handleNewThread: vi.fn(),
  startNewThread: vi.fn(),
  startNewThreadInProject: vi.fn(),
  clientSettings: null as unknown,
  composerHandleRef: { current: null as { focusAtEnd: () => void } | null },
  toast: {
    add: vi.fn(),
    update: vi.fn(),
    close: vi.fn(),
  },
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useCallback: hooks.useCallback,
    useMemo: hooks.useMemo,
    useRef: hooks.useRef,
    useState: hooks.useState,
    useReducer: hooks.useReducer.bind(hooks),
    useEffect: hooks.useEffect.bind(hooks),
    useLayoutEffect: hooks.useLayoutEffect.bind(hooks),
    useDeferredValue: hooks.useDeferredValue,
  };
});

vi.mock("react/compiler-runtime", () => ({
  c: hooks.useMemoCache,
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: () => testState.keybindings,
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => testState.navigate,
  useParams: (opts?: { select?: (params: Record<string, string>) => unknown }) =>
    opts?.select ? opts.select(testState.routeParams) : testState.routeParams,
}));

vi.mock("../commandPaletteContext", () => ({
  OpenAddProjectCommandPaletteProvider: (props: {
    openAddProject: () => void;
    children?: ReactNode;
  }) => {
    captured.providers.push(props);
    return <>{props.children}</>;
  },
}));

vi.mock("../composerHandleContext", () => ({
  ComposerHandleContext: (props: { children?: ReactNode }) => <>{props.children}</>,
  useComposerHandleContext: () => testState.composerHandleRef,
}));

vi.mock("../hooks/useHandleNewThread", () => ({
  useHandleNewThread: () => ({
    activeDraftThread: testState.activeDraftThread,
    activeThread: testState.activeThread,
    defaultProjectRef: testState.defaultProjectRef,
    handleNewThread: testState.handleNewThread,
  }),
}));

vi.mock("../hooks/useSettings", () => ({
  useClientSettings: () => testState.clientSettings,
}));

vi.mock("../state/entities", () => ({
  useProjects: () => testState.projects,
  useThreadShells: () => testState.threads,
  useActiveEnvironmentId: () => testState.activeEnvironmentId,
}));

vi.mock("../state/environments", () => ({
  useEnvironments: () => ({ environments: testState.environments }),
}));

vi.mock("../lib/chatThreadActions", () => ({
  startNewThreadFromContext: (...args: unknown[]) => testState.startNewThread(...args),
  startNewThreadInProjectFromContext: (...args: unknown[]) =>
    testState.startNewThreadInProject(...args),
}));

vi.mock("../lib/terminalFocus", () => ({
  isTerminalFocused: () => false,
}));

vi.mock("../terminalSurfaceState", () => ({
  useThreadHasTerminalSurface: () => testState.terminalSurfaceOpen,
}));

vi.mock("./device/deviceAvailability", () => ({
  useDevicePreviewsAvailability: (environmentId: unknown) => {
    testState.deviceAvailabilityFor.push(environmentId);
    return testState.deviceAvailability;
  },
}));

vi.mock("../state/server", () => ({
  primaryServerKeybindingsAtom: "atom:keybindings",
}));

vi.mock("../keybindings", () => ({
  resolveShortcutCommand: (...args: unknown[]) => {
    testState.shortcutCalls.push(args);
    return testState.shortcutCommand;
  },
}));

vi.mock("./AddProjectDialog", () => ({
  AddProjectDialog: (props: { open: boolean; onOpenChange: (open: boolean) => void }) => {
    captured.addProjectDialogs.push(props);
    return <div data-testid="add-project-dialog" data-open={props.open ? "true" : "false"} />;
  },
}));

vi.mock("./CommandPaletteResults", () => ({
  CommandPaletteResults: (props: Record<string, unknown>) => {
    captured.results.push(props);
    const groups = props["groups"] as ReadonlyArray<{ value: string }>;
    return (
      <div data-testid="palette-results">
        {groups.map((group) => group.value).join("|")}
        {(props["emptyStateMessage"] as string | undefined) ?? null}
      </div>
    );
  },
}));

vi.mock("./ProjectFavicon", () => ({
  ProjectFavicon: () => <span data-testid="project-favicon" />,
}));

vi.mock("./ThreadStatusIndicators", () => ({
  ThreadRowLeadingStatus: () => <span data-testid="leading-status" />,
  ThreadRowTrailingStatus: () => <span data-testid="trailing-status" />,
}));

vi.mock("./ui/command", () => ({
  Command: (props: Record<string, unknown>) => {
    captured.commands.push(props);
    return <div data-testid="command">{props["children"] as ReactNode}</div>;
  },
  CommandDialog: (props: Record<string, unknown>) => {
    captured.commandDialogs.push(props);
    return <div data-testid="command-dialog">{props["children"] as ReactNode}</div>;
  },
  CommandDialogPopup: (props: Record<string, unknown>) => {
    captured.commandPopups.push(props);
    return <div data-testid="command-popup">{props["children"] as ReactNode}</div>;
  },
  CommandFooter: (props: { children?: ReactNode }) => (
    <div data-testid="command-footer">{props.children}</div>
  ),
  CommandInput: (props: Record<string, unknown>) => {
    captured.commandInputs.push(props);
    return (
      <div data-testid="command-input">
        {props["startAddon"] as ReactNode}
        <span>{props["placeholder"] as string | undefined}</span>
      </div>
    );
  },
  CommandPanel: (props: { children?: ReactNode }) => (
    <div data-testid="command-panel">{props.children}</div>
  ),
}));

vi.mock("./ui/kbd", () => ({
  Kbd: (props: { children?: ReactNode }) => <kbd>{props.children}</kbd>,
  KbdGroup: (props: { children?: ReactNode }) => <span>{props.children}</span>,
}));

vi.mock("./ui/toast", () => ({
  toastManager: testState.toast,
  stackedThreadToast: (toast: Record<string, unknown>) => ({ ...toast, stacked: true }),
}));

import { CommandPalette, reduceCommandPaletteUiState } from "./CommandPalette";
import { scopeThreadRef } from "@bibcode/client-runtime/environment";
import { selectActiveRightPanel, useRightPanelStore } from "../rightPanelStore";

// ─────────────────────────────────────────────────────────────────────────────
// Fixtures
// ─────────────────────────────────────────────────────────────────────────────

const ENV_PRIMARY = EnvironmentId.make("env-primary");
const ENV_REMOTE = EnvironmentId.make("env-remote");
const PROJECT_A = ProjectId.make("project-a");
const PROJECT_B = ProjectId.make("project-b");

const NOW = Date.parse("2026-07-06T12:00:00.000Z");
const iso = (minutesAgo: number) => new Date(NOW - minutesAgo * 60_000).toISOString();

function makeProject(
  id: string,
  overrides: Partial<Omit<EnvironmentProject, "id">> = {},
): EnvironmentProject {
  return {
    id: ProjectId.make(id),
    title: "Repo A",
    workspaceRoot: "~/dev/repo-a",
    repositoryIdentity: null,
    defaultModelSelection: null,
    scripts: [],
    worktreeDiscovery: { visibility: "hidden", initialPromptDismissedAt: null, baselinePaths: [] },
    createdAt: iso(600),
    updatedAt: iso(60),
    environmentId: ENV_PRIMARY,
    ...overrides,
  };
}

function makeThread(
  id: string,
  overrides: Partial<Omit<EnvironmentThreadShell, "id">> = {},
): EnvironmentThreadShell {
  return {
    id: ThreadId.make(id),
    projectId: PROJECT_A,
    title: `Thread ${id}`,
    modelSelection: createModelSelection(ProviderInstanceId.make("codex"), "gpt-5-codex"),
    runtimeMode: "full-access",
    interactionMode: "default",
    branch: null,
    worktreePath: null,
    latestTurn: null,
    createdAt: iso(500),
    updatedAt: iso(50),
    archivedAt: null,
    session: null,
    latestUserMessageAt: iso(40),
    hasPendingApprovals: false,
    hasPendingUserInput: false,
    hasActionableProposedPlan: false,
    environmentId: ENV_PRIMARY,
    ...overrides,
  };
}

describe("command palette helpers", () => {
  it("reduces no-op palette intent changes", () => {
    const closed = { open: false, openIntent: null } as const;
    expect(reduceCommandPaletteUiState(closed, { _tag: "ClearOpenIntent" })).toBe(closed);
    expect(reduceCommandPaletteUiState(closed, { _tag: "SetOpen", open: true })).toEqual({
      open: true,
      openIntent: null,
    });
    expect(
      reduceCommandPaletteUiState(
        { open: true, openIntent: { kind: "add-project" } },
        { _tag: "SetOpen", open: false },
      ),
    ).toEqual({ open: false, openIntent: null });
  });
});

// ─────────────────────────────────────────────────────────────────────────────
// Render + capture helpers
// ─────────────────────────────────────────────────────────────────────────────

function render(): string {
  hooks.beginRender();
  captured.clear();
  return renderToStaticMarkup(
    <CommandPalette>
      <div data-testid="app-child" />
    </CommandPalette>,
  );
}

const rerender = render;

async function flushPromises(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

interface CapturedCommandDialogProps {
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}

interface CapturedCommandProps {
  value?: string;
  autoHighlight?: boolean | "always";
  onValueChange?: (value: string) => void;
}

interface FakeInputKeyEvent {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  preventDefault: () => void;
}

interface CapturedCommandInputProps {
  placeholder?: string;
  className?: string;
  startAddon?: ReactNode;
  onKeyDown?: (event: FakeInputKeyEvent) => void;
}

interface CapturedResultsProps {
  groups: ReadonlyArray<CommandPaletteGroup>;
  isActionsOnly: boolean;
  onExecuteItem: (item: CommandPaletteActionItem | CommandPaletteSubmenuItem) => void;
  emptyStateMessage?: string;
}

function lastCommandDialog(): CapturedCommandDialogProps {
  const props = captured.commandDialogs.at(-1);
  if (!props) throw new Error("CommandDialog was not rendered");
  return props as CapturedCommandDialogProps;
}

function lastCommand(): CapturedCommandProps {
  const props = captured.commands.at(-1);
  if (!props) throw new Error("Command was not rendered");
  return props as CapturedCommandProps;
}

function lastCommandInput(): CapturedCommandInputProps {
  const props = captured.commandInputs.at(-1);
  if (!props) throw new Error("CommandInput was not rendered");
  return props as CapturedCommandInputProps;
}

function resultsProps(): CapturedResultsProps {
  const props = captured.results.at(-1);
  if (!props) throw new Error("CommandPaletteResults was not rendered");
  return props as unknown as CapturedResultsProps;
}

function allItems(): Array<CommandPaletteActionItem | CommandPaletteSubmenuItem> {
  return resultsProps().groups.flatMap((group) => [...group.items]);
}

function itemByValue(
  value: string,
): CommandPaletteActionItem | CommandPaletteSubmenuItem | undefined {
  return allItems().find((item) => item.value === value);
}

function executeValue(value: string): void {
  const item = itemByValue(value);
  if (!item) {
    throw new Error(
      `Palette item "${value}" not found; available: ${allItems()
        .map((item) => item.value)
        .join(", ")}`,
    );
  }
  resultsProps().onExecuteItem(item);
}

function openPalette(): string {
  render();
  lastCommandDialog().onOpenChange?.(true);
  return rerender();
}

/** Depth-first search of a React element tree for the first onClick handler. */
function findFirstOnClick(node: unknown): (() => void) | null {
  if (node === null || node === undefined || typeof node !== "object") {
    return null;
  }
  if (Array.isArray(node)) {
    for (const child of node) {
      const handler = findFirstOnClick(child);
      if (handler) return handler;
    }
    return null;
  }
  const element = node as { props?: Record<string, unknown> };
  if (!element.props) {
    return null;
  }
  if (typeof element.props["onClick"] === "function") {
    return element.props["onClick"] as () => void;
  }
  for (const value of Object.values(element.props)) {
    const handler = findFirstOnClick(value);
    if (handler) return handler;
  }
  return null;
}

interface WindowStub {
  addEventListener: ReturnType<typeof vi.fn>;
  removeEventListener: ReturnType<typeof vi.fn>;
}

let windowStub: WindowStub;

function keydownHandler(): (event: {
  defaultPrevented: boolean;
  preventDefault: () => void;
  stopPropagation: () => void;
}) => void {
  const handler = windowStub.addEventListener.mock.calls.find(
    ([event]) => event === "keydown",
  )?.[1] as ((event: unknown) => void) | undefined;
  if (!handler) throw new Error("keydown handler not registered");
  return handler;
}

beforeEach(() => {
  hooks.reset();
  captured.clear();
  vi.clearAllMocks();
  testState.shortcutCommand = null;
  testState.shortcutCalls = [];
  testState.routeParams = {};
  testState.terminalSurfaceOpen = false;
  testState.projects = [
    makeProject("project-a"),
    makeProject("project-b", {
      title: "Repo B",
      workspaceRoot: "~/dev/repo-b",
    }),
  ];
  testState.threads = [makeThread("thread-1")];
  testState.environments = [
    {
      environmentId: ENV_PRIMARY,
      label: "Local",
      entry: { target: { _tag: "PrimaryConnectionTarget" } },
    },
    {
      environmentId: ENV_REMOTE,
      label: "AI-SERVER",
      entry: {
        target: { _tag: "BearerConnectionTarget", connectionId: "remote:server" },
      },
    },
  ];
  testState.activeEnvironmentId = null;
  testState.activeThread = makeThread("thread-1");
  testState.deviceAvailability = { known: true, supported: true, enabled: false };
  testState.deviceAvailabilityFor = [];
  testState.activeDraftThread = null;
  testState.defaultProjectRef = null;
  testState.clientSettings = DEFAULT_CLIENT_SETTINGS;
  testState.composerHandleRef = { current: { focusAtEnd: vi.fn() } };
  testState.navigate.mockResolvedValue(undefined);
  testState.handleNewThread.mockResolvedValue(undefined);
  testState.startNewThread.mockResolvedValue(undefined);
  testState.startNewThreadInProject.mockResolvedValue(undefined);

  windowStub = {
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  };
  vi.stubGlobal("window", windowStub);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

describe("palette shell", () => {
  it("renders only its children while closed", () => {
    const markup = render();
    expect(markup).toContain('data-testid="app-child"');
    expect(markup).not.toContain('data-testid="command-popup"');
  });

  it("opens and closes through the keyboard shortcut", () => {
    render();
    hooks.runEffects();
    const handler = keydownHandler();

    // Non-matching command: stays closed.
    testState.shortcutCommand = null;
    handler({ defaultPrevented: false, preventDefault: vi.fn(), stopPropagation: vi.fn() });
    expect(rerender()).not.toContain('data-testid="command-popup"');

    // defaultPrevented events are ignored entirely.
    testState.shortcutCommand = "commandPalette.toggle";
    handler({ defaultPrevented: true, preventDefault: vi.fn(), stopPropagation: vi.fn() });
    expect(rerender()).not.toContain('data-testid="command-popup"');

    // Matching command toggles open.
    const event = {
      defaultPrevented: false,
      preventDefault: vi.fn(),
      stopPropagation: vi.fn(),
    };
    handler(event);
    expect(event.preventDefault).toHaveBeenCalled();
    expect(event.stopPropagation).toHaveBeenCalled();
    expect(rerender()).toContain('data-testid="command-popup"');

    // And toggles closed again.
    handler({ defaultPrevented: false, preventDefault: vi.fn(), stopPropagation: vi.fn() });
    expect(rerender()).not.toContain('data-testid="command-popup"');
  });

  it("opens via the dialog onOpenChange and shows the root placeholder", () => {
    const markup = openPalette();
    expect(markup).toContain('data-testid="command-popup"');
    expect(lastCommandInput().placeholder).toBe("Search commands, projects, and threads...");
    expect(lastCommand().autoHighlight).toBe("always");
  });

  it("closes when the backdrop is pressed", () => {
    openPalette();
    const popup = captured.commandPopups.at(-1) as {
      onBackdropPointerDown?: () => void;
    };
    popup.onBackdropPointerDown?.();
    expect(rerender()).not.toContain('data-testid="command-popup"');
  });

  it("returns focus to the composer when the dialog closes", () => {
    openPalette();
    const popup = captured.commandPopups.at(-1) as {
      finalFocus?: () => boolean;
    };
    expect(popup.finalFocus?.()).toBe(false);
    expect(testState.composerHandleRef.current?.focusAtEnd).toHaveBeenCalledTimes(1);
  });

  it("routes every add-project intent to the sole dialog owner", () => {
    render();
    const provider = captured.providers.at(-1) as { openAddProject: () => void };
    provider.openAddProject();
    const queuedMarkup = rerender();

    expect(captured.addProjectDialogs.at(-1)).toEqual(expect.objectContaining({ open: false }));
    expect(queuedMarkup).toContain('data-testid="command-popup"');

    hooks.runEffects();
    const openedMarkup = rerender();
    expect(captured.addProjectDialogs.at(-1)).toEqual(expect.objectContaining({ open: true }));
    expect(openedMarkup).not.toContain('data-testid="command-popup"');
  });

  it("closes the add-project dialog through its onOpenChange", () => {
    openPalette();
    executeValue("action:add-project");
    rerender();
    const dialog = captured.addProjectDialogs.at(-1) as {
      open: boolean;
      onOpenChange: (open: boolean) => void;
    };
    expect(dialog.open).toBe(true);
    dialog.onOpenChange(false);
    expect(rerender()).toContain('data-open="false"');
  });

  it("passes active terminal surfaces to shortcut resolution", () => {
    testState.routeParams = { environmentId: "env-primary", threadId: "thread-1" };
    testState.terminalSurfaceOpen = true;
    render();
    hooks.runEffects();

    const event = { defaultPrevented: false };
    keydownHandler()(event as never);

    expect(testState.shortcutCalls.at(-1)?.[2]).toEqual(
      expect.objectContaining({
        context: expect.objectContaining({ terminalOpen: true }),
      }),
    );
  });
});

describe("root actions", () => {
  it("names Add project for the selected remote environment", () => {
    testState.activeEnvironmentId = ENV_REMOTE;

    openPalette();

    expect(itemByValue("action:add-project")?.title).toBe("Add project on AI-SERVER");
  });

  it("builds the root actions and recent threads", () => {
    openPalette();
    const groups = resultsProps().groups;
    expect(groups.map((group) => group.value)).toEqual(["actions", "recent-threads"]);
    const values = allItems().map((item) => item.value);
    expect(values).toContain("action:new-thread");
    expect(values).toContain("action:new-thread-in");
    expect(values).toContain("action:add-project");
    expect(values).toContain("action:settings");
    expect(values).toContain("thread:thread-1");
  });

  it("omits the active-project new-thread action without projects", () => {
    testState.projects = [];
    testState.threads = [];
    testState.activeThread = null;
    openPalette();
    const values = allItems().map((item) => item.value);
    expect(values).not.toContain("action:new-thread");
    expect(values).not.toContain("action:new-thread-in");
    expect(values).toContain("action:add-project");
  });

  it("offers the device preview only while device previews are on", async () => {
    openPalette();
    expect(allItems().map((item) => item.value)).not.toContain("action:open-device-preview");

    testState.deviceAvailability = { known: true, supported: true, enabled: true };
    rerender();
    expect(allItems().map((item) => item.value)).toContain("action:open-device-preview");
    executeValue("action:open-device-preview");
    await flushPromises();
    const thread = testState.activeThread as { environmentId: string; id: string };
    expect(testState.deviceAvailabilityFor).toContain(thread.environmentId);
    expect(
      selectActiveRightPanel(
        useRightPanelStore.getState().byThreadKey,
        scopeThreadRef(thread.environmentId as EnvironmentId, thread.id as ThreadId),
      ),
    ).toBe("device");
  });

  it("navigates to settings and closes the palette", async () => {
    openPalette();
    executeValue("action:settings");
    await flushPromises();
    expect(testState.navigate).toHaveBeenCalledWith({ to: "/settings" });
    expect(rerender()).not.toContain('data-testid="command-popup"');
  });

  it("starts a new thread in the active project", async () => {
    openPalette();
    executeValue("action:new-thread");
    await flushPromises();
    expect(testState.startNewThread).toHaveBeenCalledTimes(1);
  });

  it("drills into the new-thread-in submenu and starts a thread in a project", async () => {
    openPalette();
    executeValue("action:new-thread-in");
    rerender();
    expect(lastCommandInput().placeholder).toBe("Search...");
    executeValue(`new-thread-in:${ENV_PRIMARY}:${PROJECT_B}`);
    await flushPromises();
    expect(testState.startNewThreadInProject).toHaveBeenCalledTimes(1);
    const scopedRef = testState.startNewThreadInProject.mock.calls[0]?.[1] as {
      environmentId: string;
      projectId: string;
    };
    expect(scopedRef.projectId).toBe(PROJECT_B);
  });

  it("pops the submenu through the back addon and Backspace", () => {
    openPalette();
    executeValue("action:new-thread-in");
    rerender();
    const backAddon = findFirstOnClick(lastCommandInput().startAddon);
    expect(backAddon).not.toBeNull();
    backAddon?.();
    rerender();
    expect(resultsProps().groups.map((group) => group.value)).toContain("actions");

    // Re-enter and pop with Backspace on an empty query.
    executeValue("action:new-thread-in");
    rerender();
    lastCommandInput().onKeyDown?.({
      key: "Backspace",
      metaKey: false,
      ctrlKey: false,
      preventDefault: vi.fn(),
    });
    rerender();
    expect(resultsProps().groups.map((group) => group.value)).toContain("actions");
  });

  it("reports failing command runs as a toast", async () => {
    openPalette();
    resultsProps().onExecuteItem({
      kind: "action",
      value: "test:boom",
      searchTerms: [],
      title: "Boom",
      icon: null,
      run: () => Promise.reject(new Error("boom")),
    });
    await flushPromises();
    expect(testState.toast.add).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        title: "Unable to run command",
        description: "boom",
      }),
    );
  });

  it("ignores disabled items", async () => {
    openPalette();
    const run = vi.fn(() => Promise.resolve());
    resultsProps().onExecuteItem({
      kind: "action",
      value: "test:disabled",
      searchTerms: [],
      title: "Disabled",
      icon: null,
      disabled: true,
      run,
    });
    await flushPromises();
    expect(run).not.toHaveBeenCalled();
    expect(rerender()).toContain('data-testid="command-popup"');
  });

  it("filters to actions when the query starts with >", () => {
    openPalette();
    lastCommand().onValueChange?.(">");
    rerender();
    expect(resultsProps().isActionsOnly).toBe(true);
    expect(resultsProps().groups.map((group) => group.value)).toEqual(["actions"]);
  });

  it("opens a searched project at its latest thread", async () => {
    openPalette();
    lastCommand().onValueChange?.("Repo A");
    rerender();
    executeValue(`project:${ENV_PRIMARY}:${PROJECT_A}`);
    await flushPromises();
    expect(testState.navigate).toHaveBeenCalledWith({
      to: "/$environmentId/$threadId",
      params: { environmentId: ENV_PRIMARY, threadId: ThreadId.make("thread-1") },
    });
  });

  it("starts a new thread for a searched project without threads", async () => {
    openPalette();
    lastCommand().onValueChange?.("Repo B");
    rerender();
    executeValue(`project:${ENV_PRIMARY}:${PROJECT_B}`);
    await flushPromises();
    expect(testState.handleNewThread).toHaveBeenCalledWith(
      expect.objectContaining({ projectId: PROJECT_B }),
    );
  });

  it("navigates to a searched thread", async () => {
    openPalette();
    lastCommand().onValueChange?.("Thread thread-1");
    rerender();
    executeValue("thread:thread-1");
    await flushPromises();
    expect(testState.navigate).toHaveBeenCalledWith({
      to: "/$environmentId/$threadId",
      params: { environmentId: ENV_PRIMARY, threadId: ThreadId.make("thread-1") },
    });
  });
});
