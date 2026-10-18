import {
  EnvironmentId,
  ThreadId,
  type GitActionProgressEvent,
  type GitRunStackedActionResult,
  type ScopedThreadRef,
  type SourceControlDiscoveryResult,
  type SourceControlProviderDiscoveryItem,
  type SourceControlPublishRepositoryResult,
  type VcsPullResult,
  type VcsStatusResult,
} from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import * as Option from "effect/Option";
import { AsyncResult } from "effect/unstable/reactivity";
import type { Dispatch, ReactElement, ReactNode, SetStateAction } from "react";
import * as React from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { GitManagerCreatePullRequestDialogProps } from "./gitManager/provider/GitManagerCreatePullRequestDialog";

type EffectCallback = () => void | (() => void);

const browserRuntime =
  typeof document !== "undefined" && typeof document.createElement === "function";
const staticDescribe = browserRuntime ? describe.skip : describe;

const hooks = vi.hoisted(() => {
  let cursor = 0;
  let stateSlots = new Map<number, unknown>();
  let refSlots = new Map<number, { current: unknown }>();
  let cacheSlots = new Map<number, unknown[]>();
  let effectEventSlots = new Map<
    number,
    { cell: { current: (...args: unknown[]) => unknown }; proxy: (...args: unknown[]) => unknown }
  >();

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
      effectEventSlots = new Map();
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
    useEffect(effect: EffectCallback): void {
      cursor += 1;
      this.effects.push(effect);
    },
    useLayoutEffect(effect: EffectCallback): void {
      cursor += 1;
      this.effects.push(effect);
    },
    useEffectEvent<TArgs extends unknown[], TResult>(
      callback: (...args: TArgs) => TResult,
    ): (...args: TArgs) => TResult {
      const index = cursor;
      cursor += 1;
      let slot = effectEventSlots.get(index);
      if (!slot) {
        const cell = { current: callback as (...args: unknown[]) => unknown };
        slot = { cell, proxy: (...args: unknown[]) => cell.current(...args) };
        effectEventSlots.set(index, slot);
      }
      slot.cell.current = callback as (...args: unknown[]) => unknown;
      return slot.proxy as (...args: TArgs) => TResult;
    },
    useDeferredValue<T>(value: T): T {
      cursor += 1;
      return value;
    },
  };
});

interface CapturedButtonProps {
  disabled?: boolean;
  onClick?: (event: { preventDefault: () => void; stopPropagation: () => void }) => void;
  children?: ReactNode;
  "aria-label"?: string;
  variant?: string;
}

interface CapturedMenuProps {
  onOpenChange?: (open: boolean) => void;
  children?: ReactNode;
}

interface CapturedMenuItemProps {
  disabled?: boolean;
  onClick?: () => void;
  children?: ReactNode;
}

interface CapturedDialogProps {
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  children?: ReactNode;
}

interface CapturedRadioGroupProps {
  value?: string;
  onValueChange?: (value: unknown) => void;
  "aria-labelledby"?: string;
  disabled?: boolean;
  children?: ReactNode;
}

interface CapturedCheckboxProps {
  checked?: boolean;
  indeterminate?: boolean;
  onCheckedChange?: () => void;
}

interface CapturedTextareaProps {
  value?: string;
  onChange?: (event: { target: { value: string } }) => void;
}

interface CapturedInputProps {
  id?: string;
  value?: string;
  onChange?: (event: { target: { value: string } }) => void;
  disabled?: boolean;
}

const captured = vi.hoisted(() => ({
  createPullRequest: null as GitManagerCreatePullRequestDialogProps | null,
  buttons: [] as unknown[],
  menus: [] as unknown[],
  menuItems: [] as unknown[],
  dialogs: [] as unknown[],
  radioGroups: [] as unknown[],
  checkboxes: [] as unknown[],
  textareas: [] as unknown[],
  inputs: [] as unknown[],
  clear() {
    this.createPullRequest = null;
    this.buttons = [];
    this.menus = [];
    this.menuItems = [];
    this.dialogs = [];
    this.radioGroups = [];
    this.checkboxes = [];
    this.textareas = [];
    this.inputs = [];
  },
}));

const testState = vi.hoisted(() => {
  let toastCounter = 0;
  return {
    resetToastCounter() {
      toastCounter = 0;
    },
    gitStatus: null as unknown,
    gitStatusError: null as string | null,
    statusRefresh: vi.fn(),
    serverConfig: { availableEditors: [] } as unknown,
    serverThread: null as Record<string, unknown> | null,
    draftThread: null as Record<string, unknown> | null,
    setDraftThreadContext: vi.fn(),
    updateThreadMetadata: vi.fn((_input: unknown) =>
      Promise.resolve({ _tag: "Success" } as unknown),
    ),
    refreshVcsStatus: vi.fn(() => Promise.resolve()),
    isGitActionRunning: false,
    discovery: null as unknown,
    initAction: { isPending: false, error: null as unknown, run: vi.fn(), resetError: vi.fn() },
    pullAction: { isPending: false, error: null as unknown, run: vi.fn(), resetError: vi.fn() },
    stackedAction: { isPending: false, error: null as unknown, run: vi.fn(), resetError: vi.fn() },
    publishAction: { isPending: false, error: null as unknown, run: vi.fn(), resetError: vi.fn() },
    openInPreferredEditor: vi.fn(),
    localApi: undefined as { shell: { openExternal: ReturnType<typeof vi.fn> } } | undefined,
    openPullRequestLink: vi.fn(),
    navigate: vi.fn(),
    toast: {
      add: vi.fn(() => {
        toastCounter += 1;
        return `toast-${toastCounter}`;
      }),
      update: vi.fn(),
      close: vi.fn(),
    },
  };
});

vi.mock("@effect/atom-react", () => ({
  useAtomValue: (atom: unknown) =>
    atom !== null &&
    typeof atom === "object" &&
    (atom as { kind?: string }).kind === "server-config"
      ? testState.serverConfig
      : undefined,
}));

vi.mock("./gitManager/provider/GitManagerCreatePullRequestDialog", () => ({
  GitManagerCreatePullRequestDialog: (props: GitManagerCreatePullRequestDialogProps) => {
    captured.createPullRequest = props;
    return null;
  },
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => testState.navigate,
}));

vi.mock("~/state/query", () => ({
  useEnvironmentQuery: (atom: unknown) => {
    if (atom !== null && typeof atom === "object") {
      const kind = (atom as { kind?: string }).kind;
      if (kind === "vcs-status") {
        return {
          data: testState.gitStatus,
          error: testState.gitStatusError,
          isPending: false,
          refresh: testState.statusRefresh,
        };
      }
      if (kind === "discovery") {
        return { data: testState.discovery, error: null, isPending: false, refresh: vi.fn() };
      }
    }
    return { data: null, error: null, isPending: false, refresh: vi.fn() };
  },
}));

vi.mock("~/state/vcs", () => ({
  vcsEnvironment: {
    status: (args: unknown) => ({ kind: "vcs-status", args }),
    refreshStatus: "cmd:refreshStatus",
  },
}));

vi.mock("~/state/server", () => ({
  serverEnvironment: {
    configValueAtom: (environmentId: unknown) => ({ kind: "server-config", environmentId }),
  },
}));

vi.mock("~/state/sourceControl", () => ({
  sourceControlEnvironment: {
    discovery: (args: unknown) => ({ kind: "discovery", args }),
  },
}));

vi.mock("~/state/threads", () => ({
  threadEnvironment: {
    updateMetadata: "cmd:updateMetadata",
  },
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: (command: unknown) =>
    command === "cmd:updateMetadata" ? testState.updateThreadMetadata : testState.refreshVcsStatus,
}));

vi.mock("~/state/entities", () => ({
  useThread: () => testState.serverThread,
}));

vi.mock("~/composerDraftStore", () => ({
  DraftId: { make: (value: string) => value },
  useComposerDraftStore: <T,>(selector: (store: Record<string, unknown>) => T): T =>
    selector({
      getDraftSession: () => testState.draftThread,
      getDraftThreadByRef: () => testState.draftThread,
      setDraftThreadContext: testState.setDraftThreadContext,
    }),
}));

vi.mock("~/editorPreferences", () => ({
  useOpenInPreferredEditor: () => testState.openInPreferredEditor,
}));

vi.mock("~/lib/sourceControlActions", () => ({
  useGitStackedAction: () => testState.stackedAction,
  useSourceControlActionRunning: () => testState.isGitActionRunning,
  useSourceControlPublishRepositoryAction: () => testState.publishAction,
  useVcsInitAction: () => testState.initAction,
  useVcsPullAction: () => testState.pullAction,
}));

vi.mock("~/localApi", () => ({
  readLocalApi: () => testState.localApi,
}));

vi.mock("~/lib/openPullRequestLink", () => ({
  openPullRequestLink: (shell: unknown, url: string) => testState.openPullRequestLink(shell, url),
}));

vi.mock("~/components/ui/toast", () => ({
  toastManager: testState.toast,
  stackedThreadToast: (toast: Record<string, unknown>) => ({ ...toast, stacked: true }),
}));

