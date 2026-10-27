import { scopeThreadRef } from "@coddy/client-runtime/environment";
import {
  EnvironmentId,
  type GitActionProgressEvent,
  ThreadId,
  type VcsStatusResult,
} from "@coddy/contracts";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/reactivity";
import type { Dispatch, ReactElement, SetStateAction } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { joinWorkspacePath } from "./files/FileTreeContextMenu.logic";
import type { WorkingTreeFile } from "./SourceControlPanel.logic";

import type { GitManagerInProgressStripProps } from "./gitManager/GitManagerInProgressStrip";
import type { GitManagerMergeDialogProps } from "./gitManager/merge/GitManagerMergeDialog";
import type { SourceControlMergeChangesProps } from "./SourceControlMergeChanges";
import type { GitManagerCreatePullRequestDialogProps } from "./gitManager/provider/GitManagerCreatePullRequestDialog";

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
  };
});

const testState = vi.hoisted(() => ({
  statusQuery: {
    data: null as unknown,
    error: null as string | null,
    isPending: false,
    refresh: vi.fn(),
  },
  statusAtoms: [] as unknown[],
  draft: { message: "" },
  setMessage: vi.fn(),
  clearDraft: vi.fn(),
  runAction: vi.fn(),
  runPull: vi.fn(),
  runStage: vi.fn(),
  runUnstage: vi.fn(),
  runDiscard: vi.fn(),
  runGenerate: vi.fn(),
  generatePending: false,
  isBusy: false,
  primaryEnvironmentId: null as unknown,
  pullRequestBranchSelection: true,
  mergeCapabilities: true,
  runGitManagerOperation: vi.fn(),
  availableEditors: [] as string[],
  preferredEditor: null as string | null,
  localApi: undefined as unknown,
  openInEditor: vi.fn(),
  readProjectFile: vi.fn(),
  writeProjectFile: vi.fn(),
  openPullRequestLink: vi.fn(),
  rightPanelOpen: vi.fn(),
  openPullRequest: vi.fn(),
  pullRequestsEnabled: true,
  selectGitScope: vi.fn(),
  toast: { add: vi.fn(), update: vi.fn(), close: vi.fn() },
  groupByFolder: true,
  setGroupByFolder: vi.fn(),
}));

interface CapturedButtonProps {
  children?: unknown;
  onClick?: () => void;
  disabled?: boolean;
  autoFocus?: boolean;
  title?: string;
  "aria-label"?: string;
  "aria-pressed"?: boolean;
  className?: string;
}

interface CapturedMenuItemProps {
  children?: unknown;
  onClick?: () => void;
  disabled?: boolean;
  title?: string;
}

interface CapturedSectionProps {
  title: string;
  files: readonly WorkingTreeFile[];
  checked?: (file: WorkingTreeFile) => boolean;
  selectionMode?: boolean;
  allSelected?: boolean;
  onSelectAll?: (selected: boolean) => void;
  selected?: (file: WorkingTreeFile) => boolean;
  onSelect?: (path: string, selected: boolean) => void;
  onToggle: (path: string) => void;
  onOpenFile: (path: string, area?: string) => void;
  primaryAction?: { icon: string; label: string; onClick: () => void };
  onDiscard?: () => void;
  discardVariant?: string;
  disabled?: boolean;
  onStageFile?: (path: string) => void;
  onUnstageFile?: (path: string) => void;
  onRequestDiscardFile?: (file: WorkingTreeFile) => void;
  onCopyPath?: (path: string, relative: boolean) => void;
  onOpenExternalEditor?: (path: string) => void;
  onIgnoreFileName?: (path: string) => void;
  onIgnoreParentFolder?: (path: string) => void;
  isPrimaryEnv?: boolean;
  onStageFiles?: (paths: readonly string[]) => void;
  onUnstageFiles?: (paths: readonly string[]) => void;
  onSelectFiles?: (files: readonly WorkingTreeFile[], selected: boolean) => void;
}

interface CapturedDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  children?: unknown;
}

interface CapturedTextareaProps {
  value: string;
  onChange: (event: { target: { value: string } }) => void;
}

const captured = vi.hoisted(() => ({
  createPullRequest: null as GitManagerCreatePullRequestDialogProps | null,
  merge: null as GitManagerMergeDialogProps | null,
  strip: null as GitManagerInProgressStripProps | null,
  banner: null as { operation: unknown; onCancel: () => void } | null,
  mergeChanges: null as SourceControlMergeChangesProps | null,
  buttons: [] as CapturedButtonProps[],
  menuItems: [] as CapturedMenuItemProps[],
  sections: [] as CapturedSectionProps[],
  groupByFolder: undefined as boolean | undefined,
  dialogs: [] as CapturedDialogProps[],
  textareas: [] as CapturedTextareaProps[],
  commits: [] as Array<{ reloadToken: number; nowMs: number; gitCwd: string | null }>,
  clear() {
    this.createPullRequest = null;
    this.merge = null;
    this.strip = null;
    this.banner = null;
    this.mergeChanges = null;
    this.buttons = [];
    this.menuItems = [];
    this.sections = [];
    this.groupByFolder = undefined;
    this.dialogs = [];
    this.textareas = [];
    this.commits = [];
  },
}));

vi.mock("./gitManager/provider/GitManagerCreatePullRequestDialog", () => ({
  GitManagerCreatePullRequestDialog: (props: GitManagerCreatePullRequestDialogProps) => {
    captured.createPullRequest = props;
    return null;
  },
}));

vi.mock("./gitManager/merge/GitManagerMergeDialog", () => ({
  GitManagerMergeDialog: (props: GitManagerMergeDialogProps) => {
    captured.merge = props;
    return null;
  },
}));

vi.mock("./gitManager/GitManagerInProgressStrip", () => ({
  GitManagerInProgressStrip: (props: GitManagerInProgressStripProps) => {
    captured.strip = props;
    return null;
  },
}));

vi.mock("./gitManager/toolbar/GitManagerOperationBanner", () => ({
  GitManagerOperationBanner: (props: { operation: unknown; onCancel: () => void }) => {
    captured.banner = props;
    return null;
  },
}));

vi.mock("./SourceControlMergeChanges", () => ({
  SourceControlMergeChanges: (props: SourceControlMergeChangesProps) => {
    captured.mergeChanges = props;
    return null;
  },
}));

vi.mock("~/state/gitManager", () => ({
  gitManagerEnvironment: {
    getRefs: (args: unknown) => ({ kind: "refs-atom", args }),
  },
  useRunGitManagerOperation: () => testState.runGitManagerOperation,
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useCallback: hooks.useCallback,
    useMemo: hooks.useMemo,
    useRef: hooks.useRef,
    useState: hooks.useState,
    useEffect: hooks.useEffect.bind(hooks),
    useLayoutEffect: hooks.useLayoutEffect.bind(hooks),
  };
});

vi.mock("react/compiler-runtime", () => ({
  c: hooks.useMemoCache,
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: () => testState.availableEditors,
}));

vi.mock("~/state/query", () => ({
  useEnvironmentQuery: (atom: unknown) => {
    testState.statusAtoms.push(atom);
    return testState.statusQuery;
  },
}));

vi.mock("~/state/vcs", () => ({
  vcsEnvironment: {
    status: (args: unknown) => ({ kind: "status-atom", args }),
  },
}));

vi.mock("~/state/entities", () => ({
  useServerConfigs: () => ({
    get: () => ({
      environment: {
        capabilities: {
          gitPullRequestBranchSelection: testState.pullRequestBranchSelection,
          gitManagerStashMergeOperations: testState.mergeCapabilities,
          gitManagerBranchSyncOperations: testState.mergeCapabilities,
        },
      },
    }),
  }),
}));

vi.mock("~/state/environments", () => ({
  usePrimaryEnvironmentId: () => testState.primaryEnvironmentId,
}));

vi.mock("~/state/server", () => ({
  primaryServerAvailableEditorsAtom: "atom:availableEditors",
}));

vi.mock("~/state/shell", () => ({
  shellEnvironment: { openInEditor: "cmd:openInEditor" },
}));

vi.mock("~/state/projects", () => ({
  projectEnvironment: {
    readFile: "query:readFile",
    writeFile: "cmd:writeFile",
  },
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: (command: unknown) =>
    command === "cmd:writeFile" ? testState.writeProjectFile : testState.openInEditor,
}));

vi.mock("~/state/use-atom-query-runner", () => ({
  useAtomQueryRunner: () => testState.readProjectFile,
}));

vi.mock("~/editorPreferences", () => ({
  usePreferredEditor: () => [testState.preferredEditor, vi.fn()],
}));

vi.mock("~/localApi", () => ({
  readLocalApi: () => testState.localApi,
}));

vi.mock("~/lib/openPullRequestLink", () => ({
  openPullRequestLink: (shell: unknown, url: string) =>
    testState.openPullRequestLink(shell, url) as Promise<void>,
}));

vi.mock("~/lib/sourceControlActions", () => ({
  useGitStackedAction: () => ({ run: testState.runAction }),
  useVcsPullAction: () => ({ run: testState.runPull }),
  useVcsStageAction: () => ({ run: testState.runStage }),
  useVcsUnstageAction: () => ({ run: testState.runUnstage }),
  useVcsDiscardAction: () => ({ run: testState.runDiscard }),
  useVcsGenerateCommitMessageAction: () => ({
    run: testState.runGenerate,
    isPending: testState.generatePending,
  }),
  useSourceControlActionRunning: () => testState.isBusy,
}));

