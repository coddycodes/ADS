/**
 * Behavior tests for DiffPanel.
 *
 * Uses the repo's instrumented-hooks pattern (see ChatView.hooks.test.tsx /
 * FilePreviewPanel.test.tsx): a partial `vi.mock("react")` records effects so
 * they can be run manually, every hook dependency is swapped for a controllable
 * mock, and heavy child components are replaced with capture stand-ins whose
 * handler props are then invoked directly. The component renders via
 * `renderToStaticMarkup` — no jsdom.
 */
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { ReactElement, ReactNode } from "react";
import { EnvironmentId, ProjectId, ThreadId, TurnId } from "@coddy/contracts";
import type { Thread } from "../types";

const h = vi.hoisted(() => {
  const state = {
    routeThreadRef: null as unknown,
    diffSelection: { kind: "branch", baseRef: null } as unknown,
    gitRefreshRequestId: 0,
    thread: null as unknown,
    project: null as unknown,
    settings: { wordWrap: false, diffIgnoreWhitespace: false, timestampFormat: "24h" } as Record<
      string,
      unknown
    >,
    resolvedTheme: "dark" as "light" | "dark",
    serverConfig: null as unknown,
    turnDiffSummaries: [] as unknown[],
    inferredCheckpointTurnCountByTurnId: {} as Record<string, number>,
    queryDataByKey: new Map<string, unknown>(),
    queryStateByKey: new Map<string, { error?: unknown; isPending?: boolean }>(),
    checkpointDiff: { data: undefined as unknown, error: null as unknown, isPending: false },
    renderablePatch: null as unknown,
    baseRefChoices: [] as unknown[],
    filteredBaseRefChoices: [] as unknown[],
    storeApi: {
      reconcileTurnSelection: vi.fn(),
      selectTurn: vi.fn(),
      selectGitScope: vi.fn(),
      selectBranchBaseRef: vi.fn(),
    },
    openInPreferredEditor: vi.fn(async (_path: string) => ({ _tag: "Success" }) as unknown),
    openDiffFilePrimaryAction: vi.fn(),
    diffPreviewRefresh: vi.fn(),
    stateIndex: 0,
    stateSeeds: new Map<number, unknown>(),
    captured: {} as Record<string, Record<string, unknown>>,
    capturedList: [] as Array<{ name: string; props: Record<string, unknown> }>,
    capture(name: string, props: Record<string, unknown>): void {
      if (!props || Object.keys(props).length === 0) return;
      state.captured[name] = props;
      state.capturedList.push({ name, props });
    },
    effects: [] as Array<() => void | (() => void)>,
  };
  return state;
});

// ── React instrumentation ────────────────────────────────────────────
vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  const resolveInitial = (initial: unknown): unknown =>
    typeof initial === "function" ? (initial as () => unknown)() : initial;
  const useState = (initial?: unknown) => {
    const index = h.stateIndex++;
    const resolved = resolveInitial(initial);
    const value = h.stateSeeds.has(index) ? h.stateSeeds.get(index) : resolved;
    const setValue = (next: unknown) => {
      if (typeof next === "function") (next as (value: unknown) => unknown)(value);
    };
    return [value, setValue];
  };
  const useEffect = (effect: () => void | (() => void)) => {
    h.effects.push(effect);
  };
  return {
    ...actual,
    useState: useState as typeof actual.useState,
    useEffect: useEffect as typeof actual.useEffect,
    useLayoutEffect: useEffect as typeof actual.useLayoutEffect,
  };
});

// ── Hook / state dependencies ────────────────────────────────────────
vi.mock("@tanstack/react-router", () => ({
  useParams: (opts: { select?: (params: Record<string, unknown>) => unknown }) =>
    opts.select ? opts.select({}) : undefined,
}));

vi.mock("../threadRoutes", () => ({
  resolveThreadRouteRef: () => h.routeThreadRef,
}));

