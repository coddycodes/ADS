import { DEFAULT_SERVER_SETTINGS, EnvironmentId, ThreadId } from "@bibcode/contracts";
import type { EditorId, ProjectEntry, VcsStatusResult } from "@bibcode/contracts";
import { FileTree as FileTreeModel } from "@pierre/trees";
import { renderToStaticMarkup } from "react-dom/server";
import * as React from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { textSizesBelowTextXs } from "~/test/uiTypography";

/**
 * FileBrowserPanel is rendered with `renderToStaticMarkup`. React's stateful
 * hooks are partially mocked: `useState` can be seeded and its setter calls are
 * recorded (so dialog requests set via `setDialogRequest` can be recovered),
 * and `useEffect` bodies are captured so the path-reset effect can be run. The
 * file-tree, dialog, and context-menu children are capture-mocked, letting the
 * tests reach the row/background action handlers and invoke the async command
 * flows directly.
 */
const harness = vi.hoisted(() => {
  type Matcher = (initial: unknown) => boolean;
  const state = {
    stateSeeds: [] as Array<{ match: Matcher; value: unknown }>,
    setStateCalls: [] as Array<{ initial: unknown; next: unknown; applied: unknown }>,
    effects: [] as Array<() => void | (() => void)>,
    refs: [] as Array<{ current: unknown }>,
    refIndex: 0,
    persistRefs: false,
    reset() {
      state.stateSeeds.length = 0;
      state.setStateCalls.length = 0;
      state.effects.length = 0;
      state.refs.length = 0;
      state.refIndex = 0;
      state.persistRefs = false;
    },
    seedState(match: Matcher, value: unknown) {
      state.stateSeeds.push({ match, value });
    },
    runEffects(): Array<() => void> {
      const cleanups: Array<() => void> = [];
      for (const effect of state.effects) {
        const cleanup = effect();
        if (typeof cleanup === "function") cleanups.push(cleanup);
      }
      return cleanups;
    },
  };
  return state;
});

const ui = vi.hoisted(() => {
  const registry = {
    entries: [] as Array<{ kind: string; props: Record<string, unknown> }>,
    reset() {
      registry.entries.length = 0;
    },
    record(kind: string, props: unknown) {
      if (props && typeof props === "object") {
        registry.entries.push({ kind, props: props as Record<string, unknown> });
      }
    },
    filter(kind: string) {
      return registry.entries.filter((entry) => entry.kind === kind).map((entry) => entry.props);
    },
    last(kind: string) {
      const matches = registry.filter(kind);
      return matches[matches.length - 1];
    },
  };
  return registry;
});