vi.mock("~/sourceControlPanelStore", () => ({
  useSourceControlPanelStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({
      sourceControlGroupByFolder: testState.groupByFolder,
      setSourceControlGroupByFolder: testState.setGroupByFolder,
    }),
}));

vi.mock("~/sourceControlDraft", () => ({
  useSourceControlDraft: () => ({
    message: testState.draft.message,
    setMessage: testState.setMessage,
    clear: testState.clearDraft,
  }),
}));

vi.mock("~/hooks/useSettings", () => ({
  usePrimarySettings: (selector: (settings: { pullRequestsEnabled: boolean }) => unknown) =>
    selector({ pullRequestsEnabled: testState.pullRequestsEnabled }),
}));

vi.mock("~/rightPanelStore", () => ({
  useRightPanelStore: {
    getState: () => ({
      open: testState.rightPanelOpen,
      openPullRequest: testState.openPullRequest,
    }),
  },
}));

vi.mock("~/diffPanelStore", () => ({
  useDiffPanelStore: {
    getState: () => ({ selectGitScope: testState.selectGitScope }),
  },
}));

vi.mock("~/components/ui/toast", () => ({
  toastManager: {
    add: (toast: unknown) => testState.toast.add(toast) as string,
    update: (id: unknown, toast: unknown) => testState.toast.update(id, toast),
    close: (id: unknown) => testState.toast.close(id),
  },
  stackedThreadToast: (toast: Record<string, unknown>) => ({ ...toast, stacked: true }),
}));

vi.mock("~/components/ui/button", () => ({
  Button: (props: CapturedButtonProps) => {
    captured.buttons.push(props);
    return (
      <button
        type="button"
        data-testid="ui-button"
        data-disabled={props.disabled ? "true" : undefined}
        title={props.title}
        aria-label={props["aria-label"]}
      >
        {props.children as never}
      </button>
    );
  },
}));

vi.mock("~/components/ui/dialog", () => ({
  Dialog: (props: CapturedDialogProps) => {
    captured.dialogs.push(props);
    return (
      <div data-testid="dialog" data-open={props.open ? "true" : "false"}>
        {props.open ? (props.children as never) : null}
      </div>
    );
  },
  DialogPopup: (props: { children?: unknown }) => <div>{props.children as never}</div>,
  DialogHeader: (props: { children?: unknown }) => <div>{props.children as never}</div>,
  DialogTitle: (props: { children?: unknown }) => <h2>{props.children as never}</h2>,
  DialogDescription: (props: { children?: unknown }) => <p>{props.children as never}</p>,
  DialogFooter: (props: { children?: unknown }) => <div>{props.children as never}</div>,
}));

vi.mock("~/components/ui/menu", () => ({
  Menu: (props: { children?: unknown }) => <div data-testid="menu">{props.children as never}</div>,
  MenuTrigger: (props: { children?: unknown; disabled?: boolean }) => (
    <div data-testid="menu-trigger" data-disabled={props.disabled ? "true" : undefined}>
      {props.children as never}
    </div>
  ),
  MenuPopup: (props: { children?: unknown }) => <div>{props.children as never}</div>,
  MenuSeparator: () => <hr data-testid="menu-separator" />,
  MenuItem: (props: CapturedMenuItemProps) => {
    captured.menuItems.push(props);
    return (
      <div
        data-testid="menu-item"
        data-disabled={props.disabled ? "true" : undefined}
        title={props.title}
      >
        {props.children as never}
      </div>
    );
  },
}));

vi.mock("~/components/ui/spinner", () => ({
  Spinner: () => <span data-testid="spinner" />,
}));

vi.mock("~/components/ui/textarea", () => ({
  Textarea: (props: CapturedTextareaProps) => {
    captured.textareas.push(props);
    return <div data-testid="textarea" data-value={props.value} />;
  },
}));

vi.mock("./SourceControlCommits", () => ({
  SourceControlCommits: (props: { reloadToken: number; nowMs: number; gitCwd: string | null }) => {
    captured.commits.push(props);
    return <div data-testid="commits" data-reload={props.reloadToken} />;
  },
}));

vi.mock("./SourceControlChangesList", () => ({
  SourceControlChangesList: (props: {
    sections: readonly CapturedSectionProps[];
    groupByFolder?: boolean;
  }) => {
    captured.sections.push(...props.sections);
    captured.groupByFolder = props.groupByFolder;
    return <div data-testid="changes" />;
  },
}));

import SourceControlPanel from "./SourceControlPanel";

const ENVIRONMENT_ID = EnvironmentId.make("env-1");
const THREAD_REF = scopeThreadRef(ENVIRONMENT_ID, ThreadId.make("thread-1"));
const GIT_CWD = "C:/repo";

const STAGED_FILE = {
  path: "src/a.ts",
  insertions: 3,
  deletions: 1,
  status: "modified",
  area: "staged",
} as const;
const UNSTAGED_FILE = {
  path: "src/b.ts",
  insertions: 2,
  deletions: 0,
  status: "modified",
  area: "unstaged",
} as const;
const UNTRACKED_FILE = {
  path: "docs/new.md",
  insertions: 5,
  deletions: 0,
  status: "untracked",
  area: "untracked",
} as const;

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

function stagingStatus(overrides: Partial<VcsStatusResult> = {}): VcsStatusResult {
  return status({
    hasWorkingTreeChanges: true,
    workingTree: {
      files: [STAGED_FILE, UNSTAGED_FILE, UNTRACKED_FILE],
      insertions: 10,
      deletions: 1,
    },
    ...overrides,
  });
}

function legacyStatus(overrides: Partial<VcsStatusResult> = {}): VcsStatusResult {
  return status({
    hasWorkingTreeChanges: true,
    workingTree: {
      files: [
        { path: "src/a.ts", insertions: 3, deletions: 1 },
        { path: "src/b.ts", insertions: 2, deletions: 0 },
      ],
      insertions: 5,
      deletions: 1,
    },
    ...overrides,
  });
}

type PanelProps = Parameters<typeof SourceControlPanel>[0];

function buildProps(overrides: Partial<PanelProps> = {}): PanelProps {
  return {
    mode: "sidebar",
    threadRef: THREAD_REF,
    gitCwd: GIT_CWD,
    ...overrides,
  };
}

let lastProps: PanelProps = buildProps();

function render(props: PanelProps = lastProps): string {
  lastProps = props;
  hooks.beginRender();
  captured.clear();
  return renderToStaticMarkup(<SourceControlPanel {...props} />);
}

function rerender(): string {
  return render(lastProps);
}

function callPanel(props: PanelProps = lastProps): ReactElement {
  lastProps = props;
  hooks.beginRender();
  captured.clear();
  return SourceControlPanel(props) as ReactElement;
}

function flattenText(node: unknown): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(flattenText).join("");
  if (typeof node === "object" && "props" in (node as Record<string, unknown>)) {
    const props = (node as { props: { children?: unknown } }).props;
    return flattenText(props?.children);
  }
  return "";
}

function buttonsByText(text: string): CapturedButtonProps[] {
  return captured.buttons.filter((button) => flattenText(button.children).includes(text));
}

function buttonByExactText(text: string): CapturedButtonProps | undefined {
  return captured.buttons.find((button) => flattenText(button.children) === text);
}

function menuItemByText(text: string): CapturedMenuItemProps | undefined {
  return captured.menuItems.find((item) => flattenText(item.children).includes(text));
}

function sectionByTitle(title: string): CapturedSectionProps | undefined {
  return captured.sections.find((section) => section.title === title);
}

type AnyElement = ReactElement<Record<string, unknown>>;

function collectElements(node: unknown, out: AnyElement[] = []): AnyElement[] {
  if (node == null || typeof node !== "object") return out;
  if (Array.isArray(node)) {
    for (const item of node) collectElements(item, out);
    return out;
  }
  const element = node as AnyElement;
  if (!("props" in element) || element.props == null) return out;
  out.push(element);
  const props = element.props;
  collectElements(props.children, out);
  collectElements(props.header, out);
  return out;
}

function nativeButton(tree: ReactElement, match: (props: Record<string, unknown>) => boolean) {
  return collectElements(tree).find((element) => element.type === "button" && match(element.props));
}

async function flushPromises(): Promise<void> {
  for (let index = 0; index < 8; index += 1) {
    await Promise.resolve();
  }
}

beforeAll(() => {
  vi.stubGlobal("navigator", {
    clipboard: { writeText: vi.fn() },
    platform: "Win32",
  });
});

afterAll(() => {
  vi.unstubAllGlobals();
});