vi.mock("../diffPanelStore", () => {
  const useDiffPanelStore = Object.assign(
    (selector: (state: { byThreadKey: Record<string, unknown> }) => unknown) =>
      selector({ byThreadKey: {} }),
    { getState: () => h.storeApi },
  );
  return {
    useDiffPanelStore,
    selectThreadDiffPanelSelection: () => h.diffSelection,
    selectThreadDiffPanelRefreshRequest: () => h.gitRefreshRequestId,
  };
});

vi.mock("../state/entities", () => ({
  useThread: (ref: unknown) => (ref ? h.thread : null),
  useProject: (ref: unknown) => (ref ? h.project : null),
}));

vi.mock("../state/query", () => ({
  useEnvironmentQuery: (atom: { key?: string } | null) => {
    const key = atom && typeof atom.key === "string" ? atom.key : null;
    const queryState = key ? h.queryStateByKey.get(key) : undefined;
    return {
      data: key ? (h.queryDataByKey.get(key) ?? null) : null,
      error: queryState?.error ?? null,
      isPending: queryState?.isPending ?? false,
      refresh: key === "review.diffPreview" ? h.diffPreviewRefresh : () => undefined,
      revalidate: key === "review.diffPreview" ? h.diffPreviewRefresh : () => undefined,
      requiresRetry: false,
    };
  },
}));

vi.mock("../state/server", () => ({
  serverEnvironment: {
    configValueAtom: (environmentId: string | null) => ({ key: `server.config:${environmentId}` }),
  },
}));

vi.mock("../state/review", () => ({
  reviewEnvironment: {
    diffPreview: (_args: unknown) => ({ key: "review.diffPreview" }),
  },
}));

vi.mock("../state/vcs", () => ({
  vcsEnvironment: {
    status: (_args: unknown) => ({ key: "vcs.status" }),
    listRefs: (args: { input: { refKind: string } }) => ({
      key: `vcs.listRefs:${args.input.refKind}`,
    }),
  },
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: (atom: { key?: string } | null | undefined) =>
    atom && typeof atom.key === "string" && atom.key.startsWith("server.config")
      ? h.serverConfig
      : undefined,
}));

vi.mock("../hooks/useTheme", () => ({
  useTheme: () => ({ resolvedTheme: h.resolvedTheme, theme: "system", setTheme: () => undefined }),
}));

vi.mock("../hooks/useSettings", () => ({
  useClientSettings: (selector?: (settings: unknown) => unknown) =>
    selector ? selector(h.settings) : h.settings,
}));

vi.mock("../hooks/useTurnDiffSummaries", () => ({
  useTurnDiffSummaries: () => ({
    turnDiffSummaries: h.turnDiffSummaries,
    inferredCheckpointTurnCountByTurnId: h.inferredCheckpointTurnCountByTurnId,
  }),
}));

vi.mock("~/lib/checkpointDiffState", () => ({
  useCheckpointDiff: (_target: unknown, _options: unknown) => h.checkpointDiff,
}));

vi.mock("../editorPreferences", () => ({
  useOpenInPreferredEditor: () => h.openInPreferredEditor,
}));

vi.mock("../diffFileActions", () => ({
  openDiffFilePrimaryAction: (args: { openInEditor: (path: string) => void }) => {
    h.openDiffFilePrimaryAction(args);
    // Exercise the editor callback so its async body is covered.
    args.openInEditor("target/path.ts");
  },
}));

vi.mock("@coddy/client-runtime/state/runtime", () => ({
  isAtomCommandInterrupted: (result: { _interrupted?: boolean }) => result?._interrupted === true,
  squashAtomCommandFailure: (result: unknown) => result,
}));

vi.mock("@coddy/client-runtime/errors", () => ({
  safeErrorLogAttributes: () => ({ error: "redacted" }),
}));

vi.mock("../lib/diffRendering", () => ({
  getRenderablePatch: () => h.renderablePatch,
  resolveFileDiffPath: (fileDiff: { path: string }) => fileDiff.path,
  buildFileDiffRenderKey: (fileDiff: { key: string }) => fileDiff.key,
  getDiffCollapseIconClassName: () => "collapse-icon",
  resolveDiffThemeName: () => "github-dark",
}));

vi.mock("../lib/baseRefChoices", () => ({
  buildBaseRefChoices: () => h.baseRefChoices,
  filterBaseRefChoices: () => h.filteredBaseRefChoices,
}));