const testState = vi.hoisted(() => ({
  entriesQuery: {
    data: null as { entries: ReadonlyArray<unknown>; truncated?: boolean } | null,
    error: null as string | null,
    isPending: false,
    refresh: (() => {}) as () => void,
  } as {
    data: { entries: ReadonlyArray<unknown>; truncated?: boolean } | null;
    error: string | null;
    isPending: boolean;
    refresh: () => void;
  },
  primaryEnvironmentId: null as string | null,
  environmentHttpBaseUrl: null as string | null,
  environment: null as Record<string, unknown> | null,
  connectionPhase: "connected" as string | null,
  preferredEditor: null as string | null,
  isPreviewSupported: true,
  isBrowserPreviewFile: (() => false) as (path: string) => boolean,
  isMarkdownPreviewFile: (() => false) as (path: string) => boolean,
  resolvedTheme: "dark" as "dark" | "light",
  remapFileSurfaces: (() => {}) as (...args: unknown[]) => void,
  closeFileSurfacesUnder: (() => {}) as (...args: unknown[]) => void,
  vcsStatus: null as VcsStatusResult | null,
  entryChangeSignal: null as { cwd: string } | null,
  openFileInPreview: (async () => ({ _tag: "Success" })) as (input: unknown) => Promise<{
    _tag: string;
    error?: unknown;
  }>,
  commandCalls: [] as Array<{ label: string; input: unknown }>,
  commandResults: {} as Record<string, unknown>,
  toastAdd: (() => {}) as (toast: unknown) => void,
  fileTree: {
    resetPaths: (() => {}) as (paths: readonly string[], options?: Record<string, unknown>) => void,
    setGitStatus: (() => {}) as (entries: readonly unknown[]) => void,
    openSearch: (() => {}) as () => void,
    getItem: (() => null) as (path: string) => unknown,
    options: null as Record<string, unknown> | null,
  },
  newProjectId: "generated-project-id",
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  const resolveInitial = (initial: unknown): unknown =>
    typeof initial === "function" ? (initial as () => unknown)() : initial;
  const useState = (initial?: unknown) => {
    const resolved = resolveInitial(initial);
    const seedIndex = harness.stateSeeds.findIndex((seed) => seed.match(resolved));
    const value = seedIndex >= 0 ? harness.stateSeeds.splice(seedIndex, 1)[0]!.value : resolved;
    const setValue = (next: unknown) => {
      const applied =
        typeof next === "function" ? (next as (value: unknown) => unknown)(value) : next;
      harness.setStateCalls.push({ initial: resolved, next, applied });
    };
    return [value, setValue];
  };
  const useEffect = (effect: () => void | (() => void)) => {
    harness.effects.push(effect);
  };
  const useRef = (initial: unknown) => {
    if (!harness.persistRefs) return actual.useRef(initial);
    const index = harness.refIndex++;
    const existing = harness.refs[index];
    if (existing) return existing;
    const created = { current: initial };
    harness.refs[index] = created;
    return created;
  };
  return {
    ...actual,
    useState: useState as typeof actual.useState,
    useEffect: useEffect as typeof actual.useEffect,
    useRef: useRef as typeof actual.useRef,
  };
});

vi.mock("@pierre/trees/react", () => ({
  useFileTree: (options: Record<string, unknown>) => {
    testState.fileTree.options = options;
    return {
      model: {
        resetPaths: (paths: readonly string[], resetOptions?: Record<string, unknown>) =>
          testState.fileTree.resetPaths(paths, resetOptions),
        setGitStatus: (entries: readonly unknown[]) => testState.fileTree.setGitStatus(entries),
        openSearch: () => testState.fileTree.openSearch(),
        getItem: (path: string) => testState.fileTree.getItem(path),
      },
    };
  },
  FileTree: (props: Record<string, unknown>) => {
    ui.record("FileTree", props);
    return <div data-file-tree />;
  },
}));

vi.mock("@bibcode/client-runtime/state/runtime", () => ({
  // Mutation commands surface interruption as a `Failure` carrying an interrupt
  // marker; the preview flow uses a dedicated `Interrupted` tag. Support both.
  isAtomCommandInterrupted: (result: { _tag: string; interrupted?: boolean }) =>
    result._tag === "Interrupted" || result.interrupted === true,
  squashAtomCommandFailure: (result: { error?: unknown }) => result.error,
}));

vi.mock("~/browser/openFileInPreview", () => ({
  isBrowserPreviewFile: (path: string) => testState.isBrowserPreviewFile(path),
  openFileInPreview: (input: unknown) => testState.openFileInPreview(input),
}));

vi.mock("~/editorPreferences", () => ({
  usePreferredEditor: () => [testState.preferredEditor],
}));

vi.mock("~/hooks/useTheme", () => ({
  useTheme: () => ({ resolvedTheme: testState.resolvedTheme }),
}));

vi.mock("~/lib/utils", () => ({
  cn: (...args: unknown[]) => args.filter(Boolean).join(" "),
  newProjectId: () => testState.newProjectId,
}));

vi.mock("~/pierre-icons", () => ({ BIBCODE_PIERRE_ICONS: {} }));

vi.mock("~/previewStateStore", () => ({
  isPreviewSupportedInRuntime: () => testState.isPreviewSupported,
  canPreviewAnyUrlInRuntime: () => testState.isPreviewSupported,
}));

vi.mock("~/rightPanelStore", () => ({
  useRightPanelStore: (
    selector: (state: {
      remapFileSurfaces: (...args: unknown[]) => void;
      closeFileSurfacesUnder: (...args: unknown[]) => void;
    }) => unknown,
  ) =>
    selector({
      remapFileSurfaces: (...args: unknown[]) => testState.remapFileSurfaces(...args),
      closeFileSurfacesUnder: (...args: unknown[]) => testState.closeFileSurfacesUnder(...args),
    }),
}));

vi.mock("~/state/assets", () => ({
  assetEnvironment: { createUrl: { label: "createUrl" } },
}));

vi.mock("~/state/environments", () => ({
  useEnvironment: () => testState.environment,
  usePrimaryEnvironmentId: () => testState.primaryEnvironmentId,
  useEnvironmentHttpBaseUrl: () => testState.environmentHttpBaseUrl,
  useEnvironmentConnectionState: () => ({
    data: testState.connectionPhase === null ? null : { phase: testState.connectionPhase },
  }),
}));

vi.mock("~/state/preview", () => ({
  previewEnvironment: { open: { label: "openPreview" } },
}));

// Two subscriptions run in the panel: vcs status and the out-of-band entry-change signal. They are
// told apart by the atom each returns so a test can move one without disturbing the other.
vi.mock("~/state/query", () => ({
  useEnvironmentQuery: (atom: { kind?: string } | null) =>
    atom?.kind === "entry-changes"
      ? { data: testState.entryChangeSignal }
      : { data: testState.vcsStatus },
}));

vi.mock("~/state/projects", () => ({
  projectEnvironment: {
    create: { label: "create" },
    createEntry: { label: "createEntry" },
    createDownloadUrl: { label: "createDownloadUrl" },
    createUploadUrl: { label: "createUploadUrl" },
    refreshEntries: { label: "refreshEntries" },
    subscribeEntries: () => ({ kind: "entry-changes" }),
    renameEntry: { label: "renameEntry" },
    deleteEntry: { label: "deleteEntry" },
    duplicateEntry: { label: "duplicateEntry" },
  },
}));

vi.mock("~/state/shell", () => ({
  shellEnvironment: { openInEditor: { label: "openInEditor" } },
}));

vi.mock("~/state/vcs", () => ({
  vcsEnvironment: { status: () => ({ kind: "vcs-status" }) },
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: (command: { label?: string }) => (input: unknown) => {
    const label = command?.label ?? "unknown";
    testState.commandCalls.push({ label, input });
    const result = testState.commandResults[label] ?? { _tag: "Success", value: {} };
    return Promise.resolve(result);
  },
}));

vi.mock("~/state/use-atom-query-runner", () => ({
  useAtomQueryRunner: () => (input: unknown) => {
    testState.commandCalls.push({ label: "createAssetUrl", input });
    return Promise.resolve({ _tag: "Success", value: {} });
  },
}));

vi.mock("../ui/toast", () => ({
  stackedThreadToast: (options: Record<string, unknown>) => ({ stacked: true, ...options }),
  toastManager: { add: (toast: unknown) => testState.toastAdd(toast) },
}));

vi.mock("./FileEntryDialog", () => ({
  default: (props: Record<string, unknown>) => {
    ui.record("FileEntryDialog", props);
    return <div data-file-entry-dialog />;
  },
}));

vi.mock("./filePreviewMode", () => ({
  isMarkdownPreviewFile: (path: string) => testState.isMarkdownPreviewFile(path),
}));

vi.mock("./FileTreeContextMenu", () => ({
  default: (props: Record<string, unknown>) => {
    ui.record("FileTreeContextMenu", props);
    return <div data-file-tree-context-menu />;
  },
}));

vi.mock("../ui/button", () => ({
  Button: (props: { children?: unknown; onClick?: () => void }) => {
    ui.record("Button", props);
    return <button type="button">{props.children as never}</button>;
  },
}));

vi.mock("./projectFilesQueryState", () => {
  // Automatic re-reads measure through the spy each test installs as `refresh`; one stable
  // function keeps the entry-signal effect's dependency unchanged across renders.
  const revalidate = () => testState.entriesQuery.refresh();
  return {
    useProjectEntriesQuery: () => ({ ...testState.entriesQuery, revalidate }),
  };
});

import type { FileTreeMenuActions } from "./FileTreeContextMenu";
import FileBrowserPanel, {
  collapseDirectoryTreePaths,
  currentlyExpandedTreePaths,
  expandedDirectoryTreePaths,
} from "./FileBrowserPanel";
import { FileEditingSessionRegistry } from "./fileEditingSessionRegistry";

/** Row menus receive every handler defined, so treat them as non-optional. */
type RowActions = { [K in keyof FileTreeMenuActions]-?: NonNullable<FileTreeMenuActions[K]> };

const environmentId = EnvironmentId.make("environment-1");
const otherEnvironmentId = EnvironmentId.make("environment-2");
const threadRef = { environmentId, threadId: ThreadId.make("thread-1") };

type PanelProps = Parameters<typeof FileBrowserPanel>[0];

function entry(path: string, kind: ProjectEntry["kind"]): ProjectEntry {
  return { path, kind } as ProjectEntry;
}

function vcsStatus(files: VcsStatusResult["workingTree"]["files"]): VcsStatusResult {
  return {
    isRepo: true,
    hasPrimaryRemote: false,
    isDefaultRef: true,
    refName: "main",
    hasWorkingTreeChanges: files.length > 0,
    workingTree: { files, insertions: 0, deletions: 0 },
    hasUpstream: false,
    aheadCount: 0,
    behindCount: 0,
    pr: null,
  };
}

function setEntries(
  entries: ReadonlyArray<ProjectEntry>,
  options: { truncated?: boolean; isPending?: boolean; error?: string | null } = {},
) {
  testState.entriesQuery = {
    data: { entries, truncated: options.truncated ?? false },
    error: options.error ?? null,
    isPending: options.isPending ?? false,
    refresh: vi.fn(),
  };
}

function baseProps(overrides: Partial<PanelProps> = {}): PanelProps {
  return {
    environmentId,
    cwd: "/workspace/demo",
    projectName: "demo",
    threadRef,
    availableEditors: [] as ReadonlyArray<EditorId>,
    onOpenFile: vi.fn(),
    onBeginPathMutation: vi.fn(async () => mutationLease()),
    ...overrides,
  };
}

function renderPanel(props: PanelProps = baseProps()): string {
  ui.reset();
  harness.setStateCalls.length = 0;
  harness.effects.length = 0;
  harness.refIndex = 0;
  return renderToStaticMarkup(<FileBrowserPanel {...props} />);
}

/** Enough microtask turns for a mutation to settle and its refresh (rebuild + query) to run. */
async function flushPromises(): Promise<void> {
  for (let turn = 0; turn < 6; turn += 1) await Promise.resolve();
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function mutationLease() {
  return {
    commitRename: vi.fn(),
    commitDelete: vi.fn(),
    release: vi.fn(),
  };
}

function editingSession(relativePath: string) {
  return {
    relativePath,
    flush: vi.fn(async () => "saved" as const),
    settle: vi.fn(async () => "saved" as const),
    setAutosaveEnabled: vi.fn(),
    pauseSaving: vi.fn(),
    resumeSaving: vi.fn(),
    discardPendingSave: vi.fn(),
    rename: vi.fn(),
    dispose: vi.fn(),
  };
}

/** Recover a dialog request pushed through `setDialogRequest`. */
function lastDialogRequest(): Record<string, unknown> {
  const call = [...harness.setStateCalls]
    .toReversed()
    .find(
      (entry) =>
        entry.applied !== null &&
        typeof entry.applied === "object" &&
        ("onSubmit" in (entry.applied as object) || "onConfirm" in (entry.applied as object)),
    );
  if (!call) throw new Error("No dialog request was set");
  return call.applied as Record<string, unknown>;
}

/** Invoke `renderContextMenu` and return the menu element rendered for a row. */
function rowMenuFor(path: string, kind: ProjectEntry["kind"]): React.ReactElement {
  const fileTree = ui.last("FileTree")!;
  const renderContextMenu = fileTree["renderContextMenu"] as (
    item: { path: string; kind: string },
    context: { anchorElement: unknown; close: () => void },
  ) => React.ReactElement;
  const treePath = kind === "directory" ? `${path}/` : path;
  return renderContextMenu(
    { path: treePath, kind },
    { anchorElement: { id: "anchor" }, close: vi.fn() },
  );
}

/** Invoke `renderContextMenu` and return the row `actions` handed to the menu. */
function rowActionsFor(path: string, kind: ProjectEntry["kind"]): RowActions {
  return (rowMenuFor(path, kind).props as { actions: RowActions }).actions;
}

/** Invoke `renderContextMenu` and return the menu `model` handed to the menu. */
function rowModelFor(
  path: string,
  kind: ProjectEntry["kind"],
): { groups: Array<Array<{ id: string; enabled: boolean }>> } {
  return (
    rowMenuFor(path, kind).props as {
      model: { groups: Array<Array<{ id: string; enabled: boolean }>> };
    }
  ).model;
}

/** A desktop bridge exposing only the transfer commands a test cares about. */
function stubDesktopBridge(bridge: Record<string, unknown>): void {
  vi.stubGlobal("window", { desktopBridge: bridge });
}

beforeEach(() => {
  harness.reset();
  ui.reset();
  testState.entriesQuery = { data: null, error: null, isPending: false, refresh: vi.fn() };
  testState.primaryEnvironmentId = environmentId;
  testState.environmentHttpBaseUrl = null;
  testState.environment = null;
  testState.connectionPhase = "connected";
  testState.preferredEditor = null;
  testState.isPreviewSupported = true;
  testState.isBrowserPreviewFile = vi.fn(() => false);
  testState.isMarkdownPreviewFile = vi.fn(() => false);
  testState.resolvedTheme = "dark";
  testState.remapFileSurfaces = vi.fn();
  testState.closeFileSurfacesUnder = vi.fn();
  testState.vcsStatus = null;
  testState.openFileInPreview = vi.fn(async () => ({ _tag: "Success" }));
  testState.commandCalls = [];
  testState.entryChangeSignal = null;
  testState.commandResults = {};
  testState.toastAdd = vi.fn();
  testState.fileTree.resetPaths = vi.fn();
  testState.fileTree.setGitStatus = vi.fn();
  testState.fileTree.openSearch = vi.fn();
  testState.fileTree.getItem = vi.fn(() => null);
  testState.fileTree.options = null;

  vi.stubGlobal("navigator", {
    clipboard: { writeText: vi.fn(async () => {}) },
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("header rendering", () => {
  it("shows the indexing state before the first result", () => {
    testState.entriesQuery = { data: null, error: null, isPending: true, refresh: vi.fn() };
    const markup = renderPanel();
    expect(markup).toContain("Indexing…");
    expect(markup).toContain("animate-spin");
    expect(markup).toContain(">demo</div>");
  });

  it("shows no file count while the list has failed", () => {
    testState.entriesQuery = {
      data: null,
      error: "Workspace query failed.",
      isPending: false,
      refresh: vi.fn(),
    };
    const markup = renderPanel();
    expect(markup).toContain("Workspace query failed.");
    expect(markup).not.toMatch(/\d files/);
    expect(markup).not.toContain("Indexing…");
  });

  it("shows no file count before any list has loaded", () => {
    testState.entriesQuery = { data: null, error: null, isPending: false, refresh: vi.fn() };
    expect(renderPanel()).not.toMatch(/\d files/);
  });

  it("shows Indexing… and no file count while a failed list is read again", () => {
    testState.entriesQuery = {
      data: null,
      error: "The connection dropped before the result arrived.",
      isPending: true,
      refresh: vi.fn(),
    };
    const markup = renderPanel();
    expect(markup).toContain("Indexing…");
    expect(markup).not.toMatch(/\d files/);
  });

  it("shows the file count and the partial suffix when truncated", () => {
    setEntries([entry("a.ts", "file"), entry("b.ts", "file"), entry("dir", "directory")], {
      truncated: true,
    });
    const markup = renderPanel();
    expect(markup).toContain("2 files");
    expect(markup).toContain("· partial");
    expect(markup).toContain('data-file-browser-panel="environment-1:/workspace/demo"');
    expect(markup).toContain("Collapse all folders");
    expect(markup).toContain("Expand all folders");
  });

  it("sets the header text at 12 px or larger (UI.md typography)", () => {
    setEntries([entry("a.ts", "file")], { truncated: true });
    const markup = renderPanel();
    expect(markup).toContain("1 files · partial");
    expect(textSizesBelowTextXs(markup)).toEqual([]);
    // UI.md: the secondary color is solid; text never alpha-reduces it.
    expect(markup).not.toMatch(/text-muted-foreground\/\d/);
  });

  it("shows and disables a user-initiated rescan while it is pending", () => {
    setEntries([entry("a.ts", "file")]);
    harness.seedState((initial) => initial === false, true);

    const markup = renderPanel();

    expect(markup).toContain("Refreshing…");
    expect(markup).toContain('aria-busy="true"');
    expect(markup).toContain('aria-label="Refresh workspace files"');
    expect(markup).toContain('disabled=""');
    expect(markup).toContain("animate-spin");
  });

  it("renders the error surface instead of the tree when the query fails", () => {
    testState.entriesQuery = {
      data: null,
      error: "Workspace query failed.",
      isPending: false,
      refresh: vi.fn(),
    };
    const markup = renderPanel();
    expect(markup).toContain("Workspace query failed.");
    expect(markup).toContain("text-destructive");
    expect(ui.filter("FileTree")).toHaveLength(0);
  });

  it("offers Retry for a cut-off list", () => {
    const refresh = vi.fn();
    testState.entriesQuery = {
      data: null,
      error: "The connection dropped before the result arrived.",
      isPending: false,
      refresh,
    };
    const markup = renderPanel();
    expect(markup).toContain("The connection dropped before the result arrived.");
    const retry = ui.filter("Button").find((props) => props.children === "Retry");
    if (retry === undefined) throw new Error("Retry was not rendered.");
    (retry.onClick as () => void)();
    expect(refresh).toHaveBeenCalledOnce();
  });

  it("offers Retry for any failed list, not only a cut-off", () => {
    const refresh = vi.fn();
    testState.entriesQuery = {
      data: null,
      error: "Workspace query failed.",
      isPending: false,
      refresh,
    };
    renderPanel();
    const retry = ui.filter("Button").find((props) => props.children === "Retry");
    if (retry === undefined) throw new Error("Retry was not rendered.");
    expect(retry["aria-disabled"]).toBeUndefined();
    (retry.onClick as () => void)();
    expect(refresh).toHaveBeenCalledOnce();
  });

  it("keeps the failure on screen and disables Retry while the list is read again", () => {
    testState.entriesQuery = {
      data: null,
      error: "The connection dropped before the result arrived.",
      isPending: true,
      refresh: vi.fn(),
    };
    const markup = renderPanel();
    expect(markup).toContain("The connection dropped before the result arrived.");
    const retry = ui.filter("Button").find((props) => props.children === "Retrying…");
    if (retry === undefined) throw new Error("Retrying… was not rendered.");
    expect(retry["aria-disabled"]).toBe(true);
  });

  it("waits for the connection instead of offering Retry while the environment is disconnected", () => {
    const refresh = vi.fn();
    testState.connectionPhase = "backoff";
    testState.entriesQuery = {
      data: null,
      error: "The connection dropped before the result arrived.",
      isPending: false,
      refresh,
    };
    renderPanel();
    const waiting = ui
      .filter("Button")
      .find((props) => props.children === "Waiting for the connection…");
    if (waiting === undefined) throw new Error("Waiting for the connection… was not rendered.");
    expect(waiting["aria-disabled"]).toBe(true);
    expect(ui.filter("Button").some((props) => props.children === "Retry")).toBe(false);
  });

  it("wires the search and refresh buttons", () => {
    setEntries([entry("a.ts", "file")]);
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    renderPanel();

    const search = ui.filter("FileTree").length; // ensure tree rendered
    expect(search).toBe(1);

    // The header search/refresh buttons live in the static markup; invoke the
    // model + query handlers they call.
    testState.fileTree.openSearch();
    expect(testState.fileTree.openSearch).toHaveBeenCalled();
  });
});

describe("expandedDirectoryTreePaths", () => {
  it("returns every directory path in file-tree format for expand all", () => {
    expect(
      expandedDirectoryTreePaths([
        entry("src", "directory"),
        entry("src/components", "directory"),
        entry("src/components/App.tsx", "file"),
        entry("README.md", "file"),
      ]),
    ).toEqual(["src/", "src/components/"]);
  });
});

describe("collapseDirectoryTreePaths", () => {
  it("collapses each directory through the tree item handle", () => {
    const collapseRoot = vi.fn();
    const collapseNested = vi.fn();
    const model = {
      getItem: vi.fn((path: string) => {
        if (path === "src/") {
          return {
            collapse: collapseRoot,
            isDirectory: () => true as const,
          };
        }
        if (path === "src/components/") {
          return {
            collapse: collapseNested,
            isDirectory: () => true as const,
          };
        }
        return null;
      }),
    };

    collapseDirectoryTreePaths(model, ["src/", "src/components/"]);

    expect(model.getItem).toHaveBeenCalledWith("src/");
    expect(model.getItem).toHaveBeenCalledWith("src/components/");
    expect(collapseRoot).toHaveBeenCalledTimes(1);
    expect(collapseNested).toHaveBeenCalledTimes(1);
  });

  it("ignores missing or non-directory items", () => {
    const collapseFile = vi.fn();
    const model = {
      getItem: vi.fn((path: string) =>
        path === "README.md"
          ? {
              collapse: collapseFile,
              isDirectory: () => false as const,
            }
          : null,
      ),
    };

    collapseDirectoryTreePaths(model, ["README.md", "missing/"]);

    expect(collapseFile).not.toHaveBeenCalled();
  });
});

describe("currentlyExpandedTreePaths", () => {
  it("keeps only the directories the model reports as expanded", () => {
    const expansion: Record<string, boolean> = { "src/": true, "docs/": false };
    const model = {
      getItem: vi.fn(
        (path: string): { isDirectory: () => boolean; isExpanded?: () => boolean } | null => {
          if (path in expansion) {
            return { isDirectory: () => true, isExpanded: () => expansion[path]! };
          }
          if (path === "README.md") return { isDirectory: () => false };
          return null;
        },
      ),
    };

    expect(currentlyExpandedTreePaths(model, ["src/", "docs/", "README.md", "gone/"])).toEqual([
      "src/",
    ]);
  });

  it("preserves expansion when a refresh replaces a file with nested directories", () => {
    const model = new FileTreeModel({
      paths: ["changed", "other/child/", "expanded/readme.md"],
      flattenEmptyDirectories: false,
      initialExpansion: 0,
      initialExpandedPaths: ["expanded/"],
    });
    const nextEntries = [
      entry("changed", "directory"),
      entry("changed/child", "directory"),
      entry("changed/child/new.txt", "file"),
      entry("other/child", "directory"),
      entry("expanded", "directory"),
      entry("expanded/readme.md", "file"),
    ];

    // The real refresh reads incoming directory paths against the previous tree before resetting.
    const initialExpandedPaths = currentlyExpandedTreePaths(
      model,
      expandedDirectoryTreePaths(nextEntries),
    );
    expect(initialExpandedPaths).toEqual(["expanded/"]);
    model.resetPaths(["changed/child/new.txt", "other/child/", "expanded/readme.md"], {
      initialExpandedPaths,
    });
    expect(model.getItem("changed/child/")?.isDirectory()).toBe(true);
    expect(model.getVisibleRows(0, model.getVisibleCount() - 1).map((row) => row.path)).toEqual([
      "changed/",
      "expanded/",
      "expanded/readme.md",
      "other/",
    ]);
  });

  it("drops a replaced directory's selected descendant while preserving surviving selection", () => {
    const model = new FileTreeModel({
      paths: ["changed/child.txt", "unchanged/child.txt"],
      flattenEmptyDirectories: false,
      initialExpansion: "open",
    });
    model.getItem("unchanged/child.txt")?.select();
    model.getItem("changed/child.txt")?.select();
    model.getItem("changed/child.txt")?.focus();

    model.resetPaths(["changed", "unchanged/child.txt"], {
      initialExpandedPaths: ["unchanged/"],
    });

    expect(model.getItem("changed/child.txt")).toBeNull();
    expect(model.getItem("changed")?.isDirectory()).toBe(false);
    expect(model.getSelectedPaths()).toEqual(["unchanged/child.txt"]);
    expect(model.getFocusedPath()).toBe("unchanged/");
    expect(model.getVisibleRows(0, model.getVisibleCount() - 1).map((row) => row.path)).toEqual([
      "unchanged/",
      "unchanged/child.txt",
      "changed",
    ]);
  });
});

describe("path reset effect", () => {
  it("starts with every folder collapsed", () => {
    setEntries([entry("src", "directory"), entry("src/app.ts", "file")]);
    renderPanel();
    expect(testState.fileTree.options?.["initialExpansion"]).toBe(0);
  });

  it("renders nested folder rows instead of flattened single-child chains", () => {
    setEntries([entry("src", "directory"), entry("src/app.ts", "file")]);
    renderPanel();
    expect(testState.fileTree.options?.["flattenEmptyDirectories"]).toBe(false);
  });

  it("resets the tree paths to the current entries", () => {
    setEntries([entry("src", "directory"), entry("src/app.ts", "file")]);
    renderPanel();
    harness.runEffects();
    expect(testState.fileTree.resetPaths).toHaveBeenCalledWith(["src/", "src/app.ts"], {
      initialExpandedPaths: [],
    });
  });

  it("keeps the expanded folders expanded across a refresh", () => {
    setEntries([
      entry("src", "directory"),
      entry("src/components", "directory"),
      entry("docs", "directory"),
      entry("src/app.ts", "file"),
    ]);
    testState.fileTree.getItem = vi.fn((path: string) => {
      if (path === "src/") return { isDirectory: () => true as const, isExpanded: () => true };
      if (path === "docs/") return { isDirectory: () => true as const, isExpanded: () => true };
      if (path === "src/components/") {
        return { isDirectory: () => true as const, isExpanded: () => false };
      }
      return null;
    });

    renderPanel();
    harness.runEffects();

    expect(testState.fileTree.resetPaths).toHaveBeenCalledWith(
      ["src/", "src/components/", "docs/", "src/app.ts"],
      { initialExpandedPaths: ["src/", "docs/"] },
    );
  });
});

describe("drag and drop", () => {
  interface DropTarget {
    directoryPath: string | null;
    flattenedSegmentPath: string | null;
    hoveredPath: string | null;
    kind: "directory" | "root";
  }

  function dragAndDrop() {
    return testState.fileTree.options!["dragAndDrop"] as {
      canDrag: (paths: readonly string[]) => boolean;
      onDropComplete: (event: {
        draggedPaths: readonly string[];
        operation: "batch" | "move";
        target: DropTarget;
      }) => void;
      onDropError: (error: string) => void;
    };
  }

  function directoryTarget(directoryPath: string): DropTarget {
    return {
      directoryPath,
      flattenedSegmentPath: null,
      hoveredPath: directoryPath,
      kind: "directory",
    };
  }

  const rootTarget: DropTarget = {
    directoryPath: null,
    flattenedSegmentPath: null,
    hoveredPath: null,
    kind: "root",
  };

  it("moves a dropped file into the target folder under a mutation lease", async () => {
    setEntries([entry("docs", "directory"), entry("src/app.ts", "file")]);
    testState.commandResults["renameEntry"] = {
      _tag: "Success",
      value: { relativePath: "docs/app.ts" },
    };
    const lease = mutationLease();
    const onBeginPathMutation = vi.fn(async () => lease);
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    renderPanel(baseProps({ onBeginPathMutation }));

    dragAndDrop().onDropComplete({
      draggedPaths: ["src/app.ts"],
      operation: "move",
      target: directoryTarget("docs/"),
    });
    await flushPromises();

    expect(onBeginPathMutation).toHaveBeenCalledWith({
      kind: "rename",
      fromRelativePath: "src/app.ts",
      toRelativePath: "docs/app.ts",
    });
    expect(testState.commandCalls.find((call) => call.label === "renameEntry")?.input).toEqual({
      environmentId,
      input: {
        cwd: "/workspace/demo",
        fromRelativePath: "src/app.ts",
        toRelativePath: "docs/app.ts",
      },
    });
    expect(lease.commitRename).toHaveBeenCalledWith("docs/app.ts");
    expect(testState.remapFileSurfaces).toHaveBeenCalledWith(
      threadRef,
      "src/app.ts",
      "docs/app.ts",
    );
    expect(lease.release).toHaveBeenCalledOnce();
    expect(refresh).toHaveBeenCalled();
  });

  it("moves a dropped folder to the workspace root", async () => {
    setEntries([entry("src/nested", "directory")]);
    testState.commandResults["renameEntry"] = {
      _tag: "Success",
      value: { relativePath: "nested" },
    };
    renderPanel();

    dragAndDrop().onDropComplete({
      draggedPaths: ["src/nested/"],
      operation: "move",
      target: rootTarget,
    });
    await flushPromises();

    expect(testState.commandCalls.find((call) => call.label === "renameEntry")?.input).toEqual({
      environmentId,
      input: { cwd: "/workspace/demo", fromRelativePath: "src/nested", toRelativePath: "nested" },
    });
  });

  it("moves every dragged path but skips entries already in the target folder", async () => {
    setEntries([entry("docs", "directory"), entry("docs/a.ts", "file"), entry("src/b.ts", "file")]);
    testState.commandResults["renameEntry"] = {
      _tag: "Success",
      value: { relativePath: "docs/b.ts" },
    };
    renderPanel();

    dragAndDrop().onDropComplete({
      draggedPaths: ["docs/a.ts", "src/b.ts"],
      operation: "batch",
      target: directoryTarget("docs/"),
    });
    await flushPromises();

    const renames = testState.commandCalls.filter((call) => call.label === "renameEntry");
    expect(renames).toHaveLength(1);
    expect((renames[0]!.input as { input: { toRelativePath: string } }).input.toRelativePath).toBe(
      "docs/b.ts",
    );
  });

  it("resyncs the tree when the server rejects the move", async () => {
    setEntries([entry("docs", "directory"), entry("src/app.ts", "file")]);
    testState.commandResults["renameEntry"] = { _tag: "Failure", error: new Error("busy") };
    const lease = mutationLease();
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    renderPanel(baseProps({ onBeginPathMutation: vi.fn(async () => lease) }));

    dragAndDrop().onDropComplete({
      draggedPaths: ["src/app.ts"],
      operation: "move",
      target: directoryTarget("docs/"),
    });
    await flushPromises();

    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: 'Failed to move "app.ts"', description: "busy" }),
    );
    expect(lease.commitRename).not.toHaveBeenCalled();
    expect(lease.release).toHaveBeenCalledOnce();
    expect(refresh).toHaveBeenCalled();
  });

  it("resyncs the tree when Pierre rejects its own optimistic move", async () => {
    setEntries([entry("docs", "directory"), entry("src/app.ts", "file")]);
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    renderPanel();

    dragAndDrop().onDropError("docs/app.ts already exists");
    await flushPromises();

    expect(testState.commandCalls.some((call) => call.label === "renameEntry")).toBe(false);
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Failed to move files",
        description: "docs/app.ts already exists",
      }),
    );
    expect(refresh).toHaveBeenCalled();
  });

  it("blocks dragging while the workspace is unavailable", () => {
    setEntries([entry("src/app.ts", "file")]);
    renderPanel();
    expect(dragAndDrop().canDrag(["src/app.ts"])).toBe(true);

    renderPanel(baseProps({ workspaceUnavailable: "Workspace unavailable." }));
    expect(dragAndDrop().canDrag(["src/app.ts"])).toBe(false);
  });

  it("resyncs Pierre's optimistic move when availability changes before completion", () => {
    harness.persistRefs = true;
    setEntries([entry("src/app.ts", "file")]);
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    renderPanel();
    harness.runEffects();
    const staleDropComplete = dragAndDrop().onDropComplete;

    renderPanel(baseProps({ workspaceUnavailable: "Workspace unavailable." }));
    harness.runEffects();
    staleDropComplete({
      draggedPaths: ["src/app.ts"],
      operation: "move",
      target: directoryTarget("docs/"),
    });

    expect(testState.commandCalls.some((call) => call.label === "renameEntry")).toBe(false);
    expect(refresh).toHaveBeenCalledOnce();
  });
});