beforeEach(() => {
  hooks.reset();
  captured.clear();
  testState.statusQuery = {
    data: stagingStatus(),
    error: null,
    isPending: false,
    refresh: vi.fn(),
  };
  testState.statusAtoms = [];
  testState.draft = { message: "" };
  testState.setMessage.mockReset();
  testState.clearDraft.mockReset();
  testState.runAction = vi
    .fn()
    .mockResolvedValue(AsyncResult.success({ toast: { title: "Done", description: "All good" } }));
  testState.runPull = vi.fn().mockResolvedValue(AsyncResult.success({ status: "pulled" }));
  testState.runStage = vi.fn().mockResolvedValue(AsyncResult.success(undefined));
  testState.runUnstage = vi.fn().mockResolvedValue(AsyncResult.success(undefined));
  testState.runDiscard = vi.fn().mockResolvedValue(AsyncResult.success(undefined));
  testState.runGenerate = vi
    .fn()
    .mockResolvedValue(AsyncResult.success({ message: "Generated message" }));
  testState.generatePending = false;
  testState.isBusy = false;
  testState.groupByFolder = true;
  testState.setGroupByFolder.mockReset();
  testState.primaryEnvironmentId = ENVIRONMENT_ID;
  testState.pullRequestBranchSelection = true;
  testState.mergeCapabilities = true;
  testState.runGitManagerOperation = vi.fn(() => ({
    result: new Promise(() => undefined),
    cancel: vi.fn(),
  }));
  testState.availableEditors = ["vscode"];
  testState.preferredEditor = "vscode";
  testState.localApi = { shell: { openExternal: vi.fn() } };
  testState.openInEditor.mockReset();
  testState.readProjectFile.mockReset().mockResolvedValue(
    AsyncResult.success({
      relativePath: ".gitignore",
      contents: "node_modules/\n",
      byteLength: 13,
      truncated: false,
    }),
  );
  testState.writeProjectFile.mockReset().mockResolvedValue(
    AsyncResult.success({
      relativePath: ".gitignore",
    }),
  );
  testState.openPullRequestLink = vi.fn().mockResolvedValue(undefined);
  testState.rightPanelOpen.mockReset();
  testState.openPullRequest.mockReset();
  testState.pullRequestsEnabled = true;
  testState.selectGitScope.mockReset();
  testState.toast.add.mockReset();
  testState.toast.add.mockReturnValue("toast-1");
  testState.toast.update.mockReset();
  testState.toast.close.mockReset();
});