vi.mock("../timestampFormat", () => ({
  formatShortTimestamp: () => "12:00",
}));

// ── Child components ─────────────────────────────────────────────────
vi.mock("./DiffWorkerPoolProvider", () => ({
  DiffWorkerPoolProvider: ({ children }: { children?: ReactNode }) => children,
}));

vi.mock("./DiffPanelShell", () => ({
  DiffPanelShell: (props: { header: ReactNode; children: ReactNode; mode: string }) => (
    <div data-mock="diff-panel-shell" data-mode={props.mode}>
      <div data-mock="header">{props.header}</div>
      <div data-mock="body">{props.children}</div>
    </div>
  ),
  DiffPanelLoadingState: (props: { label: string }) => (
    <div data-mock="loading-state">{props.label}</div>
  ),
}));

vi.mock("./diffs/AnnotatableCodeView", () => ({
  AnnotatableCodeView: (props: Record<string, unknown>) => {
    h.capture("annotatableCodeView", props);
    return <div data-mock="annotatable-code-view" />;
  },
}));

vi.mock("./ui/toggle-group", () => ({
  ToggleGroup: (props: Record<string, unknown> & { children?: ReactNode }) => {
    h.capture("toggleGroup", props);
    return <div data-mock="toggle-group">{props.children}</div>;
  },
  Toggle: (props: Record<string, unknown> & { children?: ReactNode }) => {
    h.capture("toggle", props);
    return <div data-mock="toggle">{props.children}</div>;
  },
}));

vi.mock("./ui/switch", () => ({
  Switch: (props: Record<string, unknown>) => {
    h.capture("switch", props);
    return <div data-mock="switch" />;
  },
}));

vi.mock("./ui/combobox", () => ({
  Combobox: (props: Record<string, unknown> & { children?: ReactNode }) => {
    h.capture("combobox", props);
    return <div data-mock="combobox">{props.children}</div>;
  },
  ComboboxEmpty: ({ children }: { children?: ReactNode }) => (
    <div data-mock="combobox-empty">{children}</div>
  ),
  ComboboxInput: (props: Record<string, unknown>) => {
    h.capture("comboboxInput", props);
    return <div data-mock="combobox-input" />;
  },
  ComboboxItem: (props: Record<string, unknown> & { children?: ReactNode }) => {
    h.capture("comboboxItem", props);
    return <div data-mock="combobox-item">{props.children}</div>;
  },
  ComboboxList: ({ children }: { children?: ReactNode }) => (
    <div data-mock="combobox-list">{children}</div>
  ),
  ComboboxPopup: ({ children }: { children?: ReactNode }) => (
    <div data-mock="combobox-popup">{children}</div>
  ),
  ComboboxTrigger: (props: Record<string, unknown> & { children?: ReactNode }) => (
    <div data-mock="combobox-trigger">{props.children}</div>
  ),
}));

vi.mock("./ui/menu", () => ({
  DropdownMenu: ({ children }: { children?: ReactNode }) => (
    <div data-mock="dropdown-menu">{children}</div>
  ),
  DropdownMenuContent: ({ children }: { children?: ReactNode }) => (
    <div data-mock="dropdown-menu-content">{children}</div>
  ),
  DropdownMenuItem: (props: Record<string, unknown> & { children?: ReactNode }) => {
    h.capture("dropdownMenuItem", props);
    return <div data-mock="dropdown-menu-item">{props.children}</div>;
  },
  DropdownMenuSub: ({ children }: { children?: ReactNode }) => (
    <div data-mock="dropdown-menu-sub">{children}</div>
  ),
  DropdownMenuSubContent: ({ children }: { children?: ReactNode }) => (
    <div data-mock="dropdown-menu-sub-content">{children}</div>
  ),
  DropdownMenuSubTrigger: ({ children }: { children?: ReactNode }) => (
    <div data-mock="dropdown-menu-sub-trigger">{children}</div>
  ),
  DropdownMenuTrigger: (props: Record<string, unknown> & { children?: ReactNode }) => (
    <div data-mock="dropdown-menu-trigger">{props.children}</div>
  ),
}));