vi.mock("~/components/ui/button", () => ({
  Button: (props: CapturedButtonProps) => {
    captured.buttons.push(props);
    return (
      <button
        type="button"
        data-slot="button"
        data-disabled={props.disabled ? "true" : undefined}
        aria-label={props["aria-label"]}
        disabled={props.disabled}
        onClick={props.onClick}
      >
        {props.children}
      </button>
    );
  },
}));

vi.mock("~/components/ui/menu", () => ({
  Menu: (props: CapturedMenuProps) => {
    captured.menus.push(props);
    return <div data-testid="menu">{props.children}</div>;
  },
  MenuTrigger: (props: { render?: ReactNode; children?: ReactNode }) => (
    <div data-testid="menu-trigger">
      {props.render ?? null}
      {props.children ?? null}
    </div>
  ),
  MenuPopup: (props: { children?: ReactNode }) => (
    <div data-testid="menu-popup">{props.children}</div>
  ),
  MenuItem: (props: CapturedMenuItemProps) => {
    captured.menuItems.push(props);
    return (
      <div data-slot="menu-item" data-disabled={props.disabled ? "true" : undefined}>
        {props.children}
      </div>
    );
  },
}));

vi.mock("~/components/ui/dialog", () => ({
  Dialog: (props: CapturedDialogProps) => {
    captured.dialogs.push(props);
    return props.open ? <div data-testid="dialog">{props.children}</div> : null;
  },
  DialogDescription: (props: { children?: ReactNode }) => <p>{props.children}</p>,
  DialogFooter: (props: { children?: ReactNode }) => <div>{props.children}</div>,
  DialogHeader: (props: { children?: ReactNode }) => <div>{props.children}</div>,
  DialogPanel: (props: { children?: ReactNode }) => <div>{props.children}</div>,
  DialogPopup: (props: { children?: ReactNode }) => <div>{props.children}</div>,
  DialogTitle: (props: { children?: ReactNode }) => <h2>{props.children}</h2>,
}));

vi.mock("~/components/ui/popover", () => ({
  Popover: (props: { children?: ReactNode }) => <div data-testid="popover">{props.children}</div>,
  PopoverTrigger: (props: { render?: ReactNode; children?: ReactNode }) => (
    <div data-testid="popover-trigger">
      {props.render ?? null}
      {props.children ?? null}
    </div>
  ),
  PopoverPopup: (props: { children?: ReactNode }) => (
    <div data-testid="popover-popup">{props.children}</div>
  ),
}));

vi.mock("~/components/ui/tooltip", () => ({
  Tooltip: (props: { children?: ReactNode }) => <>{props.children}</>,
  TooltipTrigger: (props: { render?: ReactNode; children?: ReactNode }) => (
    <div data-testid="tooltip-trigger">
      {props.render ?? null}
      {props.children ?? null}
    </div>
  ),
  TooltipPopup: (props: { children?: ReactNode }) => (
    <div data-testid="tooltip-popup">{props.children}</div>
  ),
}));

vi.mock("~/components/ui/radio-group", () => ({
  RadioGroup: (props: CapturedRadioGroupProps) => {
    captured.radioGroups.push(props);
    return <div data-testid="radio-group">{props.children}</div>;
  },
}));

vi.mock("@base-ui/react/radio", () => ({
  Radio: {
    Root: (props: { value?: string; children?: ReactNode }) => (
      <div data-testid="radio-option" data-value={props.value}>
        {props.children}
      </div>
    ),
  },
}));

vi.mock("~/components/ui/checkbox", () => ({
  Checkbox: (props: CapturedCheckboxProps) => {
    captured.checkboxes.push(props);
    return <span data-slot="checkbox" data-checked={props.checked ? "true" : "false"} />;
  },
}));

vi.mock("~/components/ui/textarea", () => ({
  Textarea: (props: CapturedTextareaProps) => {
    captured.textareas.push(props);
    return <textarea data-slot="textarea" defaultValue={props.value} />;
  },
}));

vi.mock("~/components/ui/input", () => ({
  Input: (props: CapturedInputProps) => {
    captured.inputs.push(props);
    return (
      <input
        data-slot="input"
        id={props.id}
        value={props.value}
        disabled={props.disabled}
        readOnly={typeof document === "undefined"}
        onChange={props.onChange}
      />
    );
  },
}));

vi.mock("~/components/ui/scroll-area", () => ({
  ScrollArea: (props: { children?: ReactNode }) => <div>{props.children}</div>,
}));

vi.mock("~/components/ui/group", () => ({
  Group: (props: { children?: ReactNode; "aria-label"?: string }) => (
    <div role="group" aria-label={props["aria-label"]}>
      {props.children}
    </div>
  ),
  GroupSeparator: () => <hr />,
}));

if (!browserRuntime) {
  vi.doMock("react", async () => {
    const actual = await vi.importActual<typeof import("react")>("react");
    return {
      ...actual,
      useCallback: hooks.useCallback,
      useMemo: hooks.useMemo,
      useRef: hooks.useRef,
      useState: hooks.useState,
      useEffect: hooks.useEffect.bind(hooks),
      useLayoutEffect: hooks.useLayoutEffect.bind(hooks),
      useEffectEvent: hooks.useEffectEvent,
      useDeferredValue: hooks.useDeferredValue,
    };
  });
  vi.doMock("react/compiler-runtime", () => ({ c: hooks.useMemoCache }));
}

const { default: GitActionsControl } = await import("./GitActionsControl");

const ENVIRONMENT_ID = EnvironmentId.make("env-1");
const THREAD_REF: ScopedThreadRef = {
  environmentId: ENVIRONMENT_ID,
  threadId: ThreadId.make("thread-1"),
};

function status(overrides: Partial<VcsStatusResult> = {}): VcsStatusResult {
  return {
    isRepo: true,
    hasPrimaryRemote: true,
    isDefaultRef: false,
    refName: "feature/test",
    hasWorkingTreeChanges: false,
    workingTree: { files: [], insertions: 0, deletions: 0 },
    hasUpstream: true,
    aheadCount: 0,
    behindCount: 0,
    pr: null,
    ...overrides,
  };
}

function stackedResult(
  overrides: Partial<GitRunStackedActionResult> = {},
): GitRunStackedActionResult {
  return {
    action: "push",
    branch: { status: "skipped_not_requested" },
    commit: { status: "skipped_not_requested" },
    push: { status: "pushed", branch: "feature/test" },
    pr: { status: "skipped_not_requested" },
    toast: { title: "Pushed feature/test", cta: { kind: "none" } },
    ...overrides,
  };
}

function pullResult(overrides: Partial<VcsPullResult> = {}): VcsPullResult {
  return {
    status: "pulled",
    refName: "feature/test",
    upstreamRef: "origin/feature/test",
    ...overrides,
  };
}

function publishResultFixture(
  overrides: Partial<SourceControlPublishRepositoryResult> = {},
): SourceControlPublishRepositoryResult {
  return {
    repository: {
      provider: "github",
      nameWithOwner: "octo/demo",
      url: "https://github.com/octo/demo",
      sshUrl: "git@github.com:octo/demo.git",
    },
    remoteName: "origin",
    remoteUrl: "git@github.com:octo/demo.git",
    branch: "main",
    status: "pushed",
    ...overrides,
  };
}

function discoveryProvider(
  overrides: Partial<SourceControlProviderDiscoveryItem> & {
    kind: SourceControlProviderDiscoveryItem["kind"];
  },
): SourceControlProviderDiscoveryItem {
  return {
    label: overrides.kind,
    status: "available",
    version: Option.some("1.0.0"),
    installHint: `Install ${overrides.kind}`,
    detail: Option.none(),
    auth: {
      status: "authenticated",
      account: Option.some("octo"),
      host: Option.none(),
      detail: Option.none(),
    },
    ...overrides,
  };
}

function discovery(
  providers: ReadonlyArray<SourceControlProviderDiscoveryItem>,
): SourceControlDiscoveryResult {
  return { versionControlSystems: [], sourceControlProviders: providers };
}

function success<A>(value: A) {
  return AsyncResult.success<A, never>(value);
}

function failure(message: string) {
  return AsyncResult.failure(Cause.fail(new Error(message)));
}

function interrupted() {
  return AsyncResult.failure(Cause.interrupt(1));
}

type ControlProps = Parameters<typeof GitActionsControl>[0];

function buildProps(overrides: Partial<ControlProps> = {}): ControlProps {
  return {
    gitCwd: "/repo",
    activeThreadRef: THREAD_REF,
    ...overrides,
  };
}

let lastProps: ControlProps = buildProps();

function render(props: ControlProps = lastProps): string {
  lastProps = props;
  hooks.beginRender();
  captured.clear();
  return renderToStaticMarkup(<GitActionsControl {...props} />);
}

function rerender(): string {
  return render(lastProps);
}