describe("SourceControlPanel", () => {
  it("pushes before opening the merge request review, and cancel does no further work", async () => {
    testState.statusQuery.data = status({
      refName: "feature/test",
      hasUpstream: false,
      aheadCount: 1,
      sourceControlProvider: { kind: "gitlab", name: "GitLab", baseUrl: "https://gitlab.test" },
    });
    const props = buildProps();
    render(props);
    const create = buttonsByText("Push & create MR")[0];
    expect(create).toBeDefined();
    create?.onClick?.();
    await flushPromises();
    render(props);
    expect(testState.runAction).toHaveBeenCalledTimes(1);
    expect(testState.runAction).toHaveBeenCalledWith(expect.objectContaining({ action: "push" }));
    expect(captured.createPullRequest).toMatchObject({
      open: true,
      scope: { environmentId: props.threadRef.environmentId, cwd: props.gitCwd },
    });
    expect(captured.createPullRequest?.commitInput).toBeUndefined();
    captured.createPullRequest?.onOpenChange(false);
    render(props);
    expect(captured.createPullRequest).toBeNull();
    expect(testState.runAction).toHaveBeenCalledTimes(1);
  });

  const OPEN_MR = {
    number: 89,
    title: "Fix it",
    url: "https://gitlab.test/team/repo/-/merge_requests/89",
    baseRef: "main",
    headRef: "feature/test",
    state: "open",
  } as const;

  it("opens the branch's open merge request in the thread's panel from the menu", () => {
    testState.statusQuery.data = status({
      sourceControlProvider: { kind: "gitlab", name: "GitLab", baseUrl: "https://gitlab.test" },
      pr: OPEN_MR,
    });
    const props = buildProps();
    render(props);
    menuItemByText("Open MR in panel")?.onClick?.();
    expect(testState.openPullRequest).toHaveBeenCalledWith(props.threadRef, {
      number: 89,
      provider: "gitlab",
    });
    expect(testState.openPullRequestLink).not.toHaveBeenCalled();
  });

  it("opens the open merge request in the thread's panel from the legacy menu", () => {
    testState.statusQuery.data = legacyStatus({
      sourceControlProvider: { kind: "gitlab", name: "GitLab", baseUrl: "https://gitlab.test" },
      pr: OPEN_MR,
    });
    const props = buildProps();
    render(props);
    menuItemByText("Open MR in panel")?.onClick?.();
    expect(testState.openPullRequest).toHaveBeenCalledWith(props.threadRef, {
      number: 89,
      provider: "gitlab",
    });
  });

  it.each([
    ["Pull Requests is turned off", false, "gitlab"],
    ["the host is Bitbucket", true, "bitbucket"],
    ["the host is Azure DevOps", true, "azure-devops"],
  ] as const)("does not offer the in-panel action when %s", (_name, enabled, kind) => {
    testState.pullRequestsEnabled = enabled;
    for (const data of [
      status({
        sourceControlProvider: { kind, name: "Host", baseUrl: "https://host.test" },
        pr: OPEN_MR,
      }),
      legacyStatus({
        sourceControlProvider: { kind, name: "Host", baseUrl: "https://host.test" },
        pr: OPEN_MR,
      }),
    ]) {
      testState.statusQuery.data = data;
      render(buildProps());
      expect(captured.menuItems.map((item) => flattenText(item.children))).not.toContainEqual(
        expect.stringContaining("in panel"),
      );
      expect(menuItemByText("View")).toBeDefined();
    }
  });

  it("opens the created merge request beside the thread on Done", async () => {
    testState.statusQuery.data = status({
      refName: "feature/test",
      hasUpstream: false,
      aheadCount: 1,
      sourceControlProvider: { kind: "gitlab", name: "GitLab", baseUrl: "https://gitlab.test" },
    });
    const props = buildProps();
    render(props);
    buttonsByText("Push & create MR")[0]?.onClick?.();
    await flushPromises();
    render(props);
    captured.createPullRequest?.onDone?.({ number: 89, provider: "gitlab" });
    expect(testState.openPullRequest).toHaveBeenCalledWith(props.threadRef, {
      number: 89,
      provider: "gitlab",
    });
  });

  it.each([
    ["Pull Requests is turned off", false, "gitlab"],
    ["the host is Bitbucket", true, "bitbucket"],
    ["the host is Azure DevOps", true, "azure-devops"],
  ] as const)("does not open the created request on Done when %s", async (_name, enabled, kind) => {
    testState.pullRequestsEnabled = enabled;
    testState.statusQuery.data = status({
      refName: "feature/test",
      hasUpstream: false,
      aheadCount: 1,
      sourceControlProvider: { kind, name: "Host", baseUrl: "https://host.test" },
    });
    const props = buildProps();
    render(props);
    buttonsByText("Push & create")[0]?.onClick?.();
    await flushPromises();
    render(props);
    expect(captured.createPullRequest).toMatchObject({ open: true });
    captured.createPullRequest?.onDone?.({ number: 89, provider: kind });
    expect(testState.openPullRequest).not.toHaveBeenCalled();
  });

  it.each([
    ["a tracked branch", { hasUpstream: true, aheadCount: 1 }, true],
    ["a server without branch selection", { hasUpstream: false, aheadCount: 1 }, false],
    [
      "an unidentified host",
      { hasUpstream: false, aheadCount: 1, sourceControlProvider: undefined },
      true,
    ],
    [
      "uncommitted changes",
      { hasUpstream: false, aheadCount: 1, hasWorkingTreeChanges: true },
      true,
    ],
  ] as const)(
    "opens the merge request review without a preliminary push for %s",
    async (_case, remote, branchSelection) => {
      testState.pullRequestBranchSelection = branchSelection;
      testState.statusQuery.data = status({
        refName: "feature/test",
        sourceControlProvider: { kind: "gitlab", name: "GitLab", baseUrl: "https://gitlab.test" },
        ...remote,
      });
      const props = buildProps();
      render(props);
      const create = buttonsByText("Push & create MR")[0] ?? buttonsByText("Push & create PR")[0];
      expect(create).toBeDefined();
      create?.onClick?.();
      await flushPromises();
      render(props);
      expect(testState.runAction).not.toHaveBeenCalled();
      expect(captured.createPullRequest).toMatchObject({ open: true });
    },
  );

  it("keeps the merge request review closed when the push fails", async () => {
    testState.statusQuery.data = status({
      refName: "feature/test",
      hasUpstream: false,
      aheadCount: 1,
      sourceControlProvider: { kind: "gitlab", name: "GitLab", baseUrl: "https://gitlab.test" },
    });
    testState.runAction.mockResolvedValueOnce(
      AsyncResult.failure(Cause.fail(new Error("push rejected"))),
    );
    const props = buildProps();
    render(props);
    buttonsByText("Push & create MR")[0]?.onClick?.();
    await flushPromises();
    render(props);
    expect(testState.runAction).toHaveBeenCalledWith(expect.objectContaining({ action: "push" }));
    expect(captured.createPullRequest).toBeNull();
  });

  it("uses the shared workspace guard instead of starting Git work", () => {
    const reason = "Workspace unavailable. Retry detection or remove it from Coddy Studio.";
    const markup = render(buildProps({ workspaceUnavailable: reason }));

    expect(testState.statusAtoms[0]).toBeNull();
    expect(markup).toContain(reason);
    expect(markup).not.toContain("Generate commit message with AI");
  });

  it("shows a loading state while the first status fetch is pending", () => {
    testState.statusQuery = { data: null, error: null, isPending: true, refresh: vi.fn() };
    const markup = render(buildProps());
    expect(markup).toContain("Loading changes");
    expect(markup).toContain("Source Control");
  });

  it("renders 'No changes' when the tree is clean and skips the query without a cwd", () => {
    testState.statusQuery = { data: status(), error: null, isPending: false, refresh: vi.fn() };
    const markup = render(buildProps({ gitCwd: null }));
    expect(testState.statusAtoms[0]).toBeNull();
    expect(markup).toContain("No changes");
    // Nothing to summarize: the +/- footer is not rendered.
    expect(markup).not.toContain("+0");
  });

  it("enables push for a clean default branch whose upstream ref does not exist yet", () => {
    testState.statusQuery.data = status({
      refName: "main",
      isDefaultRef: true,
      hasUpstream: false,
    });
    render(buildProps());

    expect(buttonByExactText("Push")?.disabled).not.toBe(true);
    expect(captured.menuItems.find((item) => flattenText(item.children) === "Push")?.disabled).toBe(
      false,
    );
  });

  it("renders header metadata: branch, vs-base, PR chip and ahead/behind counts", () => {
    testState.statusQuery.data = stagingStatus({
      refName: "feature/test",
      defaultRefName: "main",
      aheadOfDefaultCount: 2,
      aheadCount: 1,
      behindCount: 2,
      pr: {
        number: 7,
        title: "Panel PR",
        url: "https://example.com/pr/7",
        baseRef: "main",
        headRef: "feature/test",
        state: "open",
      },
    });
    const markup = render(buildProps());
    expect(markup).toContain("feature/test");
    expect(markup).toContain("vs main ↑2");
    expect(markup).toContain("#7");
    expect(markup).toContain("↑1");
    expect(markup).toContain("↓2");
    expect(markup).toContain("+10");
    expect(markup).toContain("-1");

    // Clicking the header PR chip opens the PR in the browser.
    const tree = callPanel();
    const chip = nativeButton(tree, (props) => flattenText(props.children).includes("#7"));
    (chip!.props.onClick as () => void)();
    expect(testState.openPullRequestLink).toHaveBeenCalledWith(
      (testState.localApi as { shell: unknown }).shell,
      "https://example.com/pr/7",
    );
  });

  it("groups files into staged, unstaged and untracked sections", () => {
    render(buildProps());
    expect(sectionByTitle("Staged Changes")?.files).toEqual([STAGED_FILE]);
    expect(sectionByTitle("Changes")?.files).toEqual([UNSTAGED_FILE]);
    expect(sectionByTitle("Untracked Files")?.files).toEqual([UNTRACKED_FILE]);
    expect(sectionByTitle("Untracked Files")?.discardVariant).toBe("delete-untracked");
    expect(sectionByTitle("Staged Changes")?.isPrimaryEnv).toBe(true);
  });

  it("enters selection mode, selects a section, and cancels cleanly", () => {
    render(buildProps());
    buttonByExactText("Select")?.onClick?.();

    let markup = rerender();
    expect(markup).toContain("0 files selected");
    expect(buttonByExactText("Cancel")?.autoFocus).toBe(true);
    expect(sectionByTitle("Changes")?.selectionMode).toBe(true);
    expect(sectionByTitle("Changes")?.allSelected).toBe(false);

    sectionByTitle("Changes")?.onSelectAll?.(true);
    markup = rerender();
    expect(markup).toContain("1 file selected");
    expect(sectionByTitle("Changes")?.selected?.(UNSTAGED_FILE)).toBe(true);
    expect(sectionByTitle("Changes")?.allSelected).toBe(true);

    buttonByExactText("Cancel")?.onClick?.();
    rerender();
    expect(sectionByTitle("Changes")?.selectionMode).toBe(false);
    expect(sectionByTitle("Changes")?.selected?.(UNSTAGED_FILE)).toBe(false);
    expect(buttonByExactText("Select")?.autoFocus).toBe(true);
  });

  it("exits selection mode on Escape", () => {
    render(buildProps());
    buttonByExactText("Select")?.onClick?.();
    const tree = callPanel();
    const root = collectElements(tree).find(
      (element) => element.type === "div" && typeof element.props.onKeyDown === "function",
    );
    const preventDefault = vi.fn();

    const onKeyDown = root?.props.onKeyDown as
      | ((event: { key: string; preventDefault(): void }) => void)
      | undefined;
    onKeyDown?.({ key: "Escape", preventDefault });
    rerender();

    expect(preventDefault).toHaveBeenCalledOnce();
    expect(buttonByExactText("Select")).toBeDefined();
  });

  it("does not expose selection mode to a legacy flat-list server", () => {
    testState.statusQuery.data = legacyStatus();
    render(buildProps());

    expect(buttonByExactText("Select")).toBeUndefined();
    expect(sectionByTitle("Changes")?.selectionMode).toBeUndefined();
  });

  it("resets selection mode when staging areas disappear", () => {
    render(buildProps());
    buttonByExactText("Select")?.onClick?.();
    rerender();
    sectionByTitle("Changes")?.onSelect?.(UNSTAGED_FILE.path, true);
    rerender();

    testState.statusQuery.data = legacyStatus({
      workingTree: {
        files: [{ path: UNSTAGED_FILE.path, insertions: 3, deletions: 1 }],
        insertions: 3,
        deletions: 1,
      },
    });
    rerender();

    expect(buttonByExactText("Cancel")).toBeUndefined();
    expect(buttonByExactText("Select")).toBeUndefined();
    expect(sectionByTitle("Changes")?.selectionMode).toBeUndefined();

    for (const effect of hooks.effects) effect();
    testState.statusQuery.data = stagingStatus();
    rerender();
    expect(sectionByTitle("Changes")?.selectionMode).toBe(false);
    expect(sectionByTitle("Changes")?.selected?.(UNSTAGED_FILE)).toBe(false);

    buttonByExactText("Select")?.onClick?.();
    rerender();
    sectionByTitle("Changes")?.onSelect?.(UNSTAGED_FILE.path, true);
    rerender();
    captured.buttons
      .find((button) => button["aria-label"] === "Discard selected files")
      ?.onClick?.();
    rerender();
    expect(captured.dialogs[1]?.open).toBe(true);

    testState.statusQuery.data = legacyStatus({
      hasWorkingTreeChanges: false,
      workingTree: { files: [], insertions: 0, deletions: 0 },
    });
    rerender();
    expect(buttonByExactText("Select")).toBeUndefined();
    expect(
      captured.buttons.find((button) => button["aria-label"] === "Ignore selected files"),
    ).toBeUndefined();

    for (const effect of hooks.effects) effect();
    rerender();
    expect(captured.dialogs[1]?.open).toBe(false);

    testState.statusQuery.data = stagingStatus();
    rerender();
    expect(sectionByTitle("Changes")?.selectionMode).toBe(false);
    expect(sectionByTitle("Changes")?.selected?.(UNSTAGED_FILE)).toBe(false);
    expect(captured.dialogs[1]?.open).toBe(false);
  });

  it("commits the staged index as-is and clears the draft on success", async () => {
    testState.draft = { message: "Panel message" };
    render(buildProps());

    const commit = buttonsByText("Commit (1)")[0];
    expect(commit).toBeDefined();
    commit?.onClick?.();
    await flushPromises();

    expect(testState.runAction).toHaveBeenCalledWith(
      expect.objectContaining({
        action: "commit",
        commitMessage: "Panel message",
        commitStagedIndexAsIs: true,
      }),
    );
    expect(testState.runAction.mock.calls[0]?.[0]).not.toHaveProperty("filePaths");
    expect(testState.clearDraft).toHaveBeenCalledWith();
    expect(testState.toast.add).toHaveBeenCalledWith(expect.objectContaining({ type: "loading" }));
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({ type: "success", title: "Done" }),
    );

    // The commits list refetches via the bumped reload token.
    rerender();
    expect(captured.commits[0]?.reloadToken).toBe(1);
  });

  it("switches the changes view between folder groups and a flat list", () => {
    render();
    const grouped = captured.buttons.find((button) => button["aria-label"] === "Group by folder");
    expect(grouped?.["aria-pressed"]).toBe(true);
    expect(captured.groupByFolder).toBe(true);
    grouped?.onClick?.();
    expect(testState.setGroupByFolder).toHaveBeenCalledWith(false);

    testState.groupByFolder = false;
    render();
    expect(
      captured.buttons.find((button) => button["aria-label"] === "Group by folder")?.[
        "aria-pressed"
      ],
    ).toBe(false);
    expect(captured.groupByFolder).toBe(false);
  });

  it("hides the view toggle while the working tree is clean", () => {
    testState.statusQuery.data = status({
      hasWorkingTreeChanges: false,
      workingTree: { files: [], insertions: 0, deletions: 0 },
    });
    render(buildProps());
    expect(
      captured.buttons.find((button) => button["aria-label"] === "Group by folder"),
    ).toBeUndefined();
  });

  it("stages and unstages a whole folder in one request", async () => {
    render();
    const section = sectionByTitle("Changes");
    section?.onStageFiles?.([UNSTAGED_FILE.path]);
    await flushPromises();
    expect(testState.runStage).toHaveBeenCalledWith([UNSTAGED_FILE.path]);

    sectionByTitle("Staged Changes")?.onUnstageFiles?.([STAGED_FILE.path]);
    await flushPromises();
    expect(testState.runUnstage).toHaveBeenCalledWith([STAGED_FILE.path]);
  });

  it("stages everything when nothing is staged yet", async () => {
    testState.statusQuery.data = stagingStatus({
      workingTree: {
        files: [UNSTAGED_FILE, UNTRACKED_FILE],
        insertions: 7,
        deletions: 0,
      },
    });
    render(buildProps());

    const stageAll = buttonsByText("Stage All Changes (2)")[0];
    stageAll?.onClick?.();
    await flushPromises();
    expect(testState.runStage).toHaveBeenCalledWith([UNSTAGED_FILE.path, UNTRACKED_FILE.path]);
  });

  it("pulls when behind and reports both pull outcomes", async () => {
    testState.statusQuery.data = status({
      hasWorkingTreeChanges: false,
      behindCount: 3,
      workingTree: {
        files: [{ path: "src/a.ts", insertions: 1, deletions: 0, area: "staged" }],
        insertions: 1,
        deletions: 0,
      },
    });
    // Force the pull path through the menu of a staged tree instead: use a
    // clean staging tree that resolves the "pull" primary action.
    testState.statusQuery.data = stagingStatus({
      behindCount: 3,
      workingTree: {
        files: [
          { path: "src/a.ts", insertions: 0, deletions: 0, status: "modified", area: "staged" },
        ],
        insertions: 0,
        deletions: 0,
      },
    });
    render(buildProps());
    const pullItem = menuItemByText("Pull");
    expect(pullItem).toBeDefined();
    pullItem?.onClick?.();
    await flushPromises();
    expect(testState.runPull).toHaveBeenCalledTimes(1);
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({ type: "success", title: "Pulled" }),
    );

    testState.runPull.mockResolvedValueOnce(AsyncResult.success({ status: "noop" }));
    pullItem?.onClick?.();
    await flushPromises();
    expect(testState.toast.update).toHaveBeenLastCalledWith(
      "toast-1",
      expect.objectContaining({ title: "Already up to date" }),
    );

    testState.runPull.mockResolvedValueOnce(
      AsyncResult.failure(Cause.fail(new Error("pull exploded"))),
    );
    pullItem?.onClick?.();
    await flushPromises();
    expect(testState.toast.update).toHaveBeenLastCalledWith(
      "toast-1",
      expect.objectContaining({ title: "Pull failed", description: "pull exploded" }),
    );

    testState.runPull.mockResolvedValueOnce(AsyncResult.failure(Cause.interrupt(1)));
    pullItem?.onClick?.();
    await flushPromises();
    expect(testState.toast.close).toHaveBeenCalledWith("toast-1");
  });

  it("asks for confirmation before pushing the default branch", async () => {
    testState.statusQuery.data = status({
      refName: "main",
      isDefaultRef: true,
      aheadCount: 2,
      hasWorkingTreeChanges: true,
      workingTree: {
        files: [STAGED_FILE],
        insertions: 3,
        deletions: 1,
      },
    });
    render(buildProps());

    const pushItem = menuItemByText("Commit & Push");
    pushItem?.onClick?.();
    await flushPromises();
    expect(testState.runAction).not.toHaveBeenCalled();

    let markup = rerender();
    expect(captured.dialogs[0]?.open).toBe(true);
    expect(markup).toContain("Commit &amp; push to default ref?");
    expect(markup).toContain("main");

    // Cancel closes the dialog without running anything.
    buttonsByText("Cancel")[0]?.onClick?.();
    markup = rerender();
    expect(captured.dialogs[0]?.open).toBe(false);
    expect(testState.runAction).not.toHaveBeenCalled();

    // Reopen and confirm: the action reruns with the confirmation skipped.
    menuItemByText("Commit & Push")?.onClick?.();
    await flushPromises();
    rerender();
    buttonsByText("Commit & push to main")[0]?.onClick?.();
    await flushPromises();
    expect(testState.runAction).toHaveBeenCalledWith(
      expect.objectContaining({ action: "commit_push", commitStagedIndexAsIs: true }),
    );
    rerender();
    expect(captured.dialogs[0]?.open).toBe(false);
  });

  it("surfaces stacked-action failures and swallows interruptions", async () => {
    testState.runAction.mockResolvedValueOnce(
      AsyncResult.failure(Cause.fail(new Error("commit exploded"))),
    );
    render(buildProps());

    buttonsByText("Commit (1)")[0]?.onClick?.();
    await flushPromises();
    expect(testState.toast.update).toHaveBeenCalledWith(
      "toast-1",
      expect.objectContaining({ title: "Action failed", description: "commit exploded" }),
    );
    expect(testState.clearDraft).not.toHaveBeenCalled();

    testState.runAction.mockResolvedValueOnce(
      AsyncResult.failure(Cause.fail("stream completion was rejected")),
    );
    buttonsByText("Commit (1)")[0]?.onClick?.();
    await flushPromises();
    expect(testState.toast.update).toHaveBeenLastCalledWith(
      "toast-1",
      expect.objectContaining({
        title: "Action failed",
        description: "stream completion was rejected",
      }),
    );

    testState.runAction.mockResolvedValueOnce(AsyncResult.failure(Cause.interrupt(1)));
    buttonsByText("Commit (1)")[0]?.onClick?.();
    await flushPromises();
    expect(testState.toast.close).toHaveBeenCalledWith("toast-1");
  });

  it("shows what Git said when the server reports the failure (#157)", async () => {
    const gitError = "fatal: pre-commit hook rejected the commit";
    testState.runAction.mockImplementationOnce(
      async (input: { actionId: string; onProgress?: (event: GitActionProgressEvent) => void }) => {
        input.onProgress?.({
          actionId: input.actionId,
          cwd: "/repo",
          action: "commit",
          kind: "action_failed",
          phase: null,
          message: gitError,
        } as GitActionProgressEvent);
        // The settled error names only the action and phase, never the remote text.
        return AsyncResult.failure(
          Cause.fail(new Error("Source control action 'commit' failed during execution.")),
        );
      },
    );
    render(buildProps());

    buttonsByText("Commit (1)")[0]?.onClick?.();
    await flushPromises();
    expect(testState.toast.update).toHaveBeenLastCalledWith(
      "toast-1",
      expect.objectContaining({ title: "Action failed", description: gitError }),
    );
  });

  it("renders the legacy flat section and commits by explicit file paths", async () => {
    testState.draft = { message: "Legacy message" };
    testState.statusQuery.data = legacyStatus();
    const markup = render(buildProps());

    expect(captured.sections).toHaveLength(1);
    expect(sectionByTitle("Changes")?.files).toHaveLength(2);
    // The legacy toggle is a shared no-op.
    sectionByTitle("Changes")?.onToggle("src/a.ts");
    expect(testState.runStage).not.toHaveBeenCalled();
    expect(markup).toContain("+5");

    const commitItem = menuItemByText("Commit");
    expect(commitItem?.disabled).toBe(false);
    commitItem?.onClick?.();
    await flushPromises();
    expect(testState.runAction).toHaveBeenCalledWith(
      expect.objectContaining({
        action: "commit",
        commitMessage: "Legacy message",
        filePaths: ["src/a.ts", "src/b.ts"],
      }),
    );
    expect(testState.runAction.mock.calls[0]?.[0]).not.toHaveProperty("commitStagedIndexAsIs");
  });

  it("opens an existing PR from the legacy quick action", async () => {
    testState.statusQuery.data = legacyStatus({
      hasWorkingTreeChanges: false,
      workingTree: { files: [], insertions: 0, deletions: 0 },
      pr: {
        number: 9,
        title: "Existing",
        url: "https://example.com/pr/9",
        baseRef: "main",
        headRef: "feature/test",
        state: "open",
      },
    });
    render(buildProps());

    const viewPr = buttonsByText("View PR")[0];
    expect(viewPr).toBeDefined();
    viewPr?.onClick?.();
    await flushPromises();
    expect(testState.openPullRequestLink).toHaveBeenCalledWith(
      (testState.localApi as { shell: unknown }).shell,
      "https://example.com/pr/9",
    );

    // Without a local API the click is a no-op.
    testState.openPullRequestLink.mockClear();
    testState.localApi = undefined;
    render(buildProps());
    buttonsByText("View PR")[0]?.onClick?.();
    await flushPromises();
    expect(testState.openPullRequestLink).not.toHaveBeenCalled();
  });

  it("routes per-file staging toggles by the file's area", () => {
    render(buildProps());

    sectionByTitle("Staged Changes")?.onToggle(STAGED_FILE.path);
    expect(testState.runUnstage).toHaveBeenCalledWith([STAGED_FILE.path]);

    sectionByTitle("Changes")?.onToggle(UNSTAGED_FILE.path);
    expect(testState.runStage).toHaveBeenCalledWith([UNSTAGED_FILE.path]);
  });

  it("wires bulk section actions to stage, unstage and discard", async () => {
    render(buildProps());

    sectionByTitle("Staged Changes")?.primaryAction?.onClick();
    expect(testState.runUnstage).toHaveBeenCalledWith([STAGED_FILE.path]);

    sectionByTitle("Changes")?.primaryAction?.onClick();
    expect(testState.runStage).toHaveBeenCalledWith([UNSTAGED_FILE.path]);

    sectionByTitle("Untracked Files")?.primaryAction?.onClick();
    expect(testState.runStage).toHaveBeenLastCalledWith([UNTRACKED_FILE.path]);

    // Stage failures surface a toast; interruptions stay silent.
    testState.runStage.mockResolvedValueOnce(
      AsyncResult.failure(Cause.fail(new Error("stage exploded"))),
    );
    sectionByTitle("Changes")?.primaryAction?.onClick();
    await flushPromises();
    expect(testState.toast.add).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not stage files", description: "stage exploded" }),
    );

    testState.toast.add.mockClear();
    testState.runStage.mockResolvedValueOnce(AsyncResult.failure(Cause.interrupt(1)));
    sectionByTitle("Changes")?.primaryAction?.onClick();
    await flushPromises();
    expect(testState.toast.add).not.toHaveBeenCalled();
  });

  it("confirms bulk discards before deleting anything", async () => {
    render(buildProps());

    sectionByTitle("Changes")?.onDiscard?.();
    let markup = rerender();
    expect(markup).toContain("Discard changes?");
    expect(markup).toContain("Discard 1 file? This cannot be undone.");

    // The confirm button runs the discard with the pending paths.
    buttonByExactText("Discard")?.onClick?.();
    await flushPromises();
    expect(testState.runDiscard).toHaveBeenCalledWith([UNSTAGED_FILE.path]);
    rerender();
    expect(captured.dialogs[1]?.open).toBe(false);

    // The untracked section confirms with delete copy and a destructive button.
    sectionByTitle("Untracked Files")?.onDiscard?.();
    markup = rerender();
    expect(markup).toContain("Delete untracked files?");
    buttonsByText("Delete")[0]?.onClick?.();
    await flushPromises();
    expect(testState.runDiscard).toHaveBeenLastCalledWith([UNTRACKED_FILE.path]);
  });

  it("confirms a toolbar discard-all action for every changed file", async () => {
    render(buildProps());

    buttonsByText("Discard All")[0]?.onClick?.();
    const markup = rerender();
    expect(markup).toContain("Discard changes?");
    expect(markup).toContain("Discard 3 files? This cannot be undone.");

    buttonByExactText("Discard")?.onClick?.();
    await flushPromises();
    expect(testState.runDiscard).toHaveBeenCalledWith([
      STAGED_FILE.path,
      UNSTAGED_FILE.path,
      UNTRACKED_FILE.path,
    ]);
  });

  it("unstages staged files before a toolbar discard-all action", async () => {
    testState.statusQuery.data = stagingStatus({
      workingTree: {
        files: [STAGED_FILE],
        insertions: STAGED_FILE.insertions,
        deletions: STAGED_FILE.deletions,
      },
    });
    render(buildProps());

    buttonsByText("Discard All")[0]?.onClick?.();
    rerender();
    buttonByExactText("Discard")?.onClick?.();
    await flushPromises();

    expect(testState.runUnstage).toHaveBeenCalledWith([STAGED_FILE.path]);
    expect(testState.runDiscard).toHaveBeenCalledWith([STAGED_FILE.path]);
    const unstageCallOrder = testState.runUnstage.mock.invocationCallOrder[0];
    const discardCallOrder = testState.runDiscard.mock.invocationCallOrder[0];
    expect(unstageCallOrder).toBeDefined();
    expect(discardCallOrder).toBeDefined();
    expect(unstageCallOrder!).toBeLessThan(discardCallOrder!);
  });

  it("selects multiple files and confirms a bulk discard for the selected paths", async () => {
    render(buildProps());
    buttonByExactText("Select")?.onClick?.();
    rerender();
    sectionByTitle("Changes")?.onSelect?.(UNSTAGED_FILE.path, true);
    sectionByTitle("Untracked Files")?.onSelect?.(UNTRACKED_FILE.path, true);

    rerender();
    expect(sectionByTitle("Changes")?.selected?.(UNSTAGED_FILE)).toBe(true);
    expect(sectionByTitle("Untracked Files")?.selected?.(UNTRACKED_FILE)).toBe(true);
    expect(
      captured.buttons.find((button) => button["aria-label"] === "Discard selected files"),
    ).toMatchObject({ children: "Discard", disabled: false });
    expect(
      captured.buttons.find((button) => button["aria-label"] === "Ignore selected files"),
    ).toMatchObject({ children: "Ignore", disabled: false });

    captured.buttons
      .find((button) => button["aria-label"] === "Discard selected files")
      ?.onClick?.();
    rerender();
    buttonsByText("Discard").at(-1)?.onClick?.();
    await flushPromises();
    expect(testState.runDiscard).toHaveBeenCalledWith([UNSTAGED_FILE.path, UNTRACKED_FILE.path]);
    rerender();
    expect(sectionByTitle("Changes")?.selectionMode).toBe(false);
    expect(sectionByTitle("Changes")?.selected?.(UNSTAGED_FILE)).toBe(false);
  });

  it("preserves selection mode when selected discard fails", async () => {
    testState.runDiscard.mockResolvedValueOnce(
      AsyncResult.failure(Cause.fail(new Error("discard failed"))),
    );
    render(buildProps());
    buttonByExactText("Select")?.onClick?.();
    rerender();
    sectionByTitle("Changes")?.onSelect?.(UNSTAGED_FILE.path, true);
    rerender();

    captured.buttons
      .find((button) => button["aria-label"] === "Discard selected files")
      ?.onClick?.();
    rerender();
    captured.buttons
      .find(
        (button) =>
          flattenText(button.children) === "Discard" &&
          button["aria-label"] !== "Discard selected files",
      )
      ?.onClick?.();
    await flushPromises();
    rerender();

    expect(sectionByTitle("Changes")?.selectionMode).toBe(true);
    expect(sectionByTitle("Changes")?.selected?.(UNSTAGED_FILE)).toBe(true);
    expect(testState.toast.add).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not discard files" }),
    );

    const toastCount = testState.toast.add.mock.calls.length;
    testState.runDiscard.mockResolvedValueOnce(AsyncResult.failure(Cause.interrupt(1)));
    captured.buttons
      .find((button) => button["aria-label"] === "Discard selected files")
      ?.onClick?.();
    rerender();
    captured.buttons
      .find(
        (button) =>
          flattenText(button.children) === "Discard" &&
          button["aria-label"] !== "Discard selected files",
      )
      ?.onClick?.();
    await flushPromises();
    rerender();

    expect(sectionByTitle("Changes")?.selectionMode).toBe(true);
    expect(sectionByTitle("Changes")?.selected?.(UNSTAGED_FILE)).toBe(true);
    expect(testState.toast.add).toHaveBeenCalledTimes(toastCount);
  });

  it("counts and submits a partially staged path only once", async () => {
    const path = "src/partial.ts";
    const stagedFile = { ...STAGED_FILE, path };
    const unstagedFile = { ...UNSTAGED_FILE, path };
    testState.statusQuery.data = stagingStatus({
      workingTree: {
        files: [stagedFile, unstagedFile],
        insertions: stagedFile.insertions + unstagedFile.insertions,
        deletions: stagedFile.deletions + unstagedFile.deletions,
      },
    });
    render(buildProps());
    buttonByExactText("Select")?.onClick?.();
    rerender();
    sectionByTitle("Changes")?.onSelect?.(path, true);

    const markup = rerender();
    expect(markup).toContain("1 file selected");
    expect(markup).not.toContain("2 files selected");

    captured.buttons
      .find((button) => button["aria-label"] === "Discard selected files")
      ?.onClick?.();
    rerender();
    buttonsByText("Discard").at(-1)?.onClick?.();
    await flushPromises();

    expect(testState.runUnstage).toHaveBeenCalledWith([path]);
    expect(testState.runDiscard).toHaveBeenCalledWith([path]);
  });

  it("labels selected untracked bulk removal as delete", () => {
    render(buildProps());
    buttonByExactText("Select")?.onClick?.();
    rerender();
    sectionByTitle("Untracked Files")?.onSelect?.(UNTRACKED_FILE.path, true);

    rerender();
    expect(
      captured.buttons.find((button) => button["aria-label"] === "Delete selected files"),
    ).toMatchObject({ children: "Delete", disabled: false });
  });

  it("adds selected file names to .gitignore and removes the selected files", async () => {
    render(buildProps());
    buttonByExactText("Select")?.onClick?.();
    rerender();
    sectionByTitle("Changes")?.onSelect?.(UNSTAGED_FILE.path, true);
    sectionByTitle("Untracked Files")?.onSelect?.(UNTRACKED_FILE.path, true);
    rerender();

    captured.buttons
      .find((button) => button["aria-label"] === "Ignore selected files")
      ?.onClick?.();
    await flushPromises();

    expect(testState.readProjectFile).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { cwd: GIT_CWD, relativePath: ".gitignore" },
    });
    expect(testState.writeProjectFile).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: {
        cwd: GIT_CWD,
        relativePath: ".gitignore",
        contents: "node_modules/\nb.ts\nnew.md\n",
      },
    });
    expect(testState.runDiscard).toHaveBeenCalledWith([UNSTAGED_FILE.path, UNTRACKED_FILE.path]);
    rerender();
    expect(sectionByTitle("Changes")?.selectionMode).toBe(false);
    expect(sectionByTitle("Changes")?.selected?.(UNSTAGED_FILE)).toBe(false);
  });

  it("locks a selection session while selected ignore is in flight", async () => {
    let resolveWrite!: (value: unknown) => void;
    testState.writeProjectFile.mockReturnValueOnce(
      new Promise((resolve) => {
        resolveWrite = resolve;
      }),
    );
    render(buildProps());
    const enterSelectionMode = buttonByExactText("Select")?.onClick;
    enterSelectionMode?.();
    rerender();
    sectionByTitle("Changes")?.onSelect?.(UNSTAGED_FILE.path, true);
    rerender();

    captured.buttons
      .find((button) => button["aria-label"] === "Ignore selected files")
      ?.onClick?.();
    await flushPromises();
    rerender();

    expect(buttonByExactText("Cancel")?.disabled).toBe(true);
    expect(
      captured.buttons.find((button) => button["aria-label"] === "Discard selected files")
        ?.disabled,
    ).toBe(true);
    expect(
      captured.buttons.find((button) => button["aria-label"] === "Ignore selected files")?.disabled,
    ).toBe(true);
    expect(sectionByTitle("Changes")?.disabled).toBe(true);

    buttonByExactText("Cancel")?.onClick?.();
    enterSelectionMode?.();
    sectionByTitle("Untracked Files")?.onSelect?.(UNTRACKED_FILE.path, true);
    captured.buttons
      .find((button) => button["aria-label"] === "Ignore selected files")
      ?.onClick?.();
    captured.buttons
      .find((button) => button["aria-label"] === "Discard selected files")
      ?.onClick?.();
    rerender();

    expect(sectionByTitle("Changes")?.selectionMode).toBe(true);
    expect(sectionByTitle("Changes")?.selected?.(UNSTAGED_FILE)).toBe(true);
    expect(sectionByTitle("Untracked Files")?.selected?.(UNTRACKED_FILE)).toBe(false);
    expect(testState.readProjectFile).toHaveBeenCalledOnce();
    expect(captured.dialogs[1]?.open).toBe(false);

    resolveWrite(AsyncResult.success({ relativePath: ".gitignore" }));
    await flushPromises();
    rerender();

    expect(testState.runDiscard).toHaveBeenCalledWith([UNSTAGED_FILE.path]);
    expect(sectionByTitle("Changes")?.selectionMode).toBe(false);
    expect(sectionByTitle("Changes")?.selected?.(UNSTAGED_FILE)).toBe(false);
  });

  it("preserves selection mode when selected ignore fails", async () => {
    testState.writeProjectFile.mockResolvedValueOnce(
      AsyncResult.failure(Cause.fail(new Error("write failed"))),
    );
    render(buildProps());
    buttonByExactText("Select")?.onClick?.();
    rerender();
    sectionByTitle("Changes")?.onSelect?.(UNSTAGED_FILE.path, true);
    rerender();

    captured.buttons
      .find((button) => button["aria-label"] === "Ignore selected files")
      ?.onClick?.();
    await flushPromises();
    rerender();

    expect(sectionByTitle("Changes")?.selectionMode).toBe(true);
    expect(sectionByTitle("Changes")?.selected?.(UNSTAGED_FILE)).toBe(true);
    expect(testState.toast.add).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not update .gitignore" }),
    );
  });

  it("preserves selection mode when reading .gitignore is interrupted", async () => {
    testState.readProjectFile.mockResolvedValueOnce(AsyncResult.failure(Cause.interrupt(1)));
    render(buildProps());
    buttonByExactText("Select")?.onClick?.();
    rerender();
    sectionByTitle("Changes")?.onSelect?.(UNSTAGED_FILE.path, true);
    rerender();

    captured.buttons
      .find((button) => button["aria-label"] === "Ignore selected files")
      ?.onClick?.();
    await flushPromises();
    rerender();

    expect(sectionByTitle("Changes")?.selectionMode).toBe(true);
    expect(sectionByTitle("Changes")?.selected?.(UNSTAGED_FILE)).toBe(true);
    expect(testState.writeProjectFile).not.toHaveBeenCalled();
    expect(testState.runDiscard).not.toHaveBeenCalled();
    expect(testState.toast.add).not.toHaveBeenCalled();
  });

  it("adds a row's parent folder to .gitignore", async () => {
    render(buildProps());
    sectionByTitle("Untracked Files")?.onIgnoreParentFolder?.(UNTRACKED_FILE.path);
    await flushPromises();

    expect(testState.writeProjectFile).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: {
        cwd: GIT_CWD,
        relativePath: ".gitignore",
        contents: "node_modules/\ndocs/\n",
      },
    });
    expect(testState.runDiscard).toHaveBeenCalledWith([UNTRACKED_FILE.path]);
  });

  it("confirms single-entry discards and supports cancel", async () => {
    render(buildProps());

    sectionByTitle("Untracked Files")?.onRequestDiscardFile?.(UNTRACKED_FILE);
    const markup = rerender();
    expect(markup).toContain("Delete untracked file?");
    expect(markup).toContain("new.md");

    // Cancel (second dialog's cancel button) closes without discarding.
    captured.dialogs[1]?.onOpenChange(false);
    rerender();
    expect(captured.dialogs[1]?.open).toBe(false);
    expect(testState.runDiscard).not.toHaveBeenCalled();
  });

  it("drafts the commit message and generates one on demand", async () => {
    render(buildProps());
    captured.textareas[0]?.onChange({ target: { value: "typed message" } });
    expect(testState.setMessage).toHaveBeenCalledWith("typed message");

    const tree = callPanel();
    const generate = nativeButton(
      tree,
      (props) => props["aria-label"] === "Generate commit message with AI",
    );
    (generate!.props.onClick as () => void)();
    await flushPromises();
    // Staged files exist, so generation scopes to them.
    expect(testState.runGenerate).toHaveBeenCalledWith({ filePaths: [STAGED_FILE.path] });
    expect(testState.setMessage).toHaveBeenLastCalledWith("Generated message");
  });

  it("ignores canceled generations and surfaces generation failures", async () => {
    let resolveRun: (value: unknown) => void = () => {};
    testState.runGenerate = vi.fn().mockReturnValue(
      new Promise((resolve) => {
        resolveRun = resolve;
      }),
    );
    let tree = callPanel(buildProps());
    const generate = nativeButton(
      tree,
      (props) => props["aria-label"] === "Generate commit message with AI",
    );
    (generate!.props.onClick as () => void)();

    // While pending, the button becomes a stop control that bumps the token.
    testState.generatePending = true;
    tree = callPanel();
    const stop = nativeButton(tree, (props) => props["aria-label"] === "Stop generating");
    expect(stop).toBeDefined();
    (stop!.props.onClick as () => void)();

    resolveRun(AsyncResult.success({ message: "Too late" }));
    await flushPromises();
    expect(testState.setMessage).not.toHaveBeenCalled();

    // Failures surface a toast.
    testState.generatePending = false;
    testState.runGenerate = vi
      .fn()
      .mockResolvedValue(AsyncResult.failure(Cause.fail(new Error("generation exploded"))));
    tree = callPanel();
    const retry = nativeButton(
      tree,
      (props) => props["aria-label"] === "Generate commit message with AI",
    );
    (retry!.props.onClick as () => void)();
    await flushPromises();
    expect(testState.toast.add).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Could not generate a commit message",
        description: "generation exploded",
      }),
    );

    // Whitespace-only results leave the draft untouched.
    testState.runGenerate = vi.fn().mockResolvedValue(AsyncResult.success({ message: "   " }));
    tree = callPanel();
    const whitespaceGenerate = nativeButton(
      tree,
      (props) => props["aria-label"] === "Generate commit message with AI",
    );
    (whitespaceGenerate!.props.onClick as () => void)();
    await flushPromises();
    expect(testState.setMessage).not.toHaveBeenCalled();
  });

  it("copies relative and absolute paths to the clipboard", () => {
    render(buildProps());
    const section = sectionByTitle("Staged Changes");
    const writeText = (
      navigator as unknown as { clipboard: { writeText: ReturnType<typeof vi.fn> } }
    ).clipboard.writeText;

    section?.onCopyPath?.(STAGED_FILE.path, true);
    expect(writeText).toHaveBeenCalledWith(STAGED_FILE.path);

    section?.onCopyPath?.(STAGED_FILE.path, false);
    expect(writeText).toHaveBeenLastCalledWith(joinWorkspacePath(GIT_CWD, STAGED_FILE.path));
  });

  it("opens files in the preferred external editor", () => {
    render(buildProps());
    sectionByTitle("Staged Changes")?.onOpenExternalEditor?.(STAGED_FILE.path);
    expect(testState.openInEditor).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { cwd: joinWorkspacePath(GIT_CWD, STAGED_FILE.path), editor: "vscode" },
    });

    // Without a preferred editor the action is a no-op.
    testState.openInEditor.mockClear();
    testState.preferredEditor = null;
    render(buildProps());
    sectionByTitle("Staged Changes")?.onOpenExternalEditor?.(STAGED_FILE.path);
    expect(testState.openInEditor).not.toHaveBeenCalled();
  });

  it("opens the diff panel scoped by staging area", () => {
    render(buildProps());
    const section = sectionByTitle("Staged Changes");

    section?.onOpenFile(STAGED_FILE.path, "staged");
    expect(testState.rightPanelOpen).toHaveBeenCalledWith(THREAD_REF, "diff");
    expect(testState.selectGitScope).toHaveBeenCalledWith(THREAD_REF, "branch");

    section?.onOpenFile(UNSTAGED_FILE.path, "unstaged");
    expect(testState.selectGitScope).toHaveBeenLastCalledWith(THREAD_REF, "unstaged");
  });

  it("disables everything while a source-control action runs", () => {
    testState.isBusy = true;
    const markup = render(buildProps());
    expect(markup).toContain('data-testid="spinner"');
    const commit = buttonsByText("Commit (1)")[0];
    expect(commit?.disabled).toBe(true);
    expect(markup).toContain('data-testid="menu-trigger" data-disabled="true"');
    expect(sectionByTitle("Staged Changes")?.disabled).toBe(true);
  });
});