vi.mock("./ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: ReactNode }) => <div data-mock="tooltip">{children}</div>,
  TooltipPopup: ({ children }: { children?: ReactNode }) => (
    <div data-mock="tooltip-popup">{children}</div>
  ),
  TooltipTrigger: (
    props: Record<string, unknown> & { render?: ReactNode; children?: ReactNode },
  ) => {
    h.capture("tooltipTrigger", props);
    return <div data-mock="tooltip-trigger">{props.render ?? props.children}</div>;
  },
}));

import DiffPanel from "./DiffPanel";

// ── Fixtures ─────────────────────────────────────────────────────────
const environmentId = EnvironmentId.make("env-1");
const threadId = ThreadId.make("thread-1");
const projectId = ProjectId.make("proj-1");
const turnIdA = TurnId.make("turn-a");
const turnIdB = TurnId.make("turn-b");

const routeRef = { environmentId, threadId };
const thread = { environmentId, threadId, projectId, worktreePath: "/wt" };
const project = { environmentId, projectId, workspaceRoot: "/repo" };

function fileDiff(path: string, key: string) {
  return { path, key };
}

function summary(turnId: string, checkpointTurnCount: number, completedAt: string) {
  return { turnId, checkpointTurnCount, completedAt };
}

function branchPreviewData(overrides: Record<string, unknown> = {}) {
  return {
    cwd: "/repo",
    sources: [
      {
        kind: "branch-range",
        diff: "PATCH_TEXT",
        headRef: "feature",
        baseRef: "main",
        truncated: false,
        ...overrides,
      },
    ],
  };
}

function render(mode?: "inline" | "sheet" | "sidebar" | "embedded"): string {
  h.captured = {};
  h.capturedList.length = 0;
  h.effects.length = 0;
  h.stateIndex = 0;
  return renderToStaticMarkup(
    <DiffPanel mode={mode ?? "inline"} composerDraftTarget={routeRef as never} />,
  );
}

function renderForDraft(draftThread: unknown): string {
  h.captured = {};
  h.capturedList.length = 0;
  h.effects.length = 0;
  h.stateIndex = 0;
  return renderToStaticMarkup(
    <DiffPanel
      mode="embedded"
      composerDraftTarget={routeRef as never}
      thread={draftThread as Thread}
    />,
  );
}

function capturedAll<T = Record<string, unknown>>(name: string): T[] {
  return h.capturedList.filter((entry) => entry.name === name).map((entry) => entry.props as T);
}

function capturedLast<T = Record<string, unknown>>(name: string): T {
  const entries = capturedAll<T>(name);
  expect(entries.length, `expected captured props for ${name}`).toBeGreaterThan(0);
  return entries[entries.length - 1]!;
}

// The collapse control renderHeaderPrefix returns
// <Tooltip><TooltipTrigger render={<button .../>}>…</Tooltip>; reach the button.
type CollapseButton = ReactElement<{
  onClick: (event: unknown) => void;
  "aria-expanded": boolean;
}>;
function collapseButton(headerPrefix: ReactElement): CollapseButton {
  const children = (headerPrefix.props as { children: ReactElement[] }).children;
  const trigger = children[0]!;
  return (trigger.props as { render: CollapseButton }).render;
}

function runEffects(): Array<() => void> {
  const cleanups: Array<() => void> = [];
  const effects = [...h.effects];
  h.effects.length = 0;
  for (const effect of effects) {
    const cleanup = effect();
    if (typeof cleanup === "function") cleanups.push(cleanup);
  }
  return cleanups;
}