describe("refresh", () => {
  it("rebuilds the server index before refreshing the entries query", async () => {
    setEntries([entry("src/app.ts", "file")]);
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    harness.seedState((initial) => initial === null, null);
    harness.seedState((initial) => initial === null, { x: 5, y: 6 });
    renderPanel();

    const menus = ui.filter("FileTreeContextMenu");
    (menus[menus.length - 1]!["actions"] as { onRefresh: () => void }).onRefresh();
    await flushPromises();

    expect(testState.commandCalls.find((call) => call.label === "refreshEntries")?.input).toEqual({
      environmentId,
      input: { cwd: "/workspace/demo", refresh: true },
    });
    expect(refresh).toHaveBeenCalled();
  });

  it("reports a failed server rescan and still re-lists the last-known index", async () => {
    setEntries([entry("src/app.ts", "file")]);
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    testState.commandResults["refreshEntries"] = {
      _tag: "Failure",
      error: new Error("index rebuild failed"),
    };
    harness.seedState((initial) => initial === null, null);
    harness.seedState((initial) => initial === null, { x: 5, y: 6 });
    renderPanel();

    const menus = ui.filter("FileTreeContextMenu");
    (menus[menus.length - 1]!["actions"] as { onRefresh: () => void }).onRefresh();
    await flushPromises();

    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Failed to refresh workspace files",
        description: "index rebuild failed",
      }),
    );
    expect(refresh).toHaveBeenCalledOnce();
  });

  it("coalesces repeated rescan requests while one rebuild is in flight", async () => {
    setEntries([entry("src/app.ts", "file")]);
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    const pending = deferred<{ _tag: "Success"; value: Record<string, never> }>();
    testState.commandResults["refreshEntries"] = pending.promise;
    harness.seedState((initial) => initial === null, null);
    harness.seedState((initial) => initial === null, { x: 5, y: 6 });
    renderPanel();

    const menus = ui.filter("FileTreeContextMenu");
    const onRefresh = (menus[menus.length - 1]!["actions"] as { onRefresh: () => void }).onRefresh;
    onRefresh();
    onRefresh();

    expect(testState.commandCalls.filter((call) => call.label === "refreshEntries")).toHaveLength(
      1,
    );
    expect(refresh).not.toHaveBeenCalled();

    pending.resolve({ _tag: "Success", value: {} });
    await flushPromises();
    expect(refresh).toHaveBeenCalledOnce();
  });

  it("re-lists when the subscription starts with a server resync signal", () => {
    harness.persistRefs = true;
    setEntries([entry("src", "directory")]);
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    testState.entryChangeSignal = { cwd: "/workspace/demo" };

    renderPanel();
    harness.runEffects();

    expect(refresh).toHaveBeenCalledOnce();
    // The signal carries no entries, so the panel re-reads rather than asking for another rescan:
    // the server already dropped its cached index before signalling.
    expect(testState.commandCalls.some((call) => call.label === "refreshEntries")).toBe(false);
  });

  // A refresh re-renders, and `useProjectEntriesQuery` hands back a fresh object each render. An
  // effect keyed on that object re-runs every render, so acting on a signal more than once turns
  // into a hot loop that hammers the server for as long as the panel is open.
  it("re-lists once per signal, not once per render", () => {
    harness.persistRefs = true;
    setEntries([entry("src", "directory")]);
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;

    renderPanel();
    harness.runEffects();
    expect(refresh).not.toHaveBeenCalled();

    testState.entryChangeSignal = { cwd: "/workspace/demo" };
    renderPanel();
    harness.runEffects();
    expect(refresh).toHaveBeenCalledTimes(1);

    // Re-renders carrying the same signal must not refresh again, however many arrive.
    for (let attempt = 0; attempt < 5; attempt += 1) {
      renderPanel();
      harness.runEffects();
    }
    expect(refresh).toHaveBeenCalledTimes(1);

    // A genuinely new signal still refreshes.
    testState.entryChangeSignal = { cwd: "/workspace/demo" };
    renderPanel();
    harness.runEffects();
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  // projects.createEntry and friends already drop the server's cached index, so asking for a second
  // rebuild would ship the whole entry list twice per mutation.
  it("does not ask the server to rescan after a mutation that already invalidated its index", async () => {
    setEntries([entry("src", "directory")]);
    testState.commandResults["createEntry"] = {
      _tag: "Success",
      value: { relativePath: "src/created.ts" },
    };
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    renderPanel();

    rowActionsFor("src", "directory").onNewFile();
    (lastDialogRequest()["onSubmit"] as (name: string) => void)("created.ts");
    await flushPromises();

    expect(refresh).toHaveBeenCalled();
    expect(testState.commandCalls.some((call) => call.label === "refreshEntries")).toBe(false);
  });
});