async function flushPromises(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

function buttonByText(text: string): CapturedButtonProps | undefined {
  return (captured.buttons as CapturedButtonProps[]).find(
    (button) => collectText(button.children).trim() === text,
  );
}

function requiredButtonByText(text: string): CapturedButtonProps {
  const match = buttonByText(text);
  expect(match, `Expected button: ${text}`).toBeDefined();
  if (!match) throw new Error(`Missing button: ${text}`);
  return match;
}

function requiredHandler<T extends (...args: never[]) => unknown>(
  value: unknown,
  label: string,
): T {
  expect(value, `Expected handler: ${label}`).toBeTypeOf("function");
  if (typeof value !== "function") throw new Error(`Missing handler: ${label}`);
  return value as T;
}

function menuItemByText(text: string): CapturedMenuItemProps | undefined {
  return (captured.menuItems as CapturedMenuItemProps[]).find((item) =>
    collectText(item.children).includes(text),
  );
}

function collectText(node: unknown): string {
  if (node === null || node === undefined || typeof node === "boolean") {
    return "";
  }
  if (typeof node === "string" || typeof node === "number") {
    return String(node);
  }
  if (Array.isArray(node)) {
    return node.map(collectText).join("");
  }
  if (typeof node === "object" && "props" in (node as Record<string, unknown>)) {
    const props = (node as { props: Record<string, unknown> }).props;
    return collectText(props["children"]) + collectText(props["render"]);
  }
  return "";
}

const clickEvent = () => ({ preventDefault: vi.fn(), stopPropagation: vi.fn() });

function stubWindow(): {
  window: Record<string, ReturnType<typeof vi.fn>>;
  document: { visibilityState: string } & Record<string, unknown>;
} {
  let timeoutId = 0;
  const windowStub = {
    setInterval: vi.fn(() => 41),
    clearInterval: vi.fn(),
    setTimeout: vi.fn(() => {
      timeoutId += 1;
      return timeoutId;
    }),
    clearTimeout: vi.fn(),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  };
  const documentStub = {
    visibilityState: "visible",
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  };
  vi.stubGlobal("window", windowStub);
  vi.stubGlobal("document", documentStub);
  return { window: windowStub, document: documentStub };
}

beforeEach(() => {
  hooks.reset();
  captured.clear();
  vi.clearAllMocks();
  testState.resetToastCounter();
  testState.gitStatus = status();
  testState.gitStatusError = null;
  testState.serverConfig = { availableEditors: [] };
  testState.serverThread = null;
  testState.draftThread = null;
  testState.isGitActionRunning = false;
  testState.discovery = null;
  testState.localApi = undefined;
  testState.initAction.isPending = false;
  testState.pullAction.isPending = false;
  testState.stackedAction.isPending = false;
  testState.publishAction.isPending = false;
  testState.stackedAction.run.mockResolvedValue(success(stackedResult()));
  testState.pullAction.run.mockResolvedValue(success(pullResult()));
  testState.initAction.run.mockResolvedValue(success({}));
  testState.publishAction.run.mockResolvedValue(success(publishResultFixture()));
  testState.openInPreferredEditor.mockResolvedValue(success({}));
  testState.openPullRequestLink.mockResolvedValue(undefined);
  lastProps = buildProps();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

staticDescribe("trigger rendering", () => {
  it("renders nothing without a git cwd", () => {
    const markup = render(buildProps({ gitCwd: null }));
    expect(markup).toBe("");
  });

  it("suppresses the visible trigger when hideTrigger is set", () => {
    const markup = render(buildProps({ hideTrigger: true }));
    expect(markup).not.toContain("Git actions");
    expect(markup).not.toContain("Initialize Git");
  });

  it("does not refresh status when no active environment is selected", () => {
    render(buildProps({ activeThreadRef: null }));
    (captured.menus[0] as CapturedMenuProps).onOpenChange?.(true);
    expect(testState.refreshVcsStatus).not.toHaveBeenCalled();
  });

  it("shows the Initialize Git button when the folder is not a repository", () => {
    testState.gitStatus = status({ isRepo: false });
    const markup = render();
    expect(markup).toContain("Initialize Git");
    expect(markup).not.toContain("Git actions");
  });

  it("shows the initializing state while init is pending", () => {
    testState.gitStatus = status({ isRepo: false });
    testState.initAction.isPending = true;
    const markup = render();
    expect(markup).toContain("Initializing...");
  });

  it("runs git init and stays quiet on success", async () => {
    testState.gitStatus = status({ isRepo: false });
    render();
    buttonByText("Initialize Git")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.initAction.run).toHaveBeenCalledTimes(1);
    expect(testState.toast.add).not.toHaveBeenCalled();
  });

  it("reports init failures as an error toast", async () => {
    testState.gitStatus = status({ isRepo: false });
    testState.initAction.run.mockResolvedValue(failure("init exploded"));
    render();
    buttonByText("Initialize Git")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.toast.add).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        title: "Git initialization failed",
        description: "init exploded",
      }),
    );
  });

  it("stays quiet when init is interrupted", async () => {
    testState.gitStatus = status({ isRepo: false });
    testState.initAction.run.mockResolvedValue(interrupted());
    render();
    buttonByText("Initialize Git")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.toast.add).not.toHaveBeenCalled();
  });
});

staticDescribe("quick action", () => {
  it("shows a disabled hint popover while a git action is running", () => {
    testState.isGitActionRunning = true;
    const markup = render();
    expect(markup).toContain("Git action in progress.");
  });

  it("shows the git-status-unavailable hint when status is missing", () => {
    testState.gitStatus = null;
    const markup = render();
    expect(markup).toContain("Git status is unavailable.");
  });

  it("runs pull when the branch is behind and reports success", async () => {
    testState.gitStatus = status({ behindCount: 2 });
    const markup = render();
    expect(markup).toContain("Pull");
    buttonByText("Pull")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.pullAction.run).toHaveBeenCalledTimes(1);
    expect(testState.toast.add).toHaveBeenCalledWith(
      expect.objectContaining({ type: "loading", title: "Pulling..." }),
    );
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({
        type: "success",
        title: "Pulled",
        description: "Updated feature/test from origin/feature/test",
      }),
    );
  });

  it("reports an already-up-to-date pull", async () => {
    testState.gitStatus = status({ behindCount: 1 });
    testState.pullAction.run.mockResolvedValue(
      success(pullResult({ status: "skipped_up_to_date", upstreamRef: null })),
    );
    render();
    buttonByText("Pull")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({
        type: "success",
        title: "Already up to date",
        description: "feature/test is already synchronized.",
      }),
    );
  });

  it("reports pull failures", async () => {
    testState.gitStatus = status({ behindCount: 1 });
    testState.pullAction.run.mockResolvedValue(failure("pull failed badly"));
    render();
    buttonByText("Pull")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({
        type: "error",
        title: "Pull failed",
        description: "pull failed badly",
      }),
    );
  });

  it("describes a pull without a named upstream", async () => {
    testState.gitStatus = status({ behindCount: 1 });
    testState.pullAction.run.mockResolvedValue(success(pullResult({ upstreamRef: null })));
    render();
    buttonByText("Pull")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({ description: "Updated feature/test from upstream" }),
    );
  });

  it("closes the pull toast when interrupted", async () => {
    testState.gitStatus = status({ behindCount: 1 });
    testState.pullAction.run.mockResolvedValue(interrupted());
    render();
    buttonByText("Pull")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.toast.close).toHaveBeenCalledWith("toast-1");
    expect(testState.toast.update).not.toHaveBeenCalled();
  });

  it("opens the publish dialog when the repo has no remote", () => {
    testState.gitStatus = status({ hasPrimaryRemote: false, hasUpstream: false });
    const markup = render();
    expect(markup).toContain("Publish repository");
    buttonByText("Publish repository")?.onClick?.(clickEvent());
    const reopened = rerender();
    expect(reopened).toContain("Pick where to host it, then point us at a repo to push to.");
  });

  it("opens an existing PR through the local api", async () => {
    const openExternal = vi.fn();
    testState.localApi = { shell: { openExternal } };
    testState.gitStatus = status({
      pr: {
        number: 7,
        title: "Open PR",
        url: "https://example.com/pr/7",
        baseRef: "main",
        headRef: "feature/test",
        state: "open",
      },
    });
    render();
    buttonByText("View PR")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.openPullRequestLink).toHaveBeenCalledWith(
      testState.localApi.shell,
      "https://example.com/pr/7",
    );
  });

  it("reports when link opening is unavailable", async () => {
    testState.localApi = undefined;
    testState.gitStatus = status({
      pr: {
        number: 7,
        title: "Open PR",
        url: "https://example.com/pr/7",
        baseRef: "main",
        headRef: "feature/test",
        state: "open",
      },
    });
    render();
    buttonByText("View PR")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.toast.add).toHaveBeenCalledWith(
      expect.objectContaining({ type: "error", title: "Link opening is unavailable." }),
    );
  });

  it("reports pr link failures", async () => {
    const openExternal = vi.fn();
    testState.localApi = { shell: { openExternal } };
    testState.openPullRequestLink.mockRejectedValue(new Error("no browser"));
    vi.spyOn(console, "error").mockImplementation(() => {});
    testState.gitStatus = status({
      pr: {
        number: 7,
        title: "Open PR",
        url: "https://example.com/pr/7",
        baseRef: "main",
        headRef: "feature/test",
        state: "open",
      },
    });
    render();
    buttonByText("View PR")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.toast.add).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        title: "Unable to open pull request link",
        description: "no browser",
      }),
    );
  });

  it("uses a generic message for opaque PR link failures", async () => {
    testState.localApi = { shell: { openExternal: vi.fn() } };
    testState.openPullRequestLink.mockRejectedValue("opaque browser failure");
    vi.spyOn(console, "error").mockImplementation(() => {});
    testState.gitStatus = status({
      pr: {
        number: 8,
        title: "Open PR",
        url: "https://example.com/pr/8",
        baseRef: "main",
        headRef: "feature/test",
        state: "open",
      },
    });
    render();
    buttonByText("View PR")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.toast.add).toHaveBeenCalledWith(
      expect.objectContaining({ description: "An error occurred." }),
    );
  });

  it("reviews the target before the commit-push-pr stack when the worktree has changes", async () => {
    testState.gitStatus = status({
      hasWorkingTreeChanges: true,
      workingTree: {
        files: [{ path: "src/a.ts", insertions: 1, deletions: 0 }],
        insertions: 1,
        deletions: 0,
      },
    });
    render();
    const quick = (captured.buttons as CapturedButtonProps[]).find((button) =>
      collectText(button.children).includes("Commit, push &"),
    );
    quick?.onClick?.(clickEvent());
    await flushPromises();
    render();
    expect(testState.stackedAction.run).not.toHaveBeenCalled();
    expect(captured.createPullRequest).toMatchObject({
      open: true,
      scope: { environmentId: ENVIRONMENT_ID, cwd: "/repo" },
      commitInput: {},
    });
    captured.createPullRequest?.onOpenChange(false);
    render();
    expect(captured.createPullRequest).toBeNull();
  });
});