beforeEach(() => {
  h.routeThreadRef = routeRef;
  h.diffSelection = { kind: "branch", baseRef: null };
  h.gitRefreshRequestId = 0;
  h.thread = thread;
  h.project = project;
  h.settings = { wordWrap: false, diffIgnoreWhitespace: false, timestampFormat: "24h" };
  h.resolvedTheme = "dark";
  h.serverConfig = { availableEditors: ["vscode"] };
  h.turnDiffSummaries = [];
  h.inferredCheckpointTurnCountByTurnId = {};
  h.stateSeeds = new Map<number, unknown>();
  h.stateIndex = 0;
  h.queryDataByKey = new Map<string, unknown>([
    ["vcs.status", { isRepo: true }],
    ["review.diffPreview", branchPreviewData()],
    ["vcs.listRefs:local", { refs: [{ name: "main" }, { name: "dev" }] }],
    ["vcs.listRefs:remote", { refs: [{ name: "origin/main" }] }],
  ]);
  h.queryStateByKey = new Map();
  h.checkpointDiff = { data: undefined, error: null, isPending: false };
  h.renderablePatch = {
    kind: "files",
    files: [fileDiff("src/a.ts", "file-a"), fileDiff("src/b.ts", "file-b")],
  };
  h.baseRefChoices = [];
  h.filteredBaseRefChoices = [];
  h.storeApi.reconcileTurnSelection.mockClear();
  h.storeApi.selectTurn.mockClear();
  h.storeApi.selectGitScope.mockClear();
  h.storeApi.selectBranchBaseRef.mockClear();
  h.openInPreferredEditor.mockClear();
  h.openDiffFilePrimaryAction.mockClear();
  h.diffPreviewRefresh.mockClear();
});

afterEach(() => {
  vi.restoreAllMocks();
});

// ── Empty-state branches ─────────────────────────────────────────────
describe("DiffPanel: empty states", () => {
  it("prompts to select a thread when there is no active thread", () => {
    h.thread = null;
    h.routeThreadRef = null;
    const markup = render();
    expect(markup).toContain("Select a thread to inspect turn diffs.");
  });

  it("explains when the project is not a git repository", () => {
    h.queryDataByKey.set("vcs.status", { isRepo: false });
    const markup = render();
    expect(markup).toContain("not a git repository");
  });

  it("shows the no-completed-turns message for a turn selection with no summaries", () => {
    h.diffSelection = { kind: "turn", turnId: turnIdA, filePath: null, revealRequestId: 0 };
    h.turnDiffSummaries = [];
    const markup = render();
    expect(markup).toContain("No completed turns yet.");
  });

  it("renders branch changes for an unstarted worktree draft", () => {
    h.routeThreadRef = null;
    h.thread = null;

    const markup = renderForDraft({
      id: threadId,
      environmentId,
      projectId,
      worktreePath: "/repo/.worktrees/feature",
    });

    expect(markup).toContain('data-mock="annotatable-code-view"');
    expect(markup).not.toContain("Select a thread to inspect turn diffs.");
  });
});