describe("git decorations", () => {
  it("passes changed files, every ancestor through root, and ignored entries to the tree", () => {
    setEntries([
      entry("apps", "directory"),
      entry("apps/web", "directory"),
      entry("apps/web/src", "directory"),
      entry("apps/web/src/app.ts", "file"),
      entry("docs", "directory"),
      entry("docs/new.ts", "file"),
      { ...entry("cache", "directory"), ignored: true },
    ]);
    testState.vcsStatus = vcsStatus([
      {
        path: "apps\\web\\src\\app.ts",
        insertions: 1,
        deletions: 0,
        status: "modified",
        area: "unstaged",
      },
      {
        path: "docs/new.ts",
        insertions: 1,
        deletions: 0,
        status: "added",
        area: "staged",
      },
    ]);

    renderPanel();

    const expected = [
      { path: "cache/", status: "ignored" },
      { path: "apps/web/src/app.ts", status: "modified" },
      { path: "apps/web/src/", status: "modified" },
      { path: "apps/web/", status: "modified" },
      { path: "apps/", status: "modified" },
      { path: "docs/new.ts", status: "added" },
      { path: "docs/", status: "added" },
    ];
    expect(testState.fileTree.options!["gitStatus"]).toEqual(expected);

    harness.runEffects();
    expect(testState.fileTree.setGitStatus).toHaveBeenCalledWith(expected);
    expect(
      (ui.last("FileTree")!["style"] as Record<string, string>)[
        "--trees-git-ignored-color-override"
      ],
    ).toBe("var(--warning)");
  });
});

describe("selection", () => {
  it("opens a file on selection but ignores directory selection", () => {
    setEntries([entry("src", "directory"), entry("src/app.ts", "file")]);
    const onOpenFile = vi.fn();
    renderPanel(baseProps({ onOpenFile }));

    const onSelectionChange = testState.fileTree.options!["onSelectionChange"] as (
      paths: string[],
    ) => void;
    onSelectionChange(["src/app.ts"]);
    expect(onOpenFile).toHaveBeenCalledWith("src/app.ts");

    onOpenFile.mockClear();
    onSelectionChange(["src/"]);
    expect(onOpenFile).not.toHaveBeenCalled();

    // Empty selection is a no-op.
    onSelectionChange([]);
    expect(onOpenFile).not.toHaveBeenCalled();
  });
});

describe("context menu model", () => {
  beforeEach(() => {
    setEntries([entry("src", "directory"), entry("src/app.ts", "file")]);
  });

  it("positions row menus from a viewport rect instead of the shadow-DOM element", () => {
    renderPanel();
    const rect = { x: 120, y: 48, width: 240, height: 24 } as DOMRect;
    const anchorElement = { getBoundingClientRect: vi.fn(() => rect) };
    const fileTree = ui.last("FileTree")!;
    const element = (
      fileTree["renderContextMenu"] as (
        item: { path: string; kind: string },
        context: { anchorElement: typeof anchorElement; close: () => void },
      ) => React.ReactElement
    )({ path: "src/app.ts", kind: "file" }, { anchorElement, close: vi.fn() });
    const anchor = (element.props as { anchor: { getBoundingClientRect: () => DOMRect } }).anchor;

    expect(anchor).not.toBe(anchorElement);
    expect(anchor.getBoundingClientRect()).toBe(rect);
    expect(anchorElement.getBoundingClientRect).toHaveBeenCalledTimes(1);
  });

  it("builds a file menu with preview + external editor for the primary env", () => {
    testState.isPreviewSupported = true;
    testState.isBrowserPreviewFile = vi.fn(() => true);
    renderPanel();
    const fileTree = ui.last("FileTree")!;
    const element = (
      fileTree["renderContextMenu"] as (
        item: { path: string; kind: string },
        context: { anchorElement: unknown; close: () => void },
      ) => React.ReactElement
    )({ path: "src/app.ts", kind: "file" }, { anchorElement: {}, close: vi.fn() });
    const menu = element.props as { model: { groups: Array<Array<{ id: string }>> } };
    const ids = menu.model.groups.flat().map((item) => item.id);
    expect(ids).toContain("open-preview");
    expect(ids).toContain("open-external-editor");
    expect(ids).toContain("duplicate");
    expect(ids).toContain("rename");
    expect(ids).toContain("delete");
  });

  it("omits external editor for a non-primary environment", () => {
    testState.primaryEnvironmentId = otherEnvironmentId;
    renderPanel();
    const actions = ui.last("FileTree")!;
    const element = (
      actions["renderContextMenu"] as (
        item: { path: string; kind: string },
        context: { anchorElement: unknown; close: () => void },
      ) => React.ReactElement
    )({ path: "src/app.ts", kind: "file" }, { anchorElement: {}, close: vi.fn() });
    const menu = element.props as { model: { groups: Array<Array<{ id: string }>> } };
    const ids = menu.model.groups.flat().map((item) => item.id);
    expect(ids).not.toContain("open-external-editor");
  });
});