staticDescribe("git actions menu", () => {
  it("refreshes vcs status when the menu opens", () => {
    render();
    (captured.menus[0] as CapturedMenuProps).onOpenChange?.(true);
    expect(testState.refreshVcsStatus).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { cwd: "/repo" },
    });
  });

  it("explains why commit and push are disabled on a clean synced branch", () => {
    const markup = render();
    expect(markup).toContain("Worktree is clean. Make changes before committing.");
    expect(markup).toContain("No local commits to push.");
    expect(markup).toContain("No local commits to include in a");
  });

  it("explains detached HEAD restrictions", () => {
    testState.gitStatus = status({ refName: null });
    const markup = render();
    expect(markup).toContain("Detached HEAD: checkout a refName before pushing.");
    expect(markup).toContain("Detached HEAD: create and checkout a refName to enable push");
  });

  it("explains that a behind branch must pull first", () => {
    testState.gitStatus = status({ behindCount: 3, aheadCount: 1 });
    const markup = render();
    expect(markup).toContain("Branch is behind upstream. Pull/rebase before pushing.");
  });

  it("explains that local changes block PR creation", () => {
    testState.gitStatus = status({
      hasWorkingTreeChanges: true,
      workingTree: {
        files: [{ path: "src/a.ts", insertions: 1, deletions: 0 }],
        insertions: 1,
        deletions: 0,
      },
      aheadCount: 1,
    });
    const markup = render();
    expect(markup).toContain("Commit local changes before creating a pull request.");
  });

  it("explains change-request restrictions for dirty and detached branches", () => {
    testState.gitStatus = status({
      hasWorkingTreeChanges: true,
      workingTree: {
        files: [{ path: "dirty.ts", insertions: 1, deletions: 0 }],
        insertions: 1,
        deletions: 0,
      },
      aheadCount: 1,
    });
    let markup = render();
    expect(markup).toContain("Commit local changes before creating a pull request.");

    testState.gitStatus = status({ refName: null, aheadCount: 1 });
    markup = render();
    expect(markup).toContain("Detached HEAD: checkout a refName before creating a pull request.");
  });

  it("explains behind-upstream change requests", () => {
    testState.gitStatus = status({ aheadCount: 1, behindCount: 1 });
    const markup = render();
    expect(markup).toContain(
      "Branch is behind upstream. Pull/rebase before creating a pull request.",
    );
  });

  it("marks all items busy while an action runs", () => {
    testState.isGitActionRunning = true;
    const markup = render();
    expect(markup).toContain("Git action in progress.");
  });

  it("notes when git status is unavailable", () => {
    testState.gitStatus = null;
    render();
    const markup = rerender();
    expect(markup).toContain("Git status is unavailable.");
  });

  it("renders the behind-upstream warning inside the menu", () => {
    testState.gitStatus = status({ behindCount: 2 });
    const markup = render();
    expect(markup).toContain("Behind upstream. Pull/rebase first.");
  });

  it("renders git status errors inside the menu", () => {
    testState.gitStatusError = "status broke";
    const markup = render();
    expect(markup).toContain("status broke");
  });

  it("runs push straight from the menu", async () => {
    testState.gitStatus = status({ aheadCount: 1 });
    render();
    menuItemByText("Push")?.onClick?.();
    await flushPromises();
    expect(testState.stackedAction.run).toHaveBeenCalledWith(
      expect.objectContaining({ action: "push" }),
    );
  });

  it("shows the publish menu entry when no primary remote exists", () => {
    testState.gitStatus = status({ hasPrimaryRemote: false, hasUpstream: false });
    render();
    const publishItem = menuItemByText("Publish repository...");
    expect(publishItem).toBeDefined();
    publishItem?.onClick?.();
    const markup = rerender();
    expect(markup).toContain("Pick where to host it");
  });

  it("opens an existing PR from the menu", async () => {
    const openExternal = vi.fn();
    testState.localApi = { shell: { openExternal } };
    testState.gitStatus = status({
      pr: {
        number: 3,
        title: "Open PR",
        url: "https://example.com/pr/3",
        baseRef: "main",
        headRef: "feature/test",
        state: "open",
      },
    });
    render();
    menuItemByText("View PR")?.onClick?.();
    await flushPromises();
    expect(testState.openPullRequestLink).toHaveBeenCalledWith(
      testState.localApi.shell,
      "https://example.com/pr/3",
    );
  });
});

staticDescribe("commit dialog", () => {
  const changedStatus = () =>
    status({
      hasWorkingTreeChanges: true,
      workingTree: {
        files: [
          { path: "src/a.ts", insertions: 3, deletions: 1 },
          { path: "src/b.ts", insertions: 2, deletions: 0 },
        ],
        insertions: 5,
        deletions: 1,
      },
    });

  function openCommitDialog(): string {
    testState.gitStatus = changedStatus();
    render();
    menuItemByText("Commit")?.onClick?.();
    return rerender();
  }

  it("lists the changed files with their diff counts", () => {
    const markup = openCommitDialog();
    expect(markup).toContain("Commit changes");
    expect(markup).toContain("src/a.ts");
    expect(markup).toContain("src/b.ts");
    expect(markup).toContain("+3");
    expect(markup).toContain("-1");
    expect(markup).toContain("feature/test");
  });

  it("commits with a custom message", async () => {
    openCommitDialog();
    const textarea = captured.textareas[0] as CapturedTextareaProps;
    textarea.onChange?.({ target: { value: "feat: message" } });
    rerender();
    buttonByText("Commit")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.stackedAction.run).toHaveBeenCalledWith(
      expect.objectContaining({ action: "commit", commitMessage: "feat: message" }),
    );
  });

  it("commits only the selected files after excluding one", async () => {
    openCommitDialog();
    buttonByText("Edit")?.onClick?.(clickEvent());
    let markup = rerender();
    expect(markup).toContain("Done");
    // First checkbox is the select-all box, the rest are per-file boxes.
    const fileCheckbox = captured.checkboxes[1] as CapturedCheckboxProps;
    fileCheckbox.onCheckedChange?.();
    rerender();
    // Leave edit mode so the "(n of m)" summary is displayed.
    buttonByText("Done")?.onClick?.(clickEvent());
    markup = rerender();
    expect(markup).toContain("(1 of 2)");
    buttonByText("Commit")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.stackedAction.run).toHaveBeenCalledWith(
      expect.objectContaining({ action: "commit", filePaths: ["src/b.ts"] }),
    );
  });

  it("toggles the whole selection through the select-all checkbox", () => {
    openCommitDialog();
    buttonByText("Edit")?.onClick?.(clickEvent());
    rerender();
    const selectAll = captured.checkboxes[0] as CapturedCheckboxProps;
    selectAll.onCheckedChange?.();
    let markup = rerender();
    expect(markup).toContain("Excluded");
    (captured.checkboxes[0] as CapturedCheckboxProps).onCheckedChange?.();
    markup = rerender();
    expect(markup).not.toContain("Excluded");
  });

  it("commits on a new feature branch", async () => {
    openCommitDialog();
    buttonByText("Commit on new refName")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.stackedAction.run).toHaveBeenCalledWith(
      expect.objectContaining({ action: "commit", featureBranch: true }),
    );
  });

  it("resets everything when cancelled", () => {
    openCommitDialog();
    buttonByText("Cancel")?.onClick?.(clickEvent());
    const markup = rerender();
    expect(markup).not.toContain("Commit changes");
  });

  it("opens a changed file in the preferred editor", async () => {
    const tree = openCommitDialogTree();
    const fileButton = findNativeButtons(tree).find((button) =>
      collectText(button.props.children).includes("src/a.ts"),
    );
    expect(fileButton).toBeDefined();
    (fileButton?.props.onClick as (() => void) | undefined)?.();
    await flushPromises();
    expect(testState.openInPreferredEditor).toHaveBeenCalledTimes(1);
  });

  it("reports editor failures", async () => {
    testState.openInPreferredEditor.mockResolvedValue(failure("cannot open"));
    const tree = openCommitDialogTree();
    const fileButton = findNativeButtons(tree).find((button) =>
      collectText(button.props.children).includes("src/a.ts"),
    );
    (fileButton?.props.onClick as (() => void) | undefined)?.();
    await flushPromises();
    expect(testState.toast.add).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        title: "Unable to open file",
        description: "cannot open",
      }),
    );
  });

  function openCommitDialogTree(): ReactElement {
    testState.gitStatus = changedStatus();
    render();
    menuItemByText("Commit")?.onClick?.();
    hooks.beginRender();
    captured.clear();
    return GitActionsControl(lastProps) as ReactElement;
  }
});