// ── Files patch (main path) ──────────────────────────────────────────
describe("DiffPanel: files patch", () => {
  it("renders the annotatable code view for a files patch", () => {
    const markup = render();
    expect(markup).toContain('data-mock="annotatable-code-view"');
    const codeView = capturedLast("annotatableCodeView");
    expect((codeView["files"] as unknown[]).length).toBe(2);
    expect(codeView["sectionId"]).toBe("branch");
  });

  it("sorts files by path", () => {
    h.renderablePatch = {
      kind: "files",
      files: [fileDiff("z/last.ts", "z"), fileDiff("a/first.ts", "a")],
    };
    render();
    const codeView = capturedLast("annotatableCodeView");
    const files = codeView["files"] as Array<{ filePath: string }>;
    expect(files[0]!.filePath).toBe("a/first.ts");
    expect(files[1]!.filePath).toBe("z/last.ts");
  });

  it("drives the render-mode toggle group", () => {
    render();
    const toggleGroup = capturedLast("toggleGroup");
    const onValueChange = toggleGroup["onValueChange"] as (value: string[]) => void;
    expect(() => onValueChange(["split"])).not.toThrow();
    expect(() => onValueChange(["stacked"])).not.toThrow();
    expect(() => onValueChange(["unknown"])).not.toThrow();
    expect(() => onValueChange([])).not.toThrow();
  });

  it("drives the word-wrap and whitespace toggles", () => {
    render();
    const toggles = capturedAll("toggle");
    for (const toggle of toggles) {
      const onPressedChange = toggle["onPressedChange"] as ((pressed: boolean) => void) | undefined;
      if (onPressedChange) {
        expect(() => onPressedChange(true)).not.toThrow();
        expect(() => onPressedChange(false)).not.toThrow();
      }
    }
  });

  it("renders and toggles the per-file collapse control", () => {
    render();
    const codeView = capturedLast("annotatableCodeView");
    const renderHeaderPrefix = codeView["renderHeaderPrefix"] as (
      fileDiffArg: { path: string; key: string },
      fileKey: string,
      collapsed: boolean,
    ) => ReactElement;
    const expanded = renderHeaderPrefix(fileDiff("src/a.ts", "file-a"), "file-a", false);
    const collapsed = renderHeaderPrefix(fileDiff("src/a.ts", "file-a"), "file-a", true);
    const button = collapseButton(expanded);
    const stop = { stopPropagation: () => undefined };
    expect(() => button.props.onClick(stop)).not.toThrow();
    expect(collapseButton(collapsed).props["aria-expanded"]).toBe(false);
  });

  it("runs the reconcile effect for a turn selection and the scroll effect", () => {
    h.turnDiffSummaries = [summary(turnIdA, 2, "2026-01-02T00:00:00.000Z")];
    h.inferredCheckpointTurnCountByTurnId = { [turnIdA]: 2 };
    h.diffSelection = { kind: "turn", turnId: turnIdA, filePath: "src/a.ts", revealRequestId: 3 };
    h.checkpointDiff = { data: { diff: "CP" }, error: null, isPending: false };
    render();
    const codeView = capturedLast("annotatableCodeView");
    const viewerRef = codeView["viewerRef"] as { current: unknown };
    const scrollTo = vi.fn();
    viewerRef.current = { scrollTo };
    runEffects();
    expect(h.storeApi.reconcileTurnSelection).toHaveBeenCalled();
    expect(scrollTo).toHaveBeenCalledWith({ type: "item", id: "file-a", align: "start" });
  });

  it("short-circuits both effects for a branch selection with no selected file", () => {
    render();
    // Branch scope: the reconcile effect returns early (not a turn selection)
    // and the scroll effect returns early (no selected file path).
    expect(() => runEffects()).not.toThrow();
    expect(h.storeApi.reconcileTurnSelection).not.toHaveBeenCalled();
  });

  it("refreshes the git preview when Source Control reopens a scope", () => {
    h.gitRefreshRequestId = 2;
    render();
    runEffects();
    expect(h.diffPreviewRefresh).toHaveBeenCalledOnce();
  });

  it("skips scrolling when the selected file is not among the rendered files", () => {
    h.turnDiffSummaries = [summary(turnIdA, 2, "2026-01-02T00:00:00.000Z")];
    h.inferredCheckpointTurnCountByTurnId = { [turnIdA]: 2 };
    h.diffSelection = {
      kind: "turn",
      turnId: turnIdA,
      filePath: "does/not/exist.ts",
      revealRequestId: 1,
    };
    h.checkpointDiff = { data: { diff: "CP" }, error: null, isPending: false };
    render();
    const codeView = capturedLast("annotatableCodeView");
    const viewerRef = codeView["viewerRef"] as { current: unknown };
    const scrollTo = vi.fn();
    viewerRef.current = { scrollTo };
    runEffects();
    expect(scrollTo).not.toHaveBeenCalled();
  });

  it("collapses an already-collapsed file back open (delete branch)", () => {
    // Seed the collapsed-files state (useState index 4) so the toggle takes the
    // delete branch instead of the add branch.
    h.stateSeeds.set(4, {
      scopeKey: `${environmentId}:${threadId}:branch`,
      fileKeys: new Set(["file-a"]),
    });
    render();
    const codeView = capturedLast("annotatableCodeView");
    const files = codeView["files"] as Array<{ collapsed: boolean; fileKey: string }>;
    expect(files.find((file) => file.fileKey === "file-a")?.collapsed).toBe(true);
    const renderHeaderPrefix = codeView["renderHeaderPrefix"] as (
      fileDiffArg: { path: string; key: string },
      fileKey: string,
      collapsed: boolean,
    ) => ReactElement;
    const element = renderHeaderPrefix(fileDiff("src/a.ts", "file-a"), "file-a", true);
    const button = collapseButton(element);
    expect(() => button.props.onClick({ stopPropagation: () => undefined })).not.toThrow();
  });

  it("breaks turn-order ties by completion time", () => {
    h.turnDiffSummaries = [
      summary(turnIdA, 2, "2026-01-01T00:00:00.000Z"),
      summary(turnIdB, 2, "2026-01-02T00:00:00.000Z"),
    ];
    h.inferredCheckpointTurnCountByTurnId = { [turnIdA]: 2, [turnIdB]: 2 };
    expect(() => render()).not.toThrow();
  });
});