describe("create entry", () => {
  beforeEach(() => {
    setEntries([entry("src", "directory"), entry("src/app.ts", "file")]);
  });

  it("creates a file and opens it on success", async () => {
    testState.commandResults["createEntry"] = {
      _tag: "Success",
      value: { relativePath: "src/created.ts" },
    };
    const onOpenFile = vi.fn();
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    renderPanel(baseProps({ onOpenFile }));

    rowActionsFor("src", "directory").onNewFile();
    const request = lastDialogRequest();
    expect(request["title"]).toBe("New File");
    (request["onSubmit"] as (name: string) => void)("created.ts");
    await flushPromises();

    const call = testState.commandCalls.find((entry) => entry.label === "createEntry");
    expect(call?.input).toEqual({
      environmentId,
      input: { cwd: "/workspace/demo", relativePath: "src/created.ts", kind: "file" },
    });
    expect(refresh).toHaveBeenCalled();
    expect(onOpenFile).toHaveBeenCalledWith("src/created.ts");
  });

  it("closes an open mutation dialog and rejects its stale submit after workspace loss", async () => {
    harness.persistRefs = true;
    const reason = "Workspace unavailable. Retry detection or remove it from BiBCode.";
    renderPanel();

    rowActionsFor("src", "directory").onNewFile();
    const staleSubmit = lastDialogRequest()["onSubmit"] as (name: string) => void;

    renderPanel(baseProps({ workspaceUnavailable: reason }));
    harness.runEffects();
    expect(harness.setStateCalls).toEqual(
      expect.arrayContaining([expect.objectContaining({ applied: null })]),
    );

    staleSubmit("blocked.ts");
    await flushPromises();
    expect(testState.commandCalls.some((call) => call.label === "createEntry")).toBe(false);
  });

  it("creates a folder without opening a file", async () => {
    testState.commandResults["createEntry"] = {
      _tag: "Success",
      value: { relativePath: "src/newdir" },
    };
    const onOpenFile = vi.fn();
    renderPanel(baseProps({ onOpenFile }));

    rowActionsFor("src", "directory").onNewFolder();
    const request = lastDialogRequest();
    expect(request["title"]).toBe("New Folder");
    (request["onSubmit"] as (name: string) => void)("newdir");
    await flushPromises();
    expect(onOpenFile).not.toHaveBeenCalled();
  });

  it("reports the symlink-outside-root failure with a plain explanation", async () => {
    testState.commandResults["createEntry"] = {
      _tag: "Failure",
      error: { failure: "resolved_path_outside_root" },
    };
    renderPanel();
    rowActionsFor("src", "directory").onNewFile();
    (lastDialogRequest()["onSubmit"] as (name: string) => void)("x.ts");
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        description: "Can't operate on a symlink that points outside the workspace.",
      }),
    );
  });

  it("reports a generic Error failure with its message", async () => {
    testState.commandResults["createEntry"] = {
      _tag: "Failure",
      error: new Error("disk full"),
    };
    renderPanel();
    rowActionsFor("src", "directory").onNewFile();
    (lastDialogRequest()["onSubmit"] as (name: string) => void)("x.ts");
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ description: "disk full" }),
    );
  });

  it("falls back to a generic message for failures without a message", async () => {
    testState.commandResults["createEntry"] = { _tag: "Failure", error: { unexpected: true } };
    renderPanel();
    rowActionsFor("src", "directory").onNewFile();
    (lastDialogRequest()["onSubmit"] as (name: string) => void)("x.ts");
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ description: "An error occurred." }),
    );
  });

  it("stays silent when the create command is interrupted", async () => {
    testState.commandResults["createEntry"] = { _tag: "Failure", interrupted: true };
    renderPanel();
    rowActionsFor("src", "directory").onNewFile();
    (lastDialogRequest()["onSubmit"] as (name: string) => void)("x.ts");
    await flushPromises();
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });
});

describe("rename entry", () => {
  beforeEach(() => {
    setEntries([entry("src/app.ts", "file")]);
  });

  it("renames a file, remapping open surfaces and refreshing", async () => {
    testState.commandResults["renameEntry"] = {
      _tag: "Success",
      value: { relativePath: "src/renamed.ts" },
    };
    const lease = mutationLease();
    const onBeginPathMutation = vi.fn(async () => lease);
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    renderPanel(baseProps({ onBeginPathMutation }));

    rowActionsFor("src/app.ts", "file").onRename();
    const request = lastDialogRequest();
    expect(request["title"]).toBe("Rename");
    expect(request["initialValue"]).toBe("app.ts");
    (request["onSubmit"] as (name: string) => void)("renamed.ts");
    await flushPromises();

    expect(onBeginPathMutation).toHaveBeenCalledWith({
      kind: "rename",
      fromRelativePath: "src/app.ts",
      toRelativePath: "src/renamed.ts",
    });
    const call = testState.commandCalls.find((entry) => entry.label === "renameEntry");
    expect(call?.input).toEqual({
      environmentId,
      input: {
        cwd: "/workspace/demo",
        fromRelativePath: "src/app.ts",
        toRelativePath: "src/renamed.ts",
      },
    });
    expect(testState.remapFileSurfaces).toHaveBeenCalledWith(
      threadRef,
      "src/app.ts",
      "src/renamed.ts",
    );
    expect(lease.commitRename).toHaveBeenCalledWith("src/renamed.ts");
    expect(lease.commitRename.mock.invocationCallOrder[0]).toBeLessThan(
      vi.mocked(testState.remapFileSurfaces).mock.invocationCallOrder[0]!,
    );
    expect(refresh).toHaveBeenCalled();
  });

  it("aborts rename when pending edits cannot settle", async () => {
    const onBeginPathMutation = vi.fn(async () => null);
    renderPanel(baseProps({ onBeginPathMutation }));

    rowActionsFor("src/app.ts", "file").onRename();
    (lastDialogRequest()["onSubmit"] as (name: string) => void)("renamed.ts");
    await flushPromises();

    expect(testState.commandCalls.some((call) => call.label === "renameEntry")).toBe(false);
    expect(onBeginPathMutation).toHaveBeenCalledWith({
      kind: "rename",
      fromRelativePath: "src/app.ts",
      toRelativePath: "src/renamed.ts",
    });
  });

  it("holds a mutation lease until a delayed rename remaps surfaces", async () => {
    const renameResult = deferred<{
      _tag: "Success";
      value: { relativePath: string };
    }>();
    testState.commandResults["renameEntry"] = renameResult.promise;
    const lease = mutationLease();
    const onBeginPathMutation = vi.fn(async () => lease);
    renderPanel(
      baseProps({
        onBeginPathMutation,
      }),
    );

    rowActionsFor("src/app.ts", "file").onRename();
    (lastDialogRequest()["onSubmit"] as (name: string) => void)("renamed.ts");
    await flushPromises();

    expect(onBeginPathMutation).toHaveBeenCalledWith({
      kind: "rename",
      fromRelativePath: "src/app.ts",
      toRelativePath: "src/renamed.ts",
    });
    expect(lease.release).not.toHaveBeenCalled();

    renameResult.resolve({
      _tag: "Success",
      value: { relativePath: "src/renamed.ts" },
    });
    await flushPromises();

    expect(lease.commitRename).toHaveBeenCalledWith("src/renamed.ts");
    expect(lease.commitRename.mock.invocationCallOrder[0]).toBeLessThan(
      vi.mocked(testState.remapFileSurfaces).mock.invocationCallOrder[0]!,
    );
    expect(vi.mocked(testState.remapFileSurfaces).mock.invocationCallOrder[0]).toBeLessThan(
      lease.release.mock.invocationCallOrder[0]!,
    );
  });

  it("releases a mutation lease unchanged when rename fails", async () => {
    testState.commandResults["renameEntry"] = {
      _tag: "Failure",
      error: new Error("locked"),
    };
    const lease = mutationLease();
    renderPanel(
      baseProps({
        onBeginPathMutation: vi.fn(async () => lease),
      }),
    );

    rowActionsFor("src/app.ts", "file").onRename();
    (lastDialogRequest()["onSubmit"] as (name: string) => void)("renamed.ts");
    await flushPromises();

    expect(lease.commitRename).not.toHaveBeenCalled();
    expect(lease.commitDelete).not.toHaveBeenCalled();
    expect(lease.release).toHaveBeenCalledOnce();
  });

  it("contains rejected rename lease acquisition without running the command", async () => {
    const onBeginPathMutation = vi.fn(async () => {
      throw new Error("settlement exploded");
    });
    renderPanel(baseProps({ onBeginPathMutation }));

    rowActionsFor("src/app.ts", "file").onRename();
    (lastDialogRequest()["onSubmit"] as (name: string) => void)("renamed.ts");
    await flushPromises();

    expect(testState.commandCalls.some((call) => call.label === "renameEntry")).toBe(false);
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: 'Failed to rename "app.ts"',
        description: "settlement exploded",
      }),
    );
  });

  it("does nothing when the name is unchanged", async () => {
    renderPanel();
    rowActionsFor("src/app.ts", "file").onRename();
    (lastDialogRequest()["onSubmit"] as (name: string) => void)("app.ts");
    await flushPromises();
    expect(testState.commandCalls.some((entry) => entry.label === "renameEntry")).toBe(false);
  });

  it("reports rename failures", async () => {
    testState.commandResults["renameEntry"] = { _tag: "Failure", error: new Error("locked") };
    renderPanel();
    rowActionsFor("src/app.ts", "file").onRename();
    (lastDialogRequest()["onSubmit"] as (name: string) => void)("other.ts");
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: 'Failed to rename "app.ts"', description: "locked" }),
    );
    expect(testState.remapFileSurfaces).not.toHaveBeenCalled();
  });
});