describe("SourceControlPanel — Merge into current branch", () => {
  const PROJECT_REF = { environmentId: ENVIRONMENT_ID, projectId: "project-1" } as NonNullable<
    PanelProps["projectRef"]
  >;

  it("opens the merge dialog in current-branch mode for the thread's project", () => {
    testState.statusQuery = { ...testState.statusQuery, data: status() };
    render(buildProps({ projectRef: PROJECT_REF }));
    const item = menuItemByText("Merge into current branch…");
    expect(item?.disabled).toBeFalsy();
    expect(captured.merge?.open).toBe(false);

    item?.onClick?.();
    render(buildProps({ projectRef: PROJECT_REF }));

    expect(captured.merge).toMatchObject({
      open: true,
      targetMode: "current-branch",
      projectRef: PROJECT_REF,
      scope: { environmentId: ENVIRONMENT_ID, cwd: GIT_CWD },
    });
  });

  it("explains why merging is unavailable without the Git Manager capabilities", () => {
    testState.mergeCapabilities = false;
    testState.statusQuery = { ...testState.statusQuery, data: status() };
    render(buildProps({ projectRef: PROJECT_REF }));
    const item = menuItemByText("Merge into current branch…");
    expect(item).toMatchObject({
      disabled: true,
      title: "This environment does not support Git Manager stash and merge operations.",
    });
  });

  it("disables the panel's actions while the dialog's merge or fetch runs", () => {
    testState.statusQuery = { ...testState.statusQuery, data: status() };
    render(buildProps({ projectRef: PROJECT_REF }));
    expect(menuItemByText("Pull")?.title).toBe("Already up to date.");
    expect(captured.merge?.onRunningChange).toBeTypeOf("function");
    captured.merge?.onRunningChange?.(true);
    render(buildProps({ projectRef: PROJECT_REF }));
    expect(menuItemByText("Pull")).toMatchObject({
      disabled: true,
      title: "Git action in progress.",
    });
  });
});