interface NativeElement {
  type: string;
  props: Record<string, unknown>;
}

function findNativeButtons(tree: unknown): NativeElement[] {
  const found: NativeElement[] = [];
  const visit = (node: unknown): void => {
    if (node === null || node === undefined || typeof node !== "object") {
      return;
    }
    if (Array.isArray(node)) {
      node.forEach(visit);
      return;
    }
    const element = node as { type?: unknown; props?: Record<string, unknown> };
    if (!element.props) {
      return;
    }
    if (element.type === "button") {
      found.push(element as NativeElement);
    }
    for (const value of Object.values(element.props)) {
      visit(value);
    }
  };
  visit(tree);
  return found;
}

staticDescribe("default branch confirmation", () => {
  function openConfirmation(): string {
    testState.gitStatus = status({ isDefaultRef: true, refName: "main", aheadCount: 1 });
    render();
    menuItemByText("Push")?.onClick?.();
    return rerender();
  }

  it("prompts before pushing to the default branch", () => {
    const markup = openConfirmation();
    expect(markup).toContain("main");
    expect(testState.stackedAction.run).not.toHaveBeenCalled();
  });

  it("continues on the default branch when confirmed", async () => {
    openConfirmation();
    buttonByText("Push to main")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.stackedAction.run).toHaveBeenCalledWith(
      expect.objectContaining({ action: "push" }),
    );
  });

  it("checks out a feature branch when requested", async () => {
    openConfirmation();
    buttonByText("Checkout feature branch & continue")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.stackedAction.run).toHaveBeenCalledWith(
      expect.objectContaining({ action: "push", featureBranch: true }),
    );
  });

  it("aborts the pending action", async () => {
    openConfirmation();
    buttonByText("Abort")?.onClick?.(clickEvent());
    const markup = rerender();
    await flushPromises();
    expect(testState.stackedAction.run).not.toHaveBeenCalled();
    expect(markup).not.toContain("Checkout feature branch & continue");
  });
});

staticDescribe("stacked action results", () => {
  async function runPush(): Promise<void> {
    testState.gitStatus = status({ aheadCount: 1 });
    render();
    menuItemByText("Push")?.onClick?.();
    await flushPromises();
  }

  it("reports failures with the squashed error message", async () => {
    testState.stackedAction.run.mockResolvedValue(failure("push rejected"));
    await runPush();
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({
        type: "error",
        title: "Action failed",
        description: "push rejected",
      }),
    );
  });

  it("closes the toast quietly when interrupted", async () => {
    testState.stackedAction.run.mockResolvedValue(interrupted());
    await runPush();
    expect(testState.toast.close).toHaveBeenCalledWith("toast-1");
  });

  it("offers a follow-up action from the success toast", async () => {
    testState.stackedAction.run.mockResolvedValue(
      success(
        stackedResult({
          toast: {
            title: "Committed",
            cta: { kind: "run_action", label: "Push now", action: { kind: "push" } },
          },
        }),
      ),
    );
    await runPush();
    const successUpdate = testState.toast.update.mock.calls.find(
      ([, options]) => (options as { type?: string }).type === "success",
    );
    expect(successUpdate).toBeDefined();
    if (!successUpdate) {
      throw new Error("Expected success toast update");
    }
    const actionProps = (
      successUpdate[1] as {
        actionProps: { children: string; onClick: () => void };
      }
    ).actionProps;
    expect(actionProps.children).toBe("Push now");
    testState.stackedAction.run.mockResolvedValue(success(stackedResult()));
    actionProps.onClick();
    await flushPromises();
    expect(testState.toast.close).toHaveBeenCalledWith("toast-1");
    expect(testState.stackedAction.run).toHaveBeenCalledTimes(2);
  });

  it("offers to open the created PR from the success toast", async () => {
    const openExternal = vi.fn();
    testState.localApi = { shell: { openExternal } };
    testState.stackedAction.run.mockResolvedValue(
      success(
        stackedResult({
          toast: {
            title: "PR created",
            cta: { kind: "open_pr", label: "Open PR", url: "https://example.com/pr/9" },
          },
        }),
      ),
    );
    await runPush();
    const successUpdate = testState.toast.update.mock.calls.find(
      ([, options]) => (options as { type?: string }).type === "success",
    );
    if (!successUpdate) {
      throw new Error("Expected success toast update");
    }
    const actionProps = (
      successUpdate[1] as {
        actionProps: { children: string; onClick: () => void };
      }
    ).actionProps;
    actionProps.onClick();
    expect(openExternal).toHaveBeenCalledWith("https://example.com/pr/9");
  });

  it("keeps an open-PR success action inert when the local API is unavailable", async () => {
    testState.stackedAction.run.mockResolvedValue(
      success(
        stackedResult({
          toast: {
            title: "PR created",
            cta: { kind: "open_pr", label: "Open PR", url: "https://example.com/pr/10" },
          },
        }),
      ),
    );
    await runPush();
    const successUpdate = testState.toast.update.mock.calls.find(
      ([, options]) => (options as { type?: string }).type === "success",
    );
    if (!successUpdate) throw new Error("Expected the success toast containing the PR CTA");
    const actionProps = (
      successUpdate[1] as {
        actionProps?: { children: string; onClick: () => void };
      }
    ).actionProps;
    expect(actionProps).toBeDefined();
    if (!actionProps) throw new Error("Expected the Open PR action props");
    expect(actionProps.children).toBe("Open PR");
    actionProps.onClick();
    expect(testState.toast.close).not.toHaveBeenCalled();
  });

  it("leaves thread metadata alone when no branch update or active thread exists", async () => {
    testState.stackedAction.run.mockResolvedValue(success(stackedResult()));
    testState.gitStatus = status({ aheadCount: 1 });
    render(buildProps({ activeThreadRef: null }));
    menuItemByText("Push")?.onClick?.();
    await flushPromises();
    expect(testState.updateThreadMetadata).not.toHaveBeenCalled();
    expect(testState.setDraftThreadContext).not.toHaveBeenCalled();
  });

  it("does not persist a branch that already matches the server thread", async () => {
    testState.serverThread = { branch: "feature/auto", worktreePath: "/wt" };
    testState.stackedAction.run.mockResolvedValue(
      success(stackedResult({ branch: { status: "created", name: "feature/auto" } })),
    );
    await runPush();
    expect(testState.updateThreadMetadata).not.toHaveBeenCalled();
  });

  it("does not persist a branch when the draft already matches", async () => {
    testState.draftThread = { branch: "feature/auto", worktreePath: null, envMode: "local" };
    testState.stackedAction.run.mockResolvedValue(
      success(stackedResult({ branch: { status: "created", name: "feature/auto" } })),
    );
    await runPush();
    expect(testState.setDraftThreadContext).not.toHaveBeenCalled();
  });

  it("persists a newly created branch to the server thread", async () => {
    testState.serverThread = { branch: "main", worktreePath: "/wt" };
    testState.stackedAction.run.mockResolvedValue(
      success(
        stackedResult({
          branch: { status: "created", name: "feature/auto" },
        }),
      ),
    );
    await runPush();
    expect(testState.updateThreadMetadata).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: {
        threadId: THREAD_REF.threadId,
        branch: "feature/auto",
      },
    });
  });

  it("persists a newly created branch to the draft thread", async () => {
    testState.draftThread = { branch: "main", worktreePath: null, envMode: "local" };
    testState.stackedAction.run.mockResolvedValue(
      success(
        stackedResult({
          branch: { status: "created", name: "feature/auto" },
        }),
      ),
    );
    await runPush();
    expect(testState.setDraftThreadContext).toHaveBeenCalledWith(THREAD_REF, {
      branch: "feature/auto",
      worktreePath: null,
    });
  });

  it("feeds progress events into the loading toast", async () => {
    testState.gitStatus = status({ aheadCount: 1 });
    let resolveRun: ((value: unknown) => void) | undefined;
    const pending = new Promise((resolve) => {
      resolveRun = resolve;
    });
    testState.stackedAction.run.mockReturnValue(pending);
    render();
    menuItemByText("Push")?.onClick?.();
    await Promise.resolve();
    const runInput = testState.stackedAction.run.mock.calls[0]?.[0] as {
      actionId: string;
      onProgress: (event: GitActionProgressEvent) => void;
    };
    expect(runInput).toBeDefined();
    const base = { actionId: runInput.actionId, cwd: "/repo" };

    runInput.onProgress({ ...base, kind: "action_started" } as GitActionProgressEvent);
    runInput.onProgress({
      ...base,
      kind: "phase_started",
      label: "Pushing...",
    } as GitActionProgressEvent);
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({ type: "loading", title: "Pushing..." }),
    );
    runInput.onProgress({
      ...base,
      kind: "hook_started",
      hookName: "pre-push",
    } as GitActionProgressEvent);
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({ title: "Running pre-push..." }),
    );
    runInput.onProgress({
      ...base,
      kind: "hook_output",
      text: "linting files",
    } as GitActionProgressEvent);
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({ description: "linting files" }),
    );
    runInput.onProgress({ ...base, kind: "hook_finished" } as GitActionProgressEvent);
    // Ignored events: wrong cwd, wrong action id, and terminal events.
    runInput.onProgress({
      ...base,
      cwd: "/other",
      kind: "hook_output",
      text: "ignored",
    } as GitActionProgressEvent);
    runInput.onProgress({
      ...base,
      actionId: "someone-else",
      kind: "hook_output",
      text: "ignored",
    } as GitActionProgressEvent);
    runInput.onProgress({ ...base, kind: "action_finished" } as GitActionProgressEvent);
    runInput.onProgress({ ...base, kind: "action_failed" } as GitActionProgressEvent);

    resolveRun?.(success(stackedResult()));
    await flushPromises();
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({ type: "success" }),
    );
  });

  it("keeps the elapsed-time description ticking through the interval", async () => {
    const { window: windowStub } = stubWindow();
    const nowSpy = vi.spyOn(Date, "now");
    nowSpy.mockReturnValue(100_000);
    testState.gitStatus = status({ aheadCount: 1 });
    let resolveRun: ((value: unknown) => void) | undefined;
    testState.stackedAction.run.mockReturnValue(
      new Promise((resolve) => {
        resolveRun = resolve;
      }),
    );
    render();
    const cleanups = hooks.runEffects();
    const tick = windowStub["setInterval"]?.mock.calls[0]?.[0] as (() => void) | undefined;
    expect(tick).toBeDefined();
    // Without an active action the tick is a no-op.
    tick?.();
    expect(testState.toast.update).not.toHaveBeenCalled();

    menuItemByText("Push")?.onClick?.();
    await Promise.resolve();
    const runInput = testState.stackedAction.run.mock.calls[0]?.[0] as {
      actionId: string;
      onProgress: (event: GitActionProgressEvent) => void;
    };
    runInput.onProgress({
      actionId: runInput.actionId,
      cwd: "/repo",
      kind: "phase_started",
      label: "Committing...",
    } as GitActionProgressEvent);

    nowSpy.mockReturnValue(105_000);
    tick?.();
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({ description: "Running for 5s" }),
    );

    nowSpy.mockReturnValue(100_000 + 95_000);
    tick?.();
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({ description: "Running for 1m 35s" }),
    );

    resolveRun?.(success(stackedResult()));
    await flushPromises();
    for (const cleanup of cleanups) {
      cleanup();
    }
    expect(windowStub["clearInterval"]).toHaveBeenCalledWith(41);
  });
});