describe("delete entry", () => {
  it("deletes a file behind a confirm and closes its surfaces", async () => {
    setEntries([entry("src/app.ts", "file")]);
    testState.commandResults["deleteEntry"] = { _tag: "Success", value: {} };
    const lease = mutationLease();
    const onBeginPathMutation = vi.fn(async () => lease);
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    renderPanel(baseProps({ onBeginPathMutation }));

    rowActionsFor("src/app.ts", "file").onDelete();
    const request = lastDialogRequest();
    expect(request["mode"]).toBe("confirm");
    expect(request["title"]).toBe("Delete file");
    expect(request["destructive"]).toBe(true);
    (request["onConfirm"] as () => void)();
    await flushPromises();

    expect(onBeginPathMutation).toHaveBeenCalledWith({
      kind: "delete",
      relativePath: "src/app.ts",
    });
    expect(lease.commitDelete).toHaveBeenCalledOnce();
    expect(lease.commitDelete.mock.invocationCallOrder[0]).toBeLessThan(
      vi.mocked(testState.closeFileSurfacesUnder).mock.invocationCallOrder[0]!,
    );
    expect(testState.closeFileSurfacesUnder).toHaveBeenCalledWith(threadRef, "src/app.ts");
    expect(refresh).toHaveBeenCalled();
  });

  it("aborts delete when pending edits cannot settle", async () => {
    setEntries([entry("src/app.ts", "file")]);
    const onBeginPathMutation = vi.fn(async () => null);
    renderPanel(baseProps({ onBeginPathMutation }));

    rowActionsFor("src/app.ts", "file").onDelete();
    (lastDialogRequest()["onConfirm"] as () => void)();
    await flushPromises();

    expect(testState.commandCalls.some((call) => call.label === "deleteEntry")).toBe(false);
    expect(onBeginPathMutation).toHaveBeenCalledWith({
      kind: "delete",
      relativePath: "src/app.ts",
    });
  });

  it("commits delete before releasing the mutation lease", async () => {
    setEntries([entry("src/app.ts", "file")]);
    testState.commandResults["deleteEntry"] = { _tag: "Success", value: {} };
    const lease = mutationLease();
    renderPanel(
      baseProps({
        onBeginPathMutation: vi.fn(async () => lease),
      }),
    );

    rowActionsFor("src/app.ts", "file").onDelete();
    (lastDialogRequest()["onConfirm"] as () => void)();
    await flushPromises();

    expect(lease.commitDelete).toHaveBeenCalledOnce();
    expect(lease.commitDelete.mock.invocationCallOrder[0]).toBeLessThan(
      lease.release.mock.invocationCallOrder[0]!,
    );
  });

  it("finishes a successful directory delete when editor cleanup reports an error", async () => {
    const reportError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      setEntries([entry("src", "directory")]);
      testState.commandResults["deleteEntry"] = { _tag: "Success", value: {} };
      const refresh = vi.fn();
      testState.entriesQuery.refresh = refresh;
      const registry = new FileEditingSessionRegistry<ReturnType<typeof editingSession>>();
      const rejected = registry.getOrCreate("src/a.ts", () => editingSession("src/a.ts"));
      const cleaned = registry.getOrCreate("src/nested/b.ts", () =>
        editingSession("src/nested/b.ts"),
      );
      rejected.discardPendingSave.mockImplementation(() => {
        throw new Error("discard failed");
      });
      renderPanel(
        baseProps({
          onBeginPathMutation: (request) => registry.beginPathMutation(request),
        }),
      );

      rowActionsFor("src", "directory").onDelete();
      (lastDialogRequest()["onConfirm"] as () => void)();
      await flushPromises();

      expect(registry.get("src/a.ts")).toBeUndefined();
      expect(registry.get("src/nested/b.ts")).toBeUndefined();
      expect(rejected.dispose).toHaveBeenCalledOnce();
      expect(cleaned.dispose).toHaveBeenCalledOnce();
      expect(testState.closeFileSurfacesUnder).toHaveBeenCalledWith(threadRef, "src");
      expect(refresh).toHaveBeenCalledOnce();
      expect(testState.toastAdd).not.toHaveBeenCalled();
      expect(reportError).toHaveBeenCalledWith(
        "[file-editing-session-registry] session cleanup failed",
        expect.objectContaining({ message: "discard failed" }),
      );
      const nextLease = await registry.beginPathMutation({
        kind: "delete",
        relativePath: "src",
      });
      expect(nextLease).not.toBeNull();
      nextLease!.release();
    } finally {
      reportError.mockRestore();
    }
  });

  it("contains rejected delete lease acquisition without running the command", async () => {
    setEntries([entry("src/app.ts", "file")]);
    const onBeginPathMutation = vi.fn(async () => {
      throw new Error("settlement exploded");
    });
    renderPanel(baseProps({ onBeginPathMutation }));

    rowActionsFor("src/app.ts", "file").onDelete();
    (lastDialogRequest()["onConfirm"] as () => void)();
    await flushPromises();

    expect(testState.commandCalls.some((call) => call.label === "deleteEntry")).toBe(false);
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: 'Failed to delete "app.ts"',
        description: "settlement exploded",
      }),
    );
  });

  it("uses folder wording for directories and reports failures", async () => {
    setEntries([entry("src", "directory")]);
    testState.commandResults["deleteEntry"] = { _tag: "Failure", error: new Error("busy") };
    renderPanel();

    rowActionsFor("src", "directory").onDelete();
    const request = lastDialogRequest();
    expect(request["title"]).toBe("Delete folder");
    expect(String(request["description"])).toContain("everything inside it");
    (request["onConfirm"] as () => void)();
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: 'Failed to delete "src"', description: "busy" }),
    );
    expect(testState.closeFileSurfacesUnder).not.toHaveBeenCalled();
  });
});

describe("duplicate entry", () => {
  beforeEach(() => {
    setEntries([entry("src/app.ts", "file")]);
  });

  it("duplicates a file and opens the copy", async () => {
    testState.commandResults["duplicateEntry"] = {
      _tag: "Success",
      value: { relativePath: "src/app copy.ts" },
    };
    const onOpenFile = vi.fn();
    const onBeginPathMutation = vi.fn(async () => mutationLease());
    renderPanel(baseProps({ onOpenFile, onBeginPathMutation }));

    rowActionsFor("src/app.ts", "file").onDuplicate();
    await flushPromises();

    expect(onBeginPathMutation).toHaveBeenCalledWith({
      kind: "duplicate",
      relativePath: "src/app.ts",
    });
    expect(onOpenFile).toHaveBeenCalledWith("src/app copy.ts");
  });

  it("aborts duplicate when pending edits cannot settle", async () => {
    const onBeginPathMutation = vi.fn(async () => null);
    const onOpenFile = vi.fn();
    renderPanel(baseProps({ onBeginPathMutation, onOpenFile }));

    rowActionsFor("src/app.ts", "file").onDuplicate();
    await flushPromises();

    expect(testState.commandCalls.some((call) => call.label === "duplicateEntry")).toBe(false);
    expect(onOpenFile).not.toHaveBeenCalled();
  });

  it("contains rejected duplicate lease acquisition without running the command", async () => {
    const onBeginPathMutation = vi.fn(async () => {
      throw new Error("settlement exploded");
    });
    renderPanel(baseProps({ onBeginPathMutation }));

    rowActionsFor("src/app.ts", "file").onDuplicate();
    await flushPromises();

    expect(testState.commandCalls.some((call) => call.label === "duplicateEntry")).toBe(false);
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: 'Failed to duplicate "app.ts"',
        description: "settlement exploded",
      }),
    );
  });

  it("reports duplicate failures", async () => {
    testState.commandResults["duplicateEntry"] = { _tag: "Failure", error: new Error("nope") };
    renderPanel();
    rowActionsFor("src/app.ts", "file").onDuplicate();
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: 'Failed to duplicate "app.ts"', description: "nope" }),
    );
  });
});

describe("copy path", () => {
  beforeEach(() => {
    setEntries([entry("src/app.ts", "file")]);
  });

  it("copies the absolute and relative paths to the clipboard", () => {
    const writeText = vi.fn(async () => {});
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    renderPanel();
    const actions = rowActionsFor("src/app.ts", "file");
    actions.onCopyPath();
    expect(writeText).toHaveBeenCalledWith("/workspace/demo/src/app.ts");
    actions.onCopyRelativePath();
    expect(writeText).toHaveBeenCalledWith("src/app.ts");
  });

  it("toasts when the clipboard write rejects", async () => {
    const error = new Error("denied");
    const writeText = vi.fn(async () => {
      throw error;
    });
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    renderPanel();
    rowActionsFor("src/app.ts", "file").onCopyPath();
    await flushPromises();
    expect(consoleError).toHaveBeenCalled();
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Unable to copy to clipboard", description: "denied" }),
    );
    consoleError.mockRestore();
  });
});

describe("open in file manager", () => {
  it("opens a local directory through the desktop bridge", async () => {
    const openInFileManager = vi.fn(async () => {});
    vi.stubGlobal("window", { desktopBridge: { openInFileManager } });
    setEntries([entry("packages", "directory")]);

    renderPanel();
    const actions = rowActionsFor("packages", "directory") as RowActions & {
      onOpenFileManager: () => void;
    };
    actions.onOpenFileManager();
    await flushPromises();

    expect(openInFileManager).toHaveBeenCalledWith("/workspace/demo/packages", true);
  });
});

describe("add as project", () => {
  it("uses the shared Codex model, effort, and fast defaults for the created default thread", async () => {
    testState.environment = {
      serverConfig: {
        providers: [
          {
            instanceId: "codex",
            driver: "codex",
            models: [
              {
                slug: "gpt-5.4",
                capabilities: {
                  optionDescriptors: [
                    {
                      id: "reasoningEffort",
                      label: "Reasoning",
                      type: "select",
                      options: [
                        { id: "medium", label: "Medium", isDefault: true },
                        { id: "high", label: "High" },
                      ],
                      currentValue: "medium",
                    },
                    {
                      id: "serviceTier",
                      label: "Service tier",
                      type: "select",
                      options: [
                        { id: "default", label: "Default", isDefault: true },
                        { id: "fast", label: "Fast" },
                      ],
                      currentValue: "default",
                    },
                  ],
                },
              },
            ],
          },
        ],
        settings: {
          ...DEFAULT_SERVER_SETTINGS,
          providerSessionDefaults: {
            codex: {
              model: "gpt-5.4",
              options: [
                { id: "reasoningEffort", value: "high" },
                { id: "serviceTier", value: "fast" },
              ],
            },
          },
        },
      },
    };
    setEntries([entry("packages/app", "directory")]);
    renderPanel();
    rowActionsFor("packages/app", "directory").onAddAsProject();
    await flushPromises();

    const call = testState.commandCalls.find((entry) => entry.label === "create");
    expect(call?.input).toEqual(
      expect.objectContaining({
        input: expect.objectContaining({
          defaultModelSelection: {
            instanceId: "codex",
            model: "gpt-5.4",
            options: [
              { id: "reasoningEffort", value: "high" },
              { id: "serviceTier", value: "fast" },
            ],
          },
        }),
      }),
    );
  });

  it("reports failures adding a directory as a project", async () => {
    setEntries([entry("packages/app", "directory")]);
    testState.commandResults["create"] = { _tag: "Failure", error: new Error("exists") };
    renderPanel();
    rowActionsFor("packages/app", "directory").onAddAsProject();
    await flushPromises();
    const call = testState.commandCalls.find((entry) => entry.label === "create");
    expect(call).toBeDefined();
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Failed to add project at /workspace/demo/packages/app",
        description: "exists",
      }),
    );
  });

  it("stays silent on success", async () => {
    setEntries([entry("packages/app", "directory")]);
    testState.commandResults["create"] = { _tag: "Success", value: {} };
    renderPanel();
    rowActionsFor("packages/app", "directory").onAddAsProject();
    await flushPromises();
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });
});

describe("open in external editor", () => {
  beforeEach(() => {
    setEntries([entry("src/app.ts", "file")]);
  });

  it("does nothing without a preferred editor", () => {
    testState.preferredEditor = null;
    renderPanel();
    rowActionsFor("src/app.ts", "file").onOpenExternalEditor();
    expect(testState.commandCalls.some((entry) => entry.label === "openInEditor")).toBe(false);
  });

  it("launches the preferred editor at the joined workspace path", () => {
    testState.preferredEditor = "vscode";
    renderPanel();
    rowActionsFor("src/app.ts", "file").onOpenExternalEditor();
    const call = testState.commandCalls.find((entry) => entry.label === "openInEditor");
    expect(call?.input).toEqual({
      environmentId,
      input: { cwd: "/workspace/demo/src/app.ts", editor: "vscode" },
    });
  });
});