describe("SourceControlPanel — merge in progress", () => {
  const PROJECT_REF = { environmentId: ENVIRONMENT_ID, projectId: "project-1" } as NonNullable<
    PanelProps["projectRef"]
  >;
  const CONFLICTED = {
    path: "a.txt",
    insertions: 0,
    deletions: 0,
    status: "conflicted",
    area: "unstaged",
  } as const;
  const RESOLVED = {
    path: "a.txt",
    insertions: 1,
    deletions: 1,
    status: "modified",
    area: "staged",
  } as const;

  function mergeStatus(files: readonly WorkingTreeFile[]): VcsStatusResult {
    return status({
      operationInProgress: "merge",
      hasWorkingTreeChanges: true,
      workingTree: { files: [...files] as never, insertions: 1, deletions: 1 },
    });
  }

  it("keeps Commit merge disabled while conflicts remain and resolves a file", () => {
    testState.statusQuery = { ...testState.statusQuery, data: mergeStatus([CONFLICTED]) };
    render(buildProps({ projectRef: PROJECT_REF }));

    expect(captured.strip).toMatchObject({
      continueLabel: "Commit merge",
      continueDisabledReason: "Resolve and stage every conflicted file first.",
      disabledReason: null,
      operation: { kind: "merge" },
    });
    expect(captured.mergeChanges?.files.map((file) => file.path)).toEqual(["a.txt"]);
    expect(sectionByTitle("Changes")?.files ?? []).toEqual([]);

    captured.mergeChanges?.onResolve("a.txt", "ours");
    expect(testState.runGitManagerOperation).toHaveBeenCalledWith(
      {
        environmentId: ENVIRONMENT_ID,
        input: {
          _tag: "resolve-conflict",
          cwd: GIT_CWD,
          projectId: "project-1",
          path: "a.txt",
          side: "ours",
        },
      },
      expect.any(Function),
    );
  });

  it("finishes the merge through continue and hides the stacked Commit", () => {
    testState.statusQuery = { ...testState.statusQuery, data: mergeStatus([RESOLVED]) };
    render(buildProps({ projectRef: PROJECT_REF }));

    expect(captured.strip?.continueDisabledReason).toBeNull();
    expect(menuItemByText("Commit")).toBeUndefined();
    expect(buttonByExactText("Commit")).toBeUndefined();

    captured.strip?.onContinue();
    expect(testState.runGitManagerOperation).toHaveBeenCalledWith(
      {
        environmentId: ENVIRONMENT_ID,
        input: { _tag: "continue", cwd: GIT_CWD, projectId: "project-1", operation: "merge" },
      },
      expect.any(Function),
    );
  });

  it("marks the panel busy and offers Cancel while a recovery operation runs", () => {
    testState.statusQuery = { ...testState.statusQuery, data: mergeStatus([RESOLVED]) };
    render(buildProps({ projectRef: PROJECT_REF }));
    captured.strip?.onAbort();
    render(buildProps({ projectRef: PROJECT_REF }));

    expect(captured.strip?.disabledReason).toBe("A merge is running.");
    expect(captured.banner?.operation).toMatchObject({ _tag: "started", operation: "abort" });
    expect(captured.banner?.onCancel).toBeTypeOf("function");
    expect(sectionByTitle("Staged Changes")?.disabled).toBe(true);
  });

  it("disables both strip actions without the Git Manager capabilities", () => {
    testState.mergeCapabilities = false;
    testState.statusQuery = { ...testState.statusQuery, data: mergeStatus([CONFLICTED]) };
    render(buildProps({ projectRef: PROJECT_REF }));

    expect(captured.strip?.disabledReason).toBe(
      "This environment does not support Git Manager stash and merge operations.",
    );
  });
});