staticDescribe("status refresh on focus", () => {
  it("debounces window focus and visibility refreshes", () => {
    const { window: windowStub, document: documentStub } = stubWindow();
    render();
    const cleanups = hooks.runEffects();

    const focusHandler = windowStub["addEventListener"]?.mock.calls.find(
      ([event]) => event === "focus",
    )?.[1] as (() => void) | undefined;
    expect(focusHandler).toBeDefined();
    focusHandler?.();
    focusHandler?.();
    expect(windowStub["clearTimeout"]).toHaveBeenCalledTimes(1);
    const debounced = windowStub["setTimeout"]?.mock.calls.at(-1)?.[0] as (() => void) | undefined;
    debounced?.();
    expect(testState.refreshVcsStatus).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { cwd: "/repo" },
    });

    const visibilityHandler = (
      documentStub["addEventListener"] as ReturnType<typeof vi.fn>
    ).mock.calls.find(([event]) => event === "visibilitychange")?.[1] as (() => void) | undefined;
    visibilityHandler?.();
    expect(windowStub["setTimeout"]).toHaveBeenCalledTimes(3);

    for (const cleanup of cleanups) {
      cleanup();
    }
    expect(windowStub["removeEventListener"]).toHaveBeenCalledWith("focus", focusHandler);
  });

  it("skips scheduling when the document is hidden", () => {
    const { window: windowStub, document: documentStub } = stubWindow();
    documentStub.visibilityState = "hidden";
    render();
    hooks.runEffects();
    const visibilityHandler = (
      documentStub["addEventListener"] as ReturnType<typeof vi.fn>
    ).mock.calls.find(([event]) => event === "visibilitychange")?.[1] as (() => void) | undefined;
    visibilityHandler?.();
    expect(windowStub["setTimeout"]).not.toHaveBeenCalled();
  });

  it("syncs the live git branch into the thread metadata", () => {
    stubWindow();
    testState.serverThread = { branch: "main", worktreePath: "/wt" };
    testState.gitStatus = status({ refName: "feature/live" });
    render();
    hooks.runEffects();
    expect(testState.updateThreadMetadata).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: {
        threadId: THREAD_REF.threadId,
        branch: "feature/live",
      },
    });
  });

  // After a Git Manager checkout the status stream publishes several new values
  // before the server echoes the thread's new branch; each re-runs the effect.
  it("sends one branch update per real branch change while the server catches up", async () => {
    stubWindow();
    testState.serverThread = { branch: "main", worktreePath: "/wt", updatedAt: "t1" };
    for (let publication = 0; publication < 6; publication += 1) {
      testState.gitStatus = status({ refName: "feature/live" });
      render();
      hooks.runEffects();
    }
    await flushPromises();
    expect(testState.updateThreadMetadata).toHaveBeenCalledOnce();

    testState.serverThread = { branch: "feature/live", worktreePath: "/wt", updatedAt: "t2" };
    testState.gitStatus = status({ refName: "feature/next" });
    render();
    hooks.runEffects();
    render();
    hooks.runEffects();
    expect(testState.updateThreadMetadata).toHaveBeenCalledTimes(2);
    expect(testState.updateThreadMetadata).toHaveBeenLastCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_REF.threadId, branch: "feature/next" },
    });
  });

  // Another client, the catalog reconciler, or a resync snapshot can put the thread
  // back on the old branch without this control seeing the echo in between.
  it("writes again once newer thread state still names the old branch", async () => {
    stubWindow();
    testState.serverThread = { branch: "main", worktreePath: "/wt", updatedAt: "t1" };
    testState.gitStatus = status({ refName: "feature/live" });
    render();
    hooks.runEffects();
    testState.serverThread = { branch: "main", worktreePath: "/wt", updatedAt: "t2" };
    testState.gitStatus = status({ refName: "feature/live" });
    render();
    hooks.runEffects();
    await flushPromises();
    expect(testState.updateThreadMetadata).toHaveBeenCalledTimes(2);
  });

  it("retries a branch update the server rejected", async () => {
    stubWindow();
    testState.updateThreadMetadata.mockResolvedValueOnce({ _tag: "Failure" } as never);
    testState.serverThread = { branch: "main", worktreePath: "/wt", updatedAt: "t1" };
    testState.gitStatus = status({ refName: "feature/live" });
    render();
    hooks.runEffects();
    await flushPromises();
    render();
    hooks.runEffects();
    expect(testState.updateThreadMetadata).toHaveBeenCalledTimes(2);
  });

  it("does not sync while a worktree base is being selected", () => {
    stubWindow();
    testState.draftThread = { branch: "main", worktreePath: null, envMode: "worktree" };
    testState.gitStatus = status({ refName: "feature/live" });
    render();
    hooks.runEffects();
    expect(testState.setDraftThreadContext).not.toHaveBeenCalled();
  });
});