describe("open in preview", () => {
  beforeEach(() => {
    setEntries([entry("index.html", "file")]);
  });

  it("does nothing without an environment base URL", () => {
    testState.environmentHttpBaseUrl = null;
    renderPanel();
    rowActionsFor("index.html", "file").onOpenPreview();
    expect(testState.openFileInPreview).not.toHaveBeenCalled();
  });

  it("opens the file through the preview flow", async () => {
    testState.environmentHttpBaseUrl = "http://127.0.0.1:4100";
    testState.openFileInPreview = vi.fn(async () => ({ _tag: "Success" }));
    renderPanel();
    rowActionsFor("index.html", "file").onOpenPreview();
    await flushPromises();
    expect(testState.openFileInPreview).toHaveBeenCalledWith(
      expect.objectContaining({
        threadRef,
        filePath: "/workspace/demo/index.html",
        httpBaseUrl: "http://127.0.0.1:4100",
      }),
    );
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });

  it("reports preview failures but ignores interrupts", async () => {
    testState.environmentHttpBaseUrl = "http://127.0.0.1:4100";
    testState.openFileInPreview = vi.fn(async () => ({
      _tag: "Failure",
      error: new Error("no server"),
    }));
    renderPanel();
    rowActionsFor("index.html", "file").onOpenPreview();
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Unable to open file in browser",
        description: "no server",
      }),
    );

    testState.toastAdd = vi.fn();
    testState.openFileInPreview = vi.fn(async () => ({ _tag: "Interrupted" }));
    renderPanel();
    rowActionsFor("index.html", "file").onOpenPreview();
    await flushPromises();
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });
});

describe("download entry", () => {
  beforeEach(() => {
    setEntries([entry("src", "directory"), entry("src/app.ts", "file")]);
    testState.environmentHttpBaseUrl = "http://127.0.0.1:4100";
    testState.commandResults["createDownloadUrl"] = {
      _tag: "Success",
      value: {
        relativeUrl: "/api/transfers/t.k",
        expiresAt: 1,
        fileName: "src.zip",
        kind: "archive",
      },
    };
  });

  it("mints a transfer URL and saves it through the desktop bridge", async () => {
    const downloadToFolder = vi.fn(async () => "/home/me/Downloads/src (1).zip");
    const pickFolder = vi.fn(async () => "/home/me/Downloads");
    stubDesktopBridge({ pickFolder, downloadToFolder });
    renderPanel();

    rowActionsFor("src", "directory").onDownload();
    await flushPromises();

    const minted = testState.commandCalls.find((call) => call.label === "createDownloadUrl");
    expect(minted!.input).toEqual({
      environmentId,
      input: { cwd: "/workspace/demo", relativePath: "src" },
    });
    expect(downloadToFolder).toHaveBeenCalledWith({
      url: "http://127.0.0.1:4100/api/transfers/t.k",
      directory: "/home/me/Downloads",
      fileName: "src.zip",
    });
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "success",
        title: "Download saved",
        description: "/home/me/Downloads/src (1).zip",
      }),
    );
  });

  it("says nothing extra when the folder picker is dismissed", async () => {
    stubDesktopBridge({ pickFolder: vi.fn(async () => null), downloadToFolder: vi.fn() });
    renderPanel();

    rowActionsFor("src", "directory").onDownload();
    await flushPromises();

    expect(testState.toastAdd).not.toHaveBeenCalled();
    expect(testState.commandCalls.some((call) => call.label === "createDownloadUrl")).toBe(false);
  });

  it("waits for the destination before minting a short-lived download URL", async () => {
    const selected = deferred<string | null>();
    const downloadToFolder = vi.fn(async () => "/home/me/Downloads/src.zip");
    stubDesktopBridge({ pickFolder: () => selected.promise, downloadToFolder });
    renderPanel();

    rowActionsFor("src", "directory").onDownload();
    await flushPromises();
    expect(testState.commandCalls.some((call) => call.label === "createDownloadUrl")).toBe(false);

    selected.resolve("/home/me/Downloads");
    await flushPromises();
    expect(downloadToFolder).toHaveBeenCalledOnce();
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Download saved" }),
    );
  });

  it("hands the URL to the browser downloader without a desktop bridge", async () => {
    renderPanel();
    const anchor = { href: "", download: "", rel: "", click: vi.fn(), remove: vi.fn() };
    vi.stubGlobal("document", {
      createElement: vi.fn(() => anchor),
      body: { appendChild: vi.fn() },
    });

    rowActionsFor("src/app.ts", "file").onDownload();
    await flushPromises();

    expect(anchor.href).toBe("http://127.0.0.1:4100/api/transfers/t.k");
    expect(anchor.download).toBe("src.zip");
    expect(anchor.click).toHaveBeenCalledOnce();
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });

  it("explains a download that cannot start because the server is unknown", async () => {
    testState.environmentHttpBaseUrl = null;
    renderPanel();

    rowActionsFor("src", "directory").onDownload();
    await flushPromises();

    expect(testState.commandCalls.some((call) => call.label === "createDownloadUrl")).toBe(false);
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        title: 'Can’t download "src"',
        description: "Not connected to this environment's server. Reconnect and try again.",
      }),
    );
  });

  it("reports a failed mint but ignores interrupts", async () => {
    testState.commandResults["createDownloadUrl"] = {
      _tag: "Failure",
      error: new Error("no such entry"),
    };
    renderPanel();
    rowActionsFor("src", "directory").onDownload();
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: 'Failed to prepare a download for "src"',
        description: "no such entry",
      }),
    );

    testState.toastAdd = vi.fn();
    testState.commandResults["createDownloadUrl"] = { _tag: "Failure", interrupted: true };
    renderPanel();
    rowActionsFor("src", "directory").onDownload();
    await flushPromises();
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });

  it("refuses a minted URL that points away from this environment's server", async () => {
    // The desktop host streams this URL with host privileges, so a foreign origin must never
    // reach its download command, even after choosing the destination.
    testState.commandResults["createDownloadUrl"] = {
      _tag: "Success",
      value: {
        relativeUrl: "https://evil.example/api/transfers/t.k",
        expiresAt: 1,
        fileName: "src.zip",
        kind: "archive",
      },
    };
    const downloadToFolder = vi.fn();
    const pickFolder = vi.fn(async () => "/home/me/Downloads");
    stubDesktopBridge({ pickFolder, downloadToFolder });
    renderPanel();

    rowActionsFor("src", "directory").onDownload();
    await flushPromises();

    expect(downloadToFolder).not.toHaveBeenCalled();
    expect(pickFolder).toHaveBeenCalledOnce();
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        title: 'Can’t download "src"',
        description: "The server did not return a usable download URL.",
      }),
    );
  });

  it("reports a desktop transfer that fails", async () => {
    stubDesktopBridge({
      pickFolder: vi.fn(async () => "/home/me/Downloads"),
      downloadToFolder: vi.fn(async () => {
        throw new Error("disk full");
      }),
    });
    renderPanel();

    rowActionsFor("src", "directory").onDownload();
    await flushPromises();

    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: 'Failed to download "src"', description: "disk full" }),
    );
  });

  it.each([
    "Could not write the download file: disk full",
    { message: "Could not write the download file: disk full" },
  ])(
    "preserves native and structured failure details in the toast and diagnostics",
    async (error) => {
      const report = vi.spyOn(console, "error").mockImplementation(() => undefined);
      try {
        stubDesktopBridge({
          pickFolder: vi.fn(async () => "/home/me/Downloads"),
          downloadToFolder: vi.fn().mockRejectedValue(error),
        });
        renderPanel();
        rowActionsFor("src", "directory").onDownload();
        await flushPromises();

        expect(testState.toastAdd).toHaveBeenCalledWith(
          expect.objectContaining({
            title: 'Failed to download "src"',
            description: "Could not write the download file: disk full",
          }),
        );
        expect(report).toHaveBeenCalledWith(
          "[file-browser] operation failed",
          expect.objectContaining({ message: "Could not write the download file: disk full" }),
        );
      } finally {
        report.mockRestore();
      }
    },
  );

  it("stays available while the workspace is unavailable", () => {
    renderPanel(baseProps({ workspaceUnavailable: "Workspace unavailable." }));
    const actions = rowMenuFor("src", "directory").props as {
      actions: Record<string, unknown>;
    };
    expect(actions.actions["onDownload"]).toEqual(expect.any(Function));
    expect(actions.actions["onUpload"]).toBeUndefined();
  });

  it("redacts signed transfer URLs from native failure diagnostics", async () => {
    const report = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      stubDesktopBridge({
        pickFolder: vi.fn(async () => "/home/me/Downloads"),
        downloadToFolder: vi
          .fn()
          .mockRejectedValue(
            "Download request failed (https://remote.example/api/transfers/secret-token)",
          ),
      });
      renderPanel();
      rowActionsFor("src", "directory").onDownload();
      await flushPromises();

      expect(report).toHaveBeenCalledOnce();
      expect(JSON.stringify(report.mock.calls)).not.toContain("secret-token");
      expect(testState.toastAdd).toHaveBeenCalledWith(
        expect.objectContaining({ description: expect.stringContaining("[REDACTED]") }),
      );
    } finally {
      report.mockRestore();
    }
  });
});