describe("SourceControlPanel — merge recovery follow-through", () => {
  const PROJECT_REF = { environmentId: ENVIRONMENT_ID, projectId: "project-1" } as NonNullable<
    PanelProps["projectRef"]
  >;
  const CONFLICTED = {
    path: "a.txt",
    insertions: 0,
    deletions: 0,
    status: "conflicted",
    area: "unstaged",
  } as const;

  function inProgress(kind: "merge" | "rebase", files: readonly WorkingTreeFile[]) {
    return status({
      operationInProgress: kind,
      hasWorkingTreeChanges: true,
      workingTree: { files: [...files] as never, insertions: 0, deletions: 0 },
    });
  }

  it("stages a manually resolved conflict without overwriting it", () => {
    testState.statusQuery = { ...testState.statusQuery, data: inProgress("merge", [CONFLICTED]) };
    render(buildProps({ projectRef: PROJECT_REF }));
    captured.mergeChanges?.onMarkResolved("a.txt");
    expect(testState.runStage).toHaveBeenCalledWith(["a.txt"]);
  });

  it("explains that conflicted files cannot open without an external editor", () => {
    testState.preferredEditor = null as never;
    testState.availableEditors = [];
    testState.statusQuery = { ...testState.statusQuery, data: inProgress("merge", [CONFLICTED]) };
    render(buildProps({ projectRef: PROJECT_REF }));
    expect(captured.mergeChanges?.openInEditorDisabledReason).toBeTruthy();

    testState.preferredEditor = "vscode";
    testState.availableEditors = ["vscode"];
    render(buildProps({ projectRef: PROJECT_REF }));
    expect(captured.mergeChanges?.openInEditorDisabledReason).toBeNull();
  });

  it("keeps conflicts in the ordinary sections outside a merge", () => {
    testState.statusQuery = { ...testState.statusQuery, data: inProgress("rebase", [CONFLICTED]) };
    render(buildProps({ projectRef: PROJECT_REF }));
    expect(captured.mergeChanges).toBeNull();
    expect(sectionByTitle("Changes")?.files.map((file) => file.path)).toEqual(["a.txt"]);
  });

  it("shows a transport failure instead of a stuck running banner", async () => {
    testState.runGitManagerOperation = vi.fn(() => ({
      result: Promise.resolve(AsyncResult.failure(Cause.fail(new Error("Socket closed.")))),
      cancel: vi.fn(),
    }));
    testState.statusQuery = { ...testState.statusQuery, data: inProgress("merge", []) };
    render(buildProps({ projectRef: PROJECT_REF }));
    captured.strip?.onContinue();
    await flushPromises();
    render(buildProps({ projectRef: PROJECT_REF }));
    expect(captured.banner?.operation).toMatchObject({
      _tag: "failed",
      operation: "continue",
      code: "transport-error",
      message: "Socket closed.",
    });
  });

  it("reloads the commit history after Commit merge finishes", () => {
    testState.statusQuery = { ...testState.statusQuery, data: inProgress("merge", []) };
    render(buildProps({ projectRef: PROJECT_REF }));
    const before = captured.commits.at(-1)?.reloadToken ?? 0;
    captured.strip?.onContinue();
    const onEvent = testState.runGitManagerOperation.mock.calls.at(-1)?.[1] as (
      event: unknown,
    ) => void;
    onEvent({ _tag: "finished", operation: "continue", message: "Merged." });
    render(buildProps({ projectRef: PROJECT_REF }));
    expect(captured.commits.at(-1)?.reloadToken).toBe(before + 1);
  });
});