staticDescribe("publish repository dialog", () => {
  function readyDiscovery(): SourceControlDiscoveryResult {
    return discovery([
      discoveryProvider({ kind: "github", label: "GitHub" }),
      discoveryProvider({
        kind: "gitlab",
        label: "GitLab",
        auth: {
          status: "unauthenticated",
          account: Option.none(),
          host: Option.none(),
          detail: Option.some("Run glab auth login."),
        },
      }),
      discoveryProvider({
        kind: "bitbucket",
        label: "Bitbucket",
        status: "missing",
        installHint: "Install the Bitbucket CLI first.",
      }),
    ]);
  }

  function openPublishDialog(): string {
    testState.discovery = readyDiscovery();
    testState.gitStatus = status({ hasPrimaryRemote: false, hasUpstream: false });
    render();
    buttonByText("Publish repository")?.onClick?.(clickEvent());
    return rerender();
  }

  it("sorts ready providers first and explains unavailable ones", () => {
    const markup = openPublishDialog();
    expect(markup).toContain("GitHub");
    expect(markup).toContain("Setup Required");
    expect(markup).toContain("Run glab auth login.");
    expect(markup).toContain("Install the Bitbucket CLI first.");
    expect(markup).toContain(
      "Provider status unavailable. Open Settings -&gt; Source Control and rescan.",
    );
  });

  it("ignores unsupported discovery providers", () => {
    testState.discovery = discovery([
      discoveryProvider({ kind: "github", label: "GitHub" }),
      discoveryProvider({ kind: "gitea" as never, label: "Gitea" }),
    ]);
    testState.gitStatus = status({ hasPrimaryRemote: false, hasUpstream: false });
    render();
    buttonByText("Publish repository")?.onClick?.(clickEvent());
    const markup = rerender();
    expect(markup).toContain("GitHub");
    expect(markup).not.toContain("Gitea");
  });

  it("falls back to a generic hint when an unauthenticated provider has no detail", () => {
    testState.discovery = discovery([
      discoveryProvider({ kind: "github", label: "GitHub" }),
      discoveryProvider({
        kind: "gitlab",
        label: "GitLab",
        auth: {
          status: "unauthenticated",
          account: Option.none(),
          host: Option.none(),
          detail: Option.none(),
        },
      }),
    ]);
    testState.gitStatus = status({ hasPrimaryRemote: false, hasUpstream: false });
    render();
    buttonByText("Publish repository")?.onClick?.(clickEvent());
    const markup = rerender();
    expect(markup).toContain("GitLab is not authenticated.");
  });

  it("walks the wizard and publishes the repository", async () => {
    openPublishDialog();
    buttonByText("Next")?.onClick?.(clickEvent());
    let markup = rerender();
    expect(markup).toContain("Repository");
    expect(markup).toContain("github.com/");

    const providerGroup = (captured.radioGroups as CapturedRadioGroupProps[]).find(
      (group) => group["aria-labelledby"] === "publish-visibility-cards-label",
    );
    providerGroup?.onValueChange?.("public");
    markup = rerender();

    const tree = publishDialogTree();
    const input = findNativeByProp(tree, "id", "publish-repository-path");
    (input?.props["onChange"] as ((event: unknown) => void) | undefined)?.({
      target: { value: "octo/demo" },
    });
    rerender();
    buttonByText("Publish")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.publishAction.run).toHaveBeenCalledWith({
      provider: "github",
      repository: "octo/demo",
      visibility: "public",
      remoteName: "origin",
      protocol: "ssh",
    });
    markup = rerender();
    expect(markup).toContain("Repository published");
    expect(markup).toContain("main is now live on GitHub.");
    expect(markup).toContain("octo/demo");
  });

  it("selects another ready provider and prefills its authenticated account", () => {
    testState.discovery = discovery([
      discoveryProvider({ kind: "github", label: "GitHub" }),
      discoveryProvider({
        kind: "gitlab",
        label: "GitLab",
        auth: {
          status: "authenticated",
          account: Option.some("team"),
          host: Option.none(),
          detail: Option.none(),
        },
      }),
    ]);
    testState.gitStatus = status({ hasPrimaryRemote: false, hasUpstream: false });
    render();
    buttonByText("Publish repository")?.onClick?.(clickEvent());
    rerender();
    const providerGroup = (captured.radioGroups as CapturedRadioGroupProps[]).find(
      (group) => group["aria-labelledby"] === "publish-provider-cards-label",
    );
    providerGroup?.onValueChange?.("gitlab");
    rerender();
    buttonByText("Next")?.onClick?.(clickEvent());
    const tree = publishDialogTree();
    const repositoryInput = findNativeByProp(tree, "id", "publish-repository-path");
    expect(repositoryInput?.props["value"]).toBe("team/");
  });

  it("keeps invalid and pending publication submissions inert", async () => {
    openPublishDialog();
    buttonByText("Next")?.onClick?.(clickEvent());
    rerender();
    expect(buttonByText("Publish")?.disabled).toBe(true);
    buttonByText("Publish")?.onClick?.(clickEvent());
    await flushPromises();
    expect(testState.publishAction.run).not.toHaveBeenCalled();

    testState.publishAction.isPending = true;
    const markup = rerender();
    expect(markup).toContain("Publishing repository to GitHub");
    expect(buttonByText("Publishing...")?.disabled).toBe(true);
  });

  it("describes a remote_added publish result and opens the repo page", async () => {
    testState.publishAction.run.mockResolvedValue(
      success(publishResultFixture({ status: "remote_added" })),
    );
    const openExternal = vi.fn();
    testState.localApi = { shell: { openExternal } };
    openPublishDialog();
    buttonByText("Next")?.onClick?.(clickEvent());
    rerender();
    const tree = publishDialogTree();
    const input = findNativeByProp(tree, "id", "publish-repository-path");
    (input?.props["onChange"] as ((event: unknown) => void) | undefined)?.({
      target: { value: "octo/demo" },
    });
    rerender();
    buttonByText("Publish")?.onClick?.(clickEvent());
    await flushPromises();
    const markup = rerender();
    expect(markup).toContain("Repository created");
    expect(markup).toContain("Remote &quot;origin&quot; is set up.");
    buttonByText("Open on GitHub")?.onClick?.(clickEvent());
    expect(openExternal).toHaveBeenCalledWith("https://github.com/octo/demo");
    buttonByText("Done")?.onClick?.(clickEvent());
    const closed = rerender();
    expect(closed).not.toContain("Repository created");
  });

  it("submits from the repository input on Enter", async () => {
    openPublishDialog();
    buttonByText("Next")?.onClick?.(clickEvent());
    rerender();
    const tree = publishDialogTree();
    const input = findNativeByProp(tree, "id", "publish-repository-path");
    (input?.props["onChange"] as ((event: unknown) => void) | undefined)?.({
      target: { value: "octo/demo" },
    });
    const treeAfter = publishDialogTree();
    const inputAfter = findNativeByProp(treeAfter, "id", "publish-repository-path");
    (inputAfter?.props["onKeyDown"] as ((event: unknown) => void) | undefined)?.({
      key: "Enter",
      preventDefault: vi.fn(),
    });
    await flushPromises();
    expect(testState.publishAction.run).toHaveBeenCalledTimes(1);
  });

  it("ignores non-Enter keys in the repository field", async () => {
    openPublishDialog();
    buttonByText("Next")?.onClick?.(clickEvent());
    rerender();
    const tree = publishDialogTree();
    const repositoryInput = findNativeByProp(tree, "id", "publish-repository-path");
    const preventDefault = vi.fn();
    (repositoryInput?.props["onKeyDown"] as ((event: unknown) => void) | undefined)?.({
      key: "Escape",
      preventDefault,
    });
    await flushPromises();
    expect(preventDefault).not.toHaveBeenCalled();
    expect(testState.publishAction.run).not.toHaveBeenCalled();
  });

  it("shows publish failures inline", async () => {
    testState.publishAction.run.mockResolvedValue(failure("name already taken"));
    openPublishDialog();
    buttonByText("Next")?.onClick?.(clickEvent());
    rerender();
    const tree = publishDialogTree();
    const input = findNativeByProp(tree, "id", "publish-repository-path");
    (input?.props["onChange"] as ((event: unknown) => void) | undefined)?.({
      target: { value: "octo/demo" },
    });
    rerender();
    buttonByText("Publish")?.onClick?.(clickEvent());
    await flushPromises();
    const markup = rerender();
    expect(markup).toContain("Publish failed");
    expect(markup).toContain("name already taken");
  });

  it("shows a generic message for opaque publish failures", async () => {
    testState.publishAction.run.mockResolvedValue(
      AsyncResult.failure(Cause.fail("opaque publish failure")),
    );
    openPublishDialog();
    buttonByText("Next")?.onClick?.(clickEvent());
    rerender();
    const tree = publishDialogTree();
    const repositoryInput = findNativeByProp(tree, "id", "publish-repository-path");
    (repositoryInput?.props["onChange"] as ((event: unknown) => void) | undefined)?.({
      target: { value: "octo/demo" },
    });
    rerender();
    buttonByText("Publish")?.onClick?.(clickEvent());
    await flushPromises();
    expect(rerender()).toContain("An error occurred.");
  });

  it("stays quiet when publishing is interrupted", async () => {
    testState.publishAction.run.mockResolvedValue(interrupted());
    openPublishDialog();
    buttonByText("Next")?.onClick?.(clickEvent());
    rerender();
    const tree = publishDialogTree();
    const input = findNativeByProp(tree, "id", "publish-repository-path");
    (input?.props["onChange"] as ((event: unknown) => void) | undefined)?.({
      target: { value: "octo/demo" },
    });
    rerender();
    buttonByText("Publish")?.onClick?.(clickEvent());
    await flushPromises();
    const markup = rerender();
    expect(markup).not.toContain("Publish failed");
  });

  it("exposes the advanced remote and protocol settings", () => {
    openPublishDialog();
    buttonByText("Next")?.onClick?.(clickEvent());
    rerender();
    const tree = publishDialogTree();
    const advanced = findNativeButtons(tree).find((button) =>
      collectText(button.props["children"]).includes("Advanced"),
    );
    (advanced?.props["onClick"] as (() => void) | undefined)?.();
    const markup = rerender();
    expect(markup).toContain("Remote");
    expect(markup).toContain("Protocol");
    const remoteInput = (captured.inputs as CapturedInputProps[]).find(
      (input) => input.id === "publish-remote-name",
    );
    remoteInput?.onChange?.({ target: { value: "upstream" } });
    const protocolGroup = (captured.radioGroups as CapturedRadioGroupProps[]).find(
      (group) => group["aria-labelledby"] === "publish-protocol-label",
    );
    protocolGroup?.onValueChange?.("https");
    const updated = rerender();
    expect(updated).toContain("HTTPS");
  });

  it("does not open a published repository without a local API", async () => {
    testState.publishAction.run.mockResolvedValue(
      success(publishResultFixture({ status: "remote_added" })),
    );
    openPublishDialog();
    buttonByText("Next")?.onClick?.(clickEvent());
    rerender();
    const tree = publishDialogTree();
    const repositoryInput = requiredNativeByProp(tree, "id", "publish-repository-path");
    requiredHandler<(event: unknown) => void>(
      repositoryInput.props["onChange"],
      "publish repository input change",
    )({
      target: { value: "octo/demo" },
    });
    rerender();
    const publishButton = requiredButtonByText("Publish");
    requiredHandler<NonNullable<CapturedButtonProps["onClick"]>>(
      publishButton.onClick,
      "Publish click",
    )(clickEvent());
    await flushPromises();
    rerender();
    const openButton = requiredButtonByText("Open on GitHub");
    requiredHandler<NonNullable<CapturedButtonProps["onClick"]>>(
      openButton.onClick,
      "Open on GitHub click",
    )(clickEvent());
    expect(testState.toast.close).not.toHaveBeenCalled();
  });

  it("preserves state when receiving a redundant open notification", () => {
    openPublishDialog();
    buttonByText("Next")?.onClick?.(clickEvent());
    rerender();
    const dialog = captured.dialogs.at(-1) as CapturedDialogProps | undefined;
    dialog?.onOpenChange?.(true);
    expect(rerender()).toContain("Visibility");
  });

  it("navigates to source control settings from a not-ready provider", () => {
    openPublishDialog();
    const setupButton = (captured.buttons as CapturedButtonProps[]).find((button) =>
      collectText(button.children).includes("Setup Required"),
    );
    setupButton?.onClick?.(clickEvent());
    expect(testState.navigate).toHaveBeenCalledWith({ to: "/settings/source-control" });
    const markup = rerender();
    expect(markup).not.toContain("Pick where to host it");
  });

  it("goes back from the repository step and cancels from the provider step", () => {
    openPublishDialog();
    buttonByText("Next")?.onClick?.(clickEvent());
    let markup = rerender();
    expect(markup).toContain("Visibility");
    buttonByText("Back")?.onClick?.(clickEvent());
    markup = rerender();
    expect(markup).toContain("Provider");
    buttonByText("Cancel")?.onClick?.(clickEvent());
    markup = rerender();
    expect(markup).not.toContain("Pick where to host it");
  });

  function publishDialogTree(): unknown {
    hooks.beginRender();
    captured.clear();
    const rootTree = GitActionsControl(lastProps) as ReactElement | null;
    const publishElement = findPublishDialogElement(rootTree);
    if (!publishElement) {
      throw new Error("PublishRepositoryDialog element not found");
    }
    const component = publishElement.type as (props: unknown) => unknown;
    return component(publishElement.props);
  }
});