describe("upload files", () => {
  beforeEach(() => {
    setEntries([entry("src", "directory"), entry("src/app.ts", "file")]);
    testState.environmentHttpBaseUrl = "http://127.0.0.1:4100";
    testState.commandResults["createUploadUrl"] = {
      _tag: "Success",
      value: { relativeUrl: "/api/transfers/u.k", expiresAt: 1, maxBytes: 1024 },
    };
  });

  it("offers Upload on folder rows and disables it on file rows", () => {
    renderPanel();
    expect(rowActionsFor("src", "directory").onUpload).toBeDefined();
    expect(rowActionsFor("src/app.ts", "file").onUpload).toBeDefined();
    const fileItem = rowModelFor("src/app.ts", "file")
      .groups.flat()
      .find((item) => item.id === "upload");
    expect(fileItem?.enabled).toBe(false);
    const folderItem = rowModelFor("src", "directory")
      .groups.flat()
      .find((item) => item.id === "upload");
    expect(folderItem?.enabled).toBe(true);
  });

  it("uploads every picked file into the right-clicked folder and refreshes the tree", async () => {
    const uploadFile = vi.fn(async () => ({ status: 201, body: '{"relativePath":"src/a.txt"}' }));
    const pickFiles = vi.fn(async () => ["/home/me/a.txt", "/home/me/b.txt"]);
    stubDesktopBridge({ pickFolder: vi.fn(async () => null), pickFiles, uploadFile });
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    renderPanel();

    rowActionsFor("src", "directory").onUpload();
    await flushPromises();

    expect(pickFiles).toHaveBeenCalledWith({ title: "Select files to upload" });
    const minted = testState.commandCalls.find((call) => call.label === "createUploadUrl");
    expect(minted!.input).toEqual({
      environmentId,
      input: { cwd: "/workspace/demo", relativeDirectory: "src", fileName: "a.txt" },
    });
    // One token per file, each bound to that file's name, so no name reaches the query.
    expect(
      testState.commandCalls
        .filter((call) => call.label === "createUploadUrl")
        .map((call) => (call.input as { input: { fileName: string } }).input.fileName),
    ).toEqual(["a.txt", "b.txt"]);
    expect(uploadFile).toHaveBeenNthCalledWith(1, {
      url: "http://127.0.0.1:4100/api/transfers/u.k",
      path: "/home/me/a.txt",
    });
    expect(uploadFile).toHaveBeenNthCalledWith(2, {
      url: "http://127.0.0.1:4100/api/transfers/u.k",
      path: "/home/me/b.txt",
    });
    expect(refresh).toHaveBeenCalled();
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });

  it("keeps uploading the rest of the batch when one file is rejected", async () => {
    const uploadFile = vi
      .fn<(input: { url: string; path: string }) => Promise<{ status: number; body: string }>>()
      .mockRejectedValueOnce(new Error("Could not read the file."))
      .mockResolvedValueOnce({ status: 201, body: '{"relativePath":"src/b.txt"}' });
    stubDesktopBridge({
      pickFolder: vi.fn(async () => null),
      pickFiles: vi.fn(async () => ["/home/me/a.txt", "/home/me/b.txt"]),
      uploadFile,
    });
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    renderPanel();

    rowActionsFor("src", "directory").onUpload();
    await flushPromises();

    expect(testState.toastAdd).toHaveBeenCalledTimes(1);
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        title: 'Can\u2019t upload "a.txt"',
        description: "Could not read the file.",
      }),
    );
    expect(uploadFile).toHaveBeenNthCalledWith(2, {
      url: "http://127.0.0.1:4100/api/transfers/u.k",
      path: "/home/me/b.txt",
    });
    expect(refresh).toHaveBeenCalled();
  });

  it("targets a file row's parent folder", async () => {
    stubDesktopBridge({
      pickFolder: vi.fn(async () => null),
      pickFiles: vi.fn(async () => ["/home/me/a.txt"]),
      uploadFile: vi.fn(async () => ({ status: 201, body: '{"relativePath":"src/a.txt"}' })),
    });
    renderPanel();

    rowActionsFor("src/app.ts", "file").onUpload();
    await flushPromises();

    const minted = testState.commandCalls.find((call) => call.label === "createUploadUrl");
    expect(
      (minted!.input as { input: { relativeDirectory: string } }).input.relativeDirectory,
    ).toBe("src");
  });

  it("asks before replacing an existing file and retries with overwrite", async () => {
    const uploadFile = vi
      .fn<(input: { url: string; path: string }) => Promise<{ status: number; body: string }>>()
      .mockResolvedValueOnce({ status: 409, body: '{"_tag":"TransferEntryExistsError"}' })
      .mockResolvedValueOnce({ status: 201, body: '{"relativePath":"src/a.txt"}' });
    stubDesktopBridge({
      pickFolder: vi.fn(async () => null),
      pickFiles: vi.fn(async () => ["/home/me/a.txt"]),
      uploadFile,
    });
    renderPanel();

    rowActionsFor("src", "directory").onUpload();
    await flushPromises();

    const request = lastDialogRequest();
    expect(request["mode"]).toBe("confirm");
    expect(request["title"]).toBe('Replace "a.txt"?');
    expect(request["destructive"]).toBe(true);
    expect(request["confirmLabel"]).toBe("Replace");
    expect(request["description"]).toContain("src");
    expect(uploadFile).toHaveBeenCalledTimes(1);

    (request["onConfirm"] as () => void)();
    await flushPromises();

    expect(uploadFile).toHaveBeenNthCalledWith(2, {
      url: "http://127.0.0.1:4100/api/transfers/u.k?overwrite=1",
      path: "/home/me/a.txt",
    });
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });

  it("keeps the existing file when the replace prompt is dismissed", async () => {
    const uploadFile = vi.fn(async () => ({
      status: 409,
      body: '{"_tag":"TransferEntryExistsError"}',
    }));
    stubDesktopBridge({
      pickFolder: vi.fn(async () => null),
      pickFiles: vi.fn(async () => ["/home/me/a.txt"]),
      uploadFile,
    });
    renderPanel();

    rowActionsFor("src", "directory").onUpload();
    await flushPromises();

    const dialog = ui.last("FileEntryDialog")!;
    (dialog["onClose"] as () => void)();
    await flushPromises();

    expect(uploadFile).toHaveBeenCalledTimes(1);
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });

  it("explains a file the server refuses as too large", async () => {
    stubDesktopBridge({
      pickFolder: vi.fn(async () => null),
      pickFiles: vi.fn(async () => ["/home/me/big.bin"]),
      uploadFile: vi.fn(async () => ({
        status: 413,
        body: `{"_tag":"TransferTooLargeError","limit":${1024 * 1024 * 1024}}`,
      })),
    });
    renderPanel();

    rowActionsFor("src", "directory").onUpload();
    await flushPromises();

    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        title: 'Can’t upload "big.bin"',
        description: "The file is larger than the 1 GiB the server accepts.",
      }),
    );
  });

  it("refuses a minted upload URL that points away from this environment's server", async () => {
    testState.commandResults["createUploadUrl"] = {
      _tag: "Success",
      value: {
        relativeUrl: "https://evil.example/api/transfers/u.k",
        expiresAt: 1,
        maxBytes: 1024,
      },
    };
    const uploadFile = vi.fn();
    stubDesktopBridge({
      pickFolder: vi.fn(async () => null),
      pickFiles: vi.fn(async () => ["/home/me/a.txt"]),
      uploadFile,
    });
    renderPanel();

    rowActionsFor("src", "directory").onUpload();
    await flushPromises();

    expect(uploadFile).not.toHaveBeenCalled();
    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        title: 'Can’t upload "a.txt"',
        description: "The server did not return a usable upload URL.",
      }),
    );
  });

  it("reports a failed mint", async () => {
    testState.commandResults["createUploadUrl"] = {
      _tag: "Failure",
      error: new Error("read-only workspace"),
    };
    stubDesktopBridge({
      pickFolder: vi.fn(async () => null),
      pickFiles: vi.fn(async () => ["/home/me/a.txt"]),
      uploadFile: vi.fn(),
    });
    renderPanel();

    rowActionsFor("src", "directory").onUpload();
    await flushPromises();

    expect(testState.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: 'Failed to prepare an upload to "src"',
        description: "read-only workspace",
      }),
    );
  });

  it("does nothing when the picker is dismissed", async () => {
    const uploadFile = vi.fn();
    stubDesktopBridge({
      pickFolder: vi.fn(async () => null),
      pickFiles: vi.fn(async () => []),
      uploadFile,
    });
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    renderPanel();

    rowActionsFor("src", "directory").onUpload();
    await flushPromises();

    expect(uploadFile).not.toHaveBeenCalled();
    expect(testState.commandCalls.some((call) => call.label === "createUploadUrl")).toBe(false);
    expect(refresh).not.toHaveBeenCalled();
  });
});

describe("background context menu", () => {
  beforeEach(() => {
    setEntries([entry("src/app.ts", "file")]);
  });

  it("opens the background menu on an unhandled right-click", () => {
    renderPanel();
    const fileTree = ui.last("FileTree")!;
    const onContextMenu = fileTree["onContextMenu"] as (event: {
      defaultPrevented: boolean;
      preventDefault: () => void;
      clientX: number;
      clientY: number;
    }) => void;

    const preventDefault = vi.fn();
    onContextMenu({ defaultPrevented: false, preventDefault, clientX: 12, clientY: 34 });
    expect(preventDefault).toHaveBeenCalled();
    expect(
      harness.setStateCalls.some(
        (call) =>
          call.applied !== null &&
          typeof call.applied === "object" &&
          (call.applied as { x?: number }).x === 12,
      ),
    ).toBe(true);
  });

  it("ignores right-clicks already handled by a row", () => {
    renderPanel();
    const fileTree = ui.last("FileTree")!;
    const onContextMenu = fileTree["onContextMenu"] as (event: {
      defaultPrevented: boolean;
      preventDefault: () => void;
    }) => void;
    const preventDefault = vi.fn();
    onContextMenu({ defaultPrevented: true, preventDefault });
    expect(preventDefault).not.toHaveBeenCalled();
  });

  it("renders the background menu and wires its actions when open", async () => {
    testState.commandResults["createEntry"] = { _tag: "Success", value: { relativePath: "a.ts" } };
    const writeText = vi.fn(async () => {});
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    // Seed dialogRequest=null (identity) then backgroundMenu={x,y}.
    harness.seedState((initial) => initial === null, null);
    harness.seedState((initial) => initial === null, { x: 5, y: 6 });
    const refresh = vi.fn();
    testState.entriesQuery.refresh = refresh;
    renderPanel();

    const menus = ui.filter("FileTreeContextMenu");
    const background = menus[menus.length - 1]!;
    const model = background["model"] as { groups: Array<Array<{ id: string }>> };
    expect(model.groups.flat().map((item) => item.id)).toEqual(
      expect.arrayContaining(["new-file", "new-folder", "copy-path", "refresh"]),
    );

    const actions = background["actions"] as {
      onCopyPath: () => void;
      onRefresh: () => void;
      onNewFile: () => void;
      onNewFolder: () => void;
    };
    actions.onCopyPath();
    expect(writeText).toHaveBeenCalledWith("/workspace/demo");
    actions.onRefresh();
    await flushPromises();
    expect(refresh).toHaveBeenCalled();

    actions.onNewFile();
    const request = lastDialogRequest();
    expect(request["description"]).toBeUndefined();
    (request["onSubmit"] as (name: string) => void)("a.ts");
    await flushPromises();
    const call = testState.commandCalls.find((entry) => entry.label === "createEntry");
    expect((call!.input as { input: { relativePath: string } }).input.relativePath).toBe("a.ts");
  });

  it("uploads into the workspace root from the background menu", async () => {
    testState.environmentHttpBaseUrl = "http://127.0.0.1:4100";
    testState.commandResults["createUploadUrl"] = {
      _tag: "Success",
      value: { relativeUrl: "/api/transfers/u.k", expiresAt: 1, maxBytes: 1024 },
    };
    const uploadFile = vi.fn(async () => ({ status: 201, body: '{"relativePath":"a.txt"}' }));
    vi.stubGlobal("window", {
      desktopBridge: {
        pickFolder: vi.fn(async () => null),
        pickFiles: vi.fn(async () => ["/home/me/a.txt"]),
        uploadFile,
      },
    });
    harness.seedState((initial) => initial === null, null);
    harness.seedState((initial) => initial === null, { x: 5, y: 6 });
    renderPanel();

    const menus = ui.filter("FileTreeContextMenu");
    const background = menus[menus.length - 1]!;
    const actions = background["actions"] as { onUpload: () => void };
    actions.onUpload();
    await flushPromises();

    const minted = testState.commandCalls.find((call) => call.label === "createUploadUrl");
    expect(
      (minted!.input as { input: { relativeDirectory: string } }).input.relativeDirectory,
    ).toBe("");
    expect(uploadFile).toHaveBeenCalledWith({
      url: "http://127.0.0.1:4100/api/transfers/u.k",
      path: "/home/me/a.txt",
    });
  });

  it("keeps copy and refresh while removing every file mutation action", () => {
    const reason = "Workspace unavailable. Retry detection or remove it from BiBCode.";
    harness.seedState((initial) => initial === null, null);
    harness.seedState((initial) => initial === null, { x: 5, y: 6 });
    renderPanel(baseProps({ workspaceUnavailable: reason }));

    const menus = ui.filter("FileTreeContextMenu");
    const background = menus[menus.length - 1]!;
    const actions = background["actions"] as Record<string, unknown>;
    expect(actions["onCopyPath"]).toEqual(expect.any(Function));
    expect(actions["onRefresh"]).toEqual(expect.any(Function));
    expect(actions["onNewFile"]).toBeUndefined();
    expect(actions["onNewFolder"]).toBeUndefined();
  });
});