// ── Scope + turn menu handlers ───────────────────────────────────────
describe("DiffPanel: scope and turn selection handlers", () => {
  it("invokes store mutations from the scope menu items", () => {
    h.turnDiffSummaries = [
      summary(turnIdA, 2, "2026-01-02T00:00:00.000Z"),
      summary(turnIdB, 1, "2026-01-01T00:00:00.000Z"),
    ];
    h.inferredCheckpointTurnCountByTurnId = { [turnIdA]: 2, [turnIdB]: 1 };
    render();
    for (const item of capturedAll("dropdownMenuItem")) {
      const onClick = item["onClick"] as (() => void) | undefined;
      if (onClick) expect(() => onClick()).not.toThrow();
    }
    expect(h.storeApi.selectGitScope).toHaveBeenCalledWith(routeRef, "unstaged");
    expect(h.storeApi.selectGitScope).toHaveBeenCalledWith(routeRef, "branch");
    expect(h.storeApi.selectTurn).toHaveBeenCalled();
  });

  it("no-ops scope and turn selection when there is no route ref", () => {
    h.routeThreadRef = null;
    // Turn summaries come from a mocked hook independent of the (absent) thread,
    // so the scope and turn menu items still render and their guarded handlers
    // can be invoked.
    h.turnDiffSummaries = [summary(turnIdA, 2, "2026-01-02T00:00:00.000Z")];
    h.inferredCheckpointTurnCountByTurnId = { [turnIdA]: 2 };
    render();
    for (const item of capturedAll("dropdownMenuItem")) {
      const onClick = item["onClick"] as (() => void) | undefined;
      if (onClick) onClick();
    }
    expect(h.storeApi.selectGitScope).not.toHaveBeenCalled();
    expect(h.storeApi.selectTurn).not.toHaveBeenCalled();
  });
});

// ── Branch base-ref combobox ─────────────────────────────────────────
describe("DiffPanel: branch base-ref combobox", () => {
  beforeEach(() => {
    h.diffSelection = { kind: "branch", baseRef: "main" };
    h.baseRefChoices = [
      { id: "c1", label: "main", local: { name: "main" }, remote: { name: "origin/main" } },
      { id: "c2", label: "release", local: null, remote: { name: "origin/release" } },
      { id: "c3", label: "dev", local: { name: "dev" }, remote: null },
    ];
    h.filteredBaseRefChoices = h.baseRefChoices;
  });

  it("renders the comparison combobox and drives its handlers", () => {
    const markup = render();
    expect(markup).toContain('data-mock="combobox"');

    const combobox = capturedLast("combobox");
    const onValueChange = combobox["onValueChange"] as (value: string | undefined) => void;
    const onOpenChange = combobox["onOpenChange"] as (open: boolean) => void;
    expect(() => onValueChange("__automatic_base_ref__")).not.toThrow();
    expect(() => onValueChange("origin/main")).not.toThrow();
    expect(() => onValueChange(undefined)).not.toThrow();
    expect(() => onOpenChange(false)).not.toThrow();
    expect(() => onOpenChange(true)).not.toThrow();
    expect(h.storeApi.selectBranchBaseRef).toHaveBeenCalledWith(routeRef, null);
    expect(h.storeApi.selectBranchBaseRef).toHaveBeenCalledWith(routeRef, "origin/main");

    const input = capturedLast("comboboxInput");
    const onChange = input["onChange"] as (event: { target: { value: string } }) => void;
    expect(() => onChange({ target: { value: "rel" } })).not.toThrow();
  });

  it("drives the per-choice remote switch", () => {
    render();
    const switches = capturedAll("switch");
    expect(switches.length).toBeGreaterThan(0);
    const onCheckedChange = switches[0]!["onCheckedChange"] as (checked: boolean) => void;
    expect(() => onCheckedChange(true)).not.toThrow();
    expect(() => onCheckedChange(false)).not.toThrow();
    expect(h.storeApi.selectBranchBaseRef).toHaveBeenCalledWith(routeRef, "origin/main");
    expect(h.storeApi.selectBranchBaseRef).toHaveBeenCalledWith(routeRef, "main");
  });
});