function findPublishDialogElement(
  tree: unknown,
): { type: unknown; props: Record<string, unknown> } | null {
  let match: { type: unknown; props: Record<string, unknown> } | null = null;
  const visit = (node: unknown): void => {
    if (match || node === null || node === undefined || typeof node !== "object") {
      return;
    }
    if (Array.isArray(node)) {
      node.forEach(visit);
      return;
    }
    const element = node as { type?: unknown; props?: Record<string, unknown> };
    if (!element.props) {
      return;
    }
    if (
      typeof element.type === "function" &&
      "gitCwd" in element.props &&
      "onOpenChange" in element.props
    ) {
      match = element as { type: unknown; props: Record<string, unknown> };
      return;
    }
    for (const value of Object.values(element.props)) {
      visit(value);
    }
  };
  visit(tree);
  return match;
}

function findNativeByProp(
  tree: unknown,
  prop: string,
  value: unknown,
): { type: unknown; props: Record<string, unknown> } | null {
  let match: { type: unknown; props: Record<string, unknown> } | null = null;
  const visit = (node: unknown): void => {
    if (match || node === null || node === undefined || typeof node !== "object") {
      return;
    }
    if (Array.isArray(node)) {
      node.forEach(visit);
      return;
    }
    const element = node as { type?: unknown; props?: Record<string, unknown> };
    if (!element.props) {
      return;
    }
    if (element.props[prop] === value) {
      match = element as { type: unknown; props: Record<string, unknown> };
      return;
    }
    for (const propValue of Object.values(element.props)) {
      visit(propValue);
    }
  };
  visit(tree);
  return match;
}

function requiredNativeByProp(
  tree: unknown,
  prop: string,
  value: unknown,
): { type: unknown; props: Record<string, unknown> } {
  const match = findNativeByProp(tree, prop, value);
  expect(match, `Expected native element with ${prop}=${String(value)}`).not.toBeNull();
  if (!match) throw new Error(`Missing native element with ${prop}=${String(value)}`);
  return match;
}

if (browserRuntime) {
  describe("GitActionsControl browser interactions", () => {
    beforeEach(() => {
      (
        globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }
      ).IS_REACT_ACT_ENVIRONMENT = true;
    });

    function requiredElement<T extends Element>(container: ParentNode, selector: string): T {
      const element = container.querySelector<T>(selector);
      if (!element) throw new Error(`Missing DOM element: ${selector}`);
      return element;
    }

    function requiredButton(container: ParentNode, label: string): HTMLButtonElement {
      const match = Array.from(container.querySelectorAll("button")).find(
        (candidate) => candidate.textContent?.trim() === label,
      );
      if (!match) throw new Error(`Missing DOM button: ${label}`);
      return match;
    }

    async function dispatch(element: Element, event: Event): Promise<void> {
      await React.act(async () => {
        element.dispatchEvent(event);
      });
    }

    async function click(container: ParentNode, label: string): Promise<void> {
      const target = requiredButton(container, label);
      await React.act(async () => target.click());
    }

    async function setInputValue(input: HTMLInputElement, value: string): Promise<void> {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      if (!setter) throw new Error("HTMLInputElement.value setter is unavailable");
      setter.call(input, value);
      await dispatch(input, new Event("input", { bubbles: true, cancelable: true }));
    }

    async function mountControl(): Promise<{
      container: HTMLDivElement;
      root: Root;
    }> {
      testState.discovery = discovery([discoveryProvider({ kind: "github", label: "GitHub" })]);
      testState.gitStatus = status({ hasPrimaryRemote: false, hasUpstream: false });
      const container = document.createElement("div");
      document.body.append(container);
      const root = createRoot(container);
      await React.act(async () => {
        root.render(<GitActionsControl {...buildProps()} />);
      });
      return { container, root };
    }

    it.each([
      [" upstream ", "upstream"],
      ["   ", "origin"],
    ])(
      "normalizes an explicitly changed advanced remote %j to %j",
      async (enteredRemote, expectedRemote) => {
        const { container, root } = await mountControl();
        await click(container, "Publish repository");
        await click(container, "Next");
        await click(container, "Advanced");

        const remoteInput = requiredElement<HTMLInputElement>(container, "#publish-remote-name");
        expect(remoteInput.value).toBe("origin");
        await setInputValue(remoteInput, enteredRemote);
        expect(remoteInput.value).toBe(enteredRemote);
        expect(remoteInput.value).not.toBe("origin");

        const repositoryInput = requiredElement<HTMLInputElement>(
          container,
          "#publish-repository-path",
        );
        await setInputValue(repositoryInput, "octo/demo");
        expect(repositoryInput.value).toBe("octo/demo");
        await click(container, "Publish");
        await React.act(async () => flushPromises());

        expect(testState.publishAction.run).toHaveBeenCalledWith(
          expect.objectContaining({ remoteName: expectedRemote, repository: "octo/demo" }),
        );

        await React.act(async () => root.unmount());
        container.remove();
      },
    );

    it("renders one disabled Git control with the shared workspace reason", async () => {
      const reason = "Workspace unavailable. Retry detection or remove it from BiBCode.";
      testState.gitStatus = status();
      const container = document.createElement("div");
      document.body.append(container);
      const root = createRoot(container);
      await React.act(async () => {
        root.render(<GitActionsControl {...buildProps({ workspaceUnavailable: reason })} />);
      });

      const disabled = container.querySelector<HTMLButtonElement>("button[disabled]");
      expect(disabled?.title).toBe(reason);
      expect(container.querySelector('[aria-label="Git action options"]')).toBeNull();

      await React.act(async () => root.unmount());
      container.remove();
    });
  });
}