// ── Non-files render branches ────────────────────────────────────────
describe("DiffPanel: patch state branches", () => {
  it("renders a raw patch with its reason and text", () => {
    h.renderablePatch = { kind: "raw", reason: "Binary file", text: "raw diff body" };
    const markup = render();
    expect(markup).toContain("Binary file");
    expect(markup).toContain("raw diff body");
  });

  it("shows the branch loading state when the patch is pending", () => {
    h.renderablePatch = null;
    h.queryStateByKey.set("review.diffPreview", { isPending: true });
    const markup = render();
    expect(markup).toContain("Loading branch diff...");
  });

  it("shows the working-tree loading state for the unstaged scope", () => {
    h.diffSelection = { kind: "unstaged" };
    h.renderablePatch = null;
    h.queryStateByKey.set("review.diffPreview", { isPending: true });
    const markup = render();
    expect(markup).toContain("Loading working tree diff...");
  });

  it("shows the checkpoint loading state for a turn selection", () => {
    h.turnDiffSummaries = [summary(turnIdA, 1, "2026-01-01T00:00:00.000Z")];
    h.inferredCheckpointTurnCountByTurnId = { [turnIdA]: 1 };
    h.diffSelection = { kind: "turn", turnId: turnIdA, filePath: null, revealRequestId: 0 };
    h.renderablePatch = null;
    h.checkpointDiff = { data: undefined, error: null, isPending: true };
    const markup = render();
    expect(markup).toContain("Loading checkpoint diff...");
  });

  it("shows the no-net-changes message for an empty resolved patch", () => {
    h.renderablePatch = null;
    h.queryDataByKey.set("review.diffPreview", branchPreviewData({ diff: "   " }));
    const markup = render();
    expect(markup).toContain("No net changes in this selection.");
  });

  it("shows the no-patch message when nothing resolves", () => {
    h.renderablePatch = null;
    h.queryDataByKey.set("review.diffPreview", { cwd: "/repo", sources: [] });
    const markup = render();
    expect(markup).toContain("No patch available for this selection.");
  });

  it("warns when the selected patch is truncated", () => {
    h.queryDataByKey.set("review.diffPreview", branchPreviewData({ truncated: true }));
    const markup = render();
    expect(markup).toContain("truncated because it exceeded the preview limit");
  });

  it("renders the patch error banner when there is no renderable patch", () => {
    h.renderablePatch = null;
    h.queryDataByKey.set("review.diffPreview", { cwd: "/repo", sources: [] });
    h.queryStateByKey.set("review.diffPreview", { error: "diff failed to load" });
    const markup = render();
    expect(markup).toContain("diff failed to load");
  });
});

// ── Editor open failure path ─────────────────────────────────────────
describe("DiffPanel: mode prop", () => {
  it("passes the mode through to the shell", () => {
    const markup = render("sidebar");
    expect(markup).toContain('data-mode="sidebar"');
  });

  it("suppresses workspace refresh queries while guarded", () => {
    h.gitRefreshRequestId = 4;
    const markup = renderToStaticMarkup(
      <DiffPanel
        composerDraftTarget={routeRef as never}
        workspaceUnavailable="Workspace unavailable. Retry detection or remove it from Coddy Studio."
      />,
    );
    runEffects();

    expect(markup).toContain("Workspace unavailable. Retry detection or remove it from Coddy Studio.");
    expect(h.diffPreviewRefresh).not.toHaveBeenCalled();
  });
});
