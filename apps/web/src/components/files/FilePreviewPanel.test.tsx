import { EnvironmentId, ThreadId, type ResolvedKeybindingsConfig } from "@bibcode/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

/**
 * Instrumented replacements for the stateful React hooks. The panel is rendered
 * once per scenario with `renderToStaticMarkup`; state can be seeded
 * per-scenario, setter calls are recorded (functional updaters are executed
 * against the rendered value so their bodies run), and effects are captured so
 * tests can run them against fake DOM containers.
 */
const harness = vi.hoisted(() => {
  type Matcher = (initial: unknown) => boolean;

  const state = {
    stateSeeds: [] as Array<{ match: Matcher; value: unknown }>,
    setStateCalls: [] as Array<{ initial: unknown; next: unknown; applied: unknown }>,
    effects: [] as Array<() => void | (() => void)>,
    refs: [] as Array<{ current: unknown }>,
    reset() {
      state.stateSeeds.length = 0;
      state.setStateCalls.length = 0;
      state.effects.length = 0;
      state.refs.length = 0;
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

/** Registry of rendered component props so tests can look up and invoke handlers. */
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
    filter(kind: string, predicate?: (props: Record<string, unknown>) => boolean) {
      return registry.entries
        .filter((entry) => entry.kind === kind && (predicate?.(entry.props) ?? true))
        .map((entry) => entry.props);
    },
    find(kind: string, predicate?: (props: Record<string, unknown>) => boolean) {
      const found = registry.entries.find(
        (entry) => entry.kind === kind && (predicate?.(entry.props) ?? true),
      )?.props;
      if (!found) throw new Error(`No recorded "${kind}" element matched`);
      return found;
    },
    byLabel(kind: string, label: string) {
      return registry.find(kind, (props) => props["aria-label"] === label);
    },
  };
  return registry;
});

const testState = vi.hoisted(() => {
  const state = {
    sessionSnapshot: {
      save: { phase: "clean", canSave: false, confirmedRevision: 0 },
      canUndo: false,
      canRedo: false,
    },
    session: null as null | {
      relativePath: string;
      cacheKey: string;
      editor: {
        options: {
          onAttach?: () => void;
          onChange: (file: { contents: string }, annotations?: unknown) => void;
        };
        canUndo: boolean;
        canRedo: boolean;
        undo: ReturnType<typeof vi.fn>;
        redo: ReturnType<typeof vi.fn>;
        cleanUp: ReturnType<typeof vi.fn>;
      };
      subscribe: (listener: () => void) => () => void;
      getSnapshot: () => unknown;
      flush: ReturnType<typeof vi.fn>;
      settle: ReturnType<typeof vi.fn>;
      undo: ReturnType<typeof vi.fn>;
      redo: ReturnType<typeof vi.fn>;
      setEditorChangeHandler: ReturnType<typeof vi.fn>;
      changeOutsideEditor: ReturnType<typeof vi.fn>;
    },
    editingSessions: null as unknown as {
      getOrCreate: ReturnType<typeof vi.fn>;
      beginPathMutation: ReturnType<typeof vi.fn>;
      reset: () => void;
    },
    sessionCreations: [] as string[],
    commands: {
      writeFile: { label: "writeFile" },
      openPreview: { label: "openPreview" },
      createAssetUrl: { label: "createAssetUrl" },
    },
    writeFile: vi.fn<(input: unknown) => Promise<unknown>>(),
    openPreview: vi.fn<(input: unknown) => Promise<unknown>>(),
    createAssetUrl: vi.fn<(input: unknown) => Promise<unknown>>(),
    openFileInPreview: vi.fn<(input: unknown) => Promise<{ _tag: string }>>(),
    isBrowserPreviewFile: vi.fn<(path: string) => boolean>(),
    isPreviewSupported: false,
    fileQuery: { data: null, error: null, isPending: false, refresh: vi.fn() } as {
      data: {
        relativePath: string;
        contents: string;
        byteLength: number;
        truncated: boolean;
      } | null;
      error: string | null;
      isPending: boolean;
      refresh: () => void;
    },
    getLocalStorageItem: vi.fn<(key: string) => unknown>(),
    setLocalStorageItem: vi.fn<(key: string, value: unknown) => void>(),
    primaryEnvironmentId: null as string | null,
    environmentHttpBaseUrl: null as string | null,
    wordWrap: true,
    addReviewComment: vi.fn(),
    removeReviewComment: vi.fn(),
    setProjectFileQueryData: vi.fn(),
    confirmProjectFileQueryData: vi.fn(),
    getOptimisticProjectFileQueryData: vi.fn<() => { contents: string } | null>(),
    installFileEditorDismissal: vi.fn<(input: unknown) => () => void>(),
    toastAdd: vi.fn(),
    coordinators: [] as Array<{
      options: {
        debounceMs: number;
        persist: (contents: string) => Promise<unknown>;
        onPendingChange: (pending: boolean) => void;
        onConfirmed: (contents: string) => void;
      };
      change: ReturnType<typeof vi.fn>;
      dispose: ReturnType<typeof vi.fn>;
      flush: ReturnType<typeof vi.fn>;
      hasPendingWork: ReturnType<typeof vi.fn>;
      settle: ReturnType<typeof vi.fn>;
      subscribe: (listener: () => void) => () => void;
      getSnapshot: () => unknown;
    }>,
    pendingWork: false,
    flushResult: "saved" as string,
    editors: [] as Array<{
      options: {
        onAttach?: () => void;
        onChange: (file: { contents: string }, annotations?: unknown) => void;
      };
      canUndo: boolean;
      canRedo: boolean;
      undo: ReturnType<typeof vi.fn>;
      redo: ReturnType<typeof vi.fn>;
      cleanUp: ReturnType<typeof vi.fn>;
    }>,
  };
  const sessions = new Map<string, NonNullable<typeof state.session>>();
  state.editingSessions = {
    getOrCreate: vi.fn((relativePath: string, create: () => any) => {
      const existing = sessions.get(relativePath);
      if (existing) {
        state.session = existing;
        return existing;
      }
      const session = create() as NonNullable<typeof state.session>;
      state.sessionCreations.push(relativePath);
      session.flush = vi.fn(session.flush.bind(session));
      session.undo = vi.fn(session.undo.bind(session));
      session.redo = vi.fn(session.redo.bind(session));
      session.setEditorChangeHandler = vi.fn(session.setEditorChangeHandler.bind(session));
      session.changeOutsideEditor = vi.fn(session.changeOutsideEditor.bind(session));
      sessions.set(relativePath, session);
      state.session = session;
      return session;
    }),
    beginPathMutation: vi.fn(
      async (request: {
        kind: "rename" | "delete" | "duplicate";
        fromRelativePath?: string;
        toRelativePath?: string;
        relativePath?: string;
      }) => {
        const scopePaths =
          request.kind === "rename"
            ? [request.fromRelativePath!, request.toRelativePath!]
            : [request.relativePath!];
        const results = await Promise.all(
          [...sessions.entries()]
            .filter(([candidate]) =>
              scopePaths.some(
                (relativePath) =>
                  candidate === relativePath || candidate.startsWith(`${relativePath}/`),
              ),
            )
            .map(([, session]) => session.settle()),
        );
        return results.every((result) => result !== "failed")
          ? {
              commitRename: vi.fn(),
              commitDelete: vi.fn(),
              release: vi.fn(),
            }
          : null;
      },
    ),
  };
  state.editingSessions.reset = () => {
    sessions.clear();
    state.session = null;
    state.sessionCreations.length = 0;
  };
  return state;
});

const pierre = vi.hoisted(() => {
  class VirtualizedFile {
    readonly contents: string;
    readonly height: number;
    readonly linePosition: { top: number; height: number } | null;
    readonly getLinePosition: ReturnType<typeof vi.fn>;
    readonly setSelectedLines: ReturnType<typeof vi.fn>;

    constructor(
      config: {
        contents?: string;
        height?: number;
        linePosition?: { top: number; height: number } | null;
      } = {},
    ) {
      this.contents = config.contents ?? "";
      this.height = config.height ?? 0;
      this.linePosition = config.linePosition ?? null;
      this.getLinePosition = vi.fn(() => this.linePosition);
      this.setSelectedLines = vi.fn();
    }

    get file() {
      return { contents: this.contents };
    }
  }
  return { VirtualizedFile };
});

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

  const useRef = (initial?: unknown) => {
    const ref = { current: initial ?? null };
    harness.refs.push(ref);
    return ref;
  };

  return {
    ...actual,
    useState: useState as typeof actual.useState,
    useEffect: useEffect as typeof actual.useEffect,
    useRef: useRef as typeof actual.useRef,
  };
});

vi.mock("@pierre/diffs", () => ({
  VirtualizedFile: pierre.VirtualizedFile,
}));

vi.mock("@pierre/diffs/edit", () => ({
  Editor: class {
    readonly options: {
      onAttach?: () => void;
      onChange: (file: { contents: string }, annotations?: unknown) => void;
    };
    canUndo = testState.sessionSnapshot.canUndo;
    canRedo = testState.sessionSnapshot.canRedo;
    readonly undo = vi.fn();
    readonly redo = vi.fn();
    readonly cleanUp = vi.fn();
    constructor(options: {
      onAttach?: () => void;
      onChange: (file: { contents: string }, annotations?: unknown) => void;
    }) {
      this.options = options;
      testState.editors.push(this);
    }
  },
}));

vi.mock("@pierre/diffs/react", () => ({
  EditProvider: ({
    children,
    ...props
  }: { children?: React.ReactNode } & Record<string, unknown>) => {
    ui.record("EditProvider", props);
    return <div>{children}</div>;
  },
  Virtualizer: ({ children, className }: { children?: React.ReactNode; className?: string }) => (
    <div className={className}>{children}</div>
  ),
  File: (props: Record<string, unknown>) => {
    ui.record("File", props);
    return <div data-file={(props.file as { name: string }).name} />;
  },
}));

vi.mock("@bibcode/client-runtime/state/runtime", () => ({
  isAtomCommandInterrupted: (result: { _tag: string }) => result._tag === "Interrupted",
  squashAtomCommandFailure: (result: { error?: unknown }) => result.error,
}));

vi.mock("~/browser/openFileInPreview", () => ({
  isBrowserPreviewFile: (path: string) => testState.isBrowserPreviewFile(path),
  openFileInPreview: (input: unknown) => testState.openFileInPreview(input),
}));

vi.mock("~/components/ChatMarkdown", () => ({
  default: (props: Record<string, unknown>) => {
    ui.record("ChatMarkdown", props);
    return <div data-chat-markdown>{props.text as string}</div>;
  },
}));

vi.mock("~/components/chat/OpenInPicker", () => ({
  OpenInPicker: (props: Record<string, unknown>) => {
    ui.record("OpenInPicker", props);
    return <div data-open-in-picker />;
  },
}));

vi.mock("~/hooks/useSettings", () => ({
  useClientSettings: (selector: (settings: { wordWrap: boolean }) => unknown) =>
    selector({ wordWrap: testState.wordWrap }),
}));

vi.mock("~/hooks/useTheme", () => ({
  useTheme: () => ({ resolvedTheme: "dark" as const }),
}));

vi.mock("~/hooks/useLocalStorage", () => ({
  getLocalStorageItem: (key: string) => testState.getLocalStorageItem(key),
  setLocalStorageItem: (key: string, value: unknown) => testState.setLocalStorageItem(key, value),
}));

vi.mock("~/previewStateStore", () => ({
  isPreviewSupportedInRuntime: () => testState.isPreviewSupported,
  canPreviewAnyUrlInRuntime: () => testState.isPreviewSupported,
}));

vi.mock("~/terminal-links", () => ({
  resolvePathLinkTarget: (rawPath: string, cwd: string) => `${cwd}/${rawPath}`,
}));

vi.mock("~/components/ui/scroll-area", () => ({
  ScrollArea: ({ children }: { children?: React.ReactNode }) => <div>{children}</div>,
}));

vi.mock("~/components/ui/toggle", () => ({
  Toggle: ({ children, ...props }: Record<string, unknown> & { children?: React.ReactNode }) => {
    ui.record("Toggle", { children, ...props });
    return (
      <button
        type="button"
        aria-label={props["aria-label"] as string | undefined}
        data-pressed={String(Boolean(props.pressed))}
      >
        {children}
      </button>
    );
  },
}));

vi.mock("~/components/ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  TooltipTrigger: ({
    render,
    children,
  }: {
    render?: React.ReactNode;
    children?: React.ReactNode;
  }) => (
    <span>
      {render}
      {children}
    </span>
  ),
  TooltipPopup: ({ children }: { children?: React.ReactNode }) => <div>{children}</div>,
}));

vi.mock("~/components/ui/toast", () => ({
  stackedThreadToast: (options: Record<string, unknown>) => ({ stacked: true, ...options }),
  toastManager: { add: (toast: unknown) => testState.toastAdd(toast) },
}));

vi.mock("~/composerDraftStore", () => ({
  useComposerDraftStore: (
    selector: (store: { addReviewComment: unknown; removeReviewComment: unknown }) => unknown,
  ) =>
    selector({
      addReviewComment: testState.addReviewComment,
      removeReviewComment: testState.removeReviewComment,
    }),
}));

vi.mock("~/reviewCommentContext", () => ({
  buildFileReviewComment: (input: Record<string, unknown>) => ({
    tag: "file-review-comment",
    ...input,
  }),
}));

vi.mock("~/state/assets", () => ({
  assetEnvironment: { createUrl: testState.commands.createAssetUrl },
}));

vi.mock("~/state/environments", () => ({
  usePrimaryEnvironmentId: () => testState.primaryEnvironmentId,
  useEnvironmentHttpBaseUrl: () => testState.environmentHttpBaseUrl,
}));

vi.mock("~/state/preview", () => ({
  previewEnvironment: { open: testState.commands.openPreview },
}));

vi.mock("~/state/projects", () => ({
  projectEnvironment: { writeFile: testState.commands.writeFile },
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: (command: unknown) =>
    command === testState.commands.writeFile ? testState.writeFile : testState.openPreview,
}));

vi.mock("~/state/use-atom-query-runner", () => ({
  useAtomQueryRunner: () => testState.createAssetUrl,
}));

vi.mock("./FileBrowserPanel", () => ({
  default: (props: Record<string, unknown>) => {
    ui.record("FileBrowserPanel", props);
    return <div data-file-browser />;
  },
}));

vi.mock("./FileEditorToolbar", () => ({
  FileEditorToolbar: (props: Record<string, unknown>) => {
    ui.record("FileEditorToolbar", props);
    return <div data-file-editor-toolbar />;
  },
}));

vi.mock("./fileEditorDismissal", () => ({
  installFileEditorDismissal: (input: unknown) => testState.installFileEditorDismissal(input),
}));

vi.mock("./LocalCommentAnnotation", () => ({
  LocalCommentAnnotation: (props: Record<string, unknown>) => {
    ui.record("LocalCommentAnnotation", props);
    return (
      <div data-local-comment={props.kind as string}>
        {props.rangeLabel as string}: {props.text as string}
      </div>
    );
  },
}));

vi.mock("./fileSaveCoordinator", () => ({
  FileSaveCoordinator: class {
    readonly options: {
      debounceMs: number;
      persist: (contents: string) => Promise<unknown>;
      onPendingChange: (pending: boolean) => void;
      onConfirmed: (contents: string) => void;
    };
    readonly listeners = new Set<() => void>();
    readonly change = vi.fn(() => {
      for (const listener of this.listeners) listener();
    });
    readonly dispose = vi.fn();
    readonly flush = vi.fn(() => Promise.resolve(testState.flushResult));
    readonly hasPendingWork = vi.fn(() => testState.pendingWork);
    readonly settle = vi.fn(() => Promise.resolve(testState.flushResult));
    readonly subscribe = (listener: () => void) => {
      this.listeners.add(listener);
      return () => this.listeners.delete(listener);
    };
    readonly getSnapshot = () => testState.sessionSnapshot.save;
    constructor(options: {
      debounceMs: number;
      persist: (contents: string) => Promise<unknown>;
      onPendingChange: (pending: boolean) => void;
      onConfirmed: (contents: string) => void;
    }) {
      this.options = options;
      testState.coordinators.push(this);
    }
  },
}));

vi.mock("./projectFilesQueryState", () => ({
  useProjectFileQuery: () => testState.fileQuery,
  setProjectFileQueryData: (...args: Array<unknown>) => testState.setProjectFileQueryData(...args),
  confirmProjectFileQueryData: (...args: Array<unknown>) =>
    testState.confirmProjectFileQueryData(...args),
  getOptimisticProjectFileQueryData: () => testState.getOptimisticProjectFileQueryData(),
}));

import { resolveDiffThemeName } from "~/lib/diffRendering";

import FilePreviewPanel from "./FilePreviewPanel";

const environmentId = EnvironmentId.make("environment-1");
const otherEnvironmentId = EnvironmentId.make("environment-2");
const threadRef = { environmentId, threadId: ThreadId.make("thread-1") };

type PanelProps = Parameters<typeof FilePreviewPanel>[0];

function baseProps(overrides: Partial<PanelProps> = {}): PanelProps {
  return {
    environmentId,
    cwd: "/workspace/demo",
    projectName: "demo",
    relativePath: "src/app.ts",
    threadRef,
    composerDraftTarget: threadRef,
    keybindings: [] as ResolvedKeybindingsConfig,
    availableEditors: [],
    revealLine: null,
    revealRequestId: 1,
    onOpenFile: vi.fn(),
    onPendingChange: vi.fn(),
    editingSessions: testState.editingSessions,
    ...overrides,
  };
}

function setFileData(contents: string, options: { truncated?: boolean; byteLength?: number } = {}) {
  testState.fileQuery = {
    data: {
      relativePath: "src/app.ts",
      contents,
      byteLength: options.byteLength ?? contents.length,
      truncated: options.truncated ?? false,
    },
    error: null,
    isPending: false,
    refresh: vi.fn(),
  };
}

function renderPanel(props: PanelProps): string {
  ui.reset();
  harness.setStateCalls.length = 0;
  harness.effects.length = 0;
  harness.refs.length = 0;
  return renderToStaticMarkup(<FilePreviewPanel {...props} />);
}

interface FakeRevealElement {
  readonly attributes: Set<string>;
  setAttribute: (name: string) => void;
  removeAttribute: (name: string) => void;
}

function fakeRevealElement(): FakeRevealElement {
  const attributes = new Set<string>();
  return {
    attributes,
    setAttribute: (name: string) => attributes.add(name),
    removeAttribute: (name: string) => attributes.delete(name),
  };
}

function fakeScrollContainer() {
  return {
    scrollTop: 50,
    clientHeight: 400,
    scrollHeight: 2000,
    getBoundingClientRect: () => ({ top: 60 }),
  };
}

function fakeFileContainer(options: {
  scrollContainer?: ReturnType<typeof fakeScrollContainer> | null;
  useShadowRoot?: boolean;
}) {
  const lineElement = fakeRevealElement();
  const columnElement = fakeRevealElement();
  const previouslyMarked = [fakeRevealElement()];
  previouslyMarked[0]!.attributes.add("data-file-link-reveal");
  const root = {
    querySelectorAll: () => previouslyMarked,
    querySelector: (selector: string) =>
      selector.includes("data-line") ? lineElement : columnElement,
  };
  const container = {
    shadowRoot: options.useShadowRoot ? root : null,
    querySelectorAll: root.querySelectorAll,
    querySelector: root.querySelector,
    style: {} as Record<string, string>,
    closest: () => options.scrollContainer ?? null,
    isConnected: true,
    getBoundingClientRect: () => ({ top: 100 }),
  };
  return { container, lineElement, columnElement, previouslyMarked };
}

type PostRender = (container: unknown, instance: unknown, phase: string) => void;

async function flushPromises(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

let rafCallbacks: Array<() => void>;
let cancelledFrames: Array<number>;

beforeEach(() => {
  harness.reset();
  ui.reset();
  testState.writeFile.mockReset().mockResolvedValue({ _tag: "Success" });
  testState.openPreview.mockReset().mockResolvedValue({ _tag: "Success" });
  testState.createAssetUrl.mockReset().mockResolvedValue({ _tag: "Success" });
  testState.openFileInPreview.mockReset().mockResolvedValue({ _tag: "Success" });
  testState.isBrowserPreviewFile.mockReset().mockReturnValue(false);
  testState.isPreviewSupported = false;
  testState.fileQuery = { data: null, error: null, isPending: true, refresh: vi.fn() };
  testState.getLocalStorageItem.mockReset().mockReturnValue(null);
  testState.setLocalStorageItem.mockReset();
  testState.primaryEnvironmentId = null;
  testState.environmentHttpBaseUrl = null;
  testState.wordWrap = true;
  testState.addReviewComment.mockReset();
  testState.removeReviewComment.mockReset();
  testState.setProjectFileQueryData.mockReset();
  testState.confirmProjectFileQueryData.mockReset();
  testState.getOptimisticProjectFileQueryData.mockReset().mockReturnValue(null);
  testState.installFileEditorDismissal.mockReset().mockReturnValue(vi.fn());
  testState.toastAdd.mockReset();
  testState.sessionSnapshot = {
    save: { phase: "clean", canSave: false, confirmedRevision: 0 },
    canUndo: false,
    canRedo: false,
  };
  testState.editingSessions.reset();
  testState.editingSessions.getOrCreate.mockClear();
  testState.editingSessions.beginPathMutation.mockClear();
  testState.coordinators.length = 0;
  testState.pendingWork = false;
  testState.flushResult = "saved";
  testState.editors.length = 0;

  rafCallbacks = [];
  cancelledFrames = [];
  vi.stubGlobal("requestAnimationFrame", (callback: () => void) => {
    rafCallbacks.push(callback);
    return rafCallbacks.length;
  });
  vi.stubGlobal("cancelAnimationFrame", (frameId: number) => {
    cancelledFrames.push(frameId);
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("panel layout states", () => {
  it("shows the file browser full-width when no file is selected", () => {
    const markup = renderPanel(baseProps({ relativePath: null }));
    expect(markup).toContain("data-file-browser");
    expect(markup).not.toContain("data-surface-subheader");
    expect(markup).toContain("min-w-0 flex-1");
  });

  it("shows a spinner while the file loads", () => {
    const markup = renderPanel(baseProps());
    expect(markup).toContain("animate-spin");
  });

  it("renders the dedicated toolbar below breadcrumbs for a selected file", () => {
    setFileData("const value = 1;\n");
    const markup = renderPanel(baseProps());
    expect(markup.indexOf("data-file-breadcrumbs")).toBeLessThan(
      markup.indexOf("data-file-editor-toolbar"),
    );
    expect(markup.indexOf("data-file-editor-toolbar")).toBeLessThan(
      markup.indexOf('data-file="src/app.ts"'),
    );
  });

  it("maps the active editing session snapshot into toolbar actions", () => {
    setFileData("const value = 1;\n");
    testState.sessionSnapshot = {
      save: { phase: "pending", canSave: true, confirmedRevision: 0 },
      canUndo: true,
      canRedo: false,
    };
    renderPanel(baseProps());

    const toolbar = ui.find("FileEditorToolbar");
    expect(toolbar).toMatchObject({
      savePhase: "pending",
      canSave: true,
      canUndo: true,
      canRedo: false,
    });
    (toolbar.onSave as () => void)();
    (toolbar.onUndo as () => void)();
    expect(testState.session!.flush).toHaveBeenCalledOnce();
    expect(testState.session!.undo).toHaveBeenCalledOnce();
  });

  it.each(["loading", "error", "truncated"] as const)(
    "keeps unavailable controls visible for the %s state",
    (state) => {
      if (state === "error") {
        testState.fileQuery = {
          data: null,
          error: "unavailable",
          isPending: false,
          refresh: vi.fn(),
        };
      } else if (state === "truncated") {
        setFileData("partial", { truncated: true, byteLength: 2_000_000 });
      }
      renderPanel(baseProps());
      expect(ui.find("FileEditorToolbar")).toMatchObject({
        canSave: false,
        canUndo: false,
        canRedo: false,
      });
    },
  );

  it("shows the error message when reading failed", () => {
    testState.fileQuery = {
      data: null,
      error: "File is too large to preview.",
      isPending: false,
      refresh: vi.fn(),
    };
    const markup = renderPanel(baseProps());
    expect(markup).toContain("File is too large to preview.");
    expect(markup).toContain("text-destructive");
  });

  it("renders breadcrumbs and an editable surface for a loaded file", () => {
    setFileData("const value = 1;\n");
    const markup = renderPanel(baseProps());

    expect(markup).toContain(">demo</span>");
    expect(markup).toContain(">src</span>");
    expect(markup).toContain(">app.ts</span>");
    expect(markup).toContain('data-current-file-crumb="true"');
    expect(markup).toContain('data-file="src/app.ts"');
    expect(markup).not.toContain("data-open-in-picker");

    const file = ui.find("File");
    const options = file.options as Record<string, unknown>;
    expect(options.disableFileHeader).toBe(true);
    expect(options.overflow).toBe("wrap");
    expect(options.theme).toBe(resolveDiffThemeName("dark"));
    expect(options.themeType).toBe("dark");
    expect(file.edit).toBe(true);
    expect(ui.find("FileEditorToolbar").markdownView).toBeUndefined();
  });

  it("respects the word wrap setting", () => {
    testState.wordWrap = false;
    setFileData("const value = 1;\n");
    renderPanel(baseProps());
    expect((ui.find("File").options as { overflow: string }).overflow).toBe("scroll");
  });

  it("shows the truncation banner and a read-only file for truncated content", () => {
    setFileData("partial contents", { truncated: true, byteLength: 2_000_000 });
    const markup = renderPanel(baseProps());
    expect(markup).toContain("Preview limited to the first 1 MB of a");
    expect(markup).toContain("byte file.");
    const file = ui.find("File");
    expect(file.edit).toBeUndefined();
    expect(file.selectedLines).toBeUndefined();
  });

  it("shows the open-in picker only for the primary environment", () => {
    setFileData("body {}\n");
    testState.primaryEnvironmentId = environmentId;
    expect(renderPanel(baseProps())).toContain("data-open-in-picker");

    testState.primaryEnvironmentId = otherEnvironmentId;
    expect(renderPanel(baseProps())).not.toContain("data-open-in-picker");
  });

  it("maps saving state into the dedicated toolbar", () => {
    setFileData("saved contents");
    testState.sessionSnapshot = {
      save: { phase: "saving", canSave: false, confirmedRevision: 0 },
      canUndo: false,
      canRedo: false,
    };
    renderPanel(baseProps());
    expect(ui.find("FileEditorToolbar")).toMatchObject({
      savePhase: "saving",
      canSave: false,
    });
  });
});

describe("file explorer toggle", () => {
  it("defaults open, persists toggling, and logs persistence failures", () => {
    setFileData("contents");
    const markup = renderPanel(baseProps());
    expect(markup).toContain("data-file-browser");
    expect(markup).toContain('aria-label="Hide file explorer"');

    const toggle = ui.byLabel("Toggle", "Hide file explorer");
    (toggle.onPressedChange as () => void)();
    expect(testState.setLocalStorageItem).toHaveBeenCalledWith("bibcode.fileExplorerOpen", false);

    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    testState.setLocalStorageItem.mockImplementation(() => {
      throw new Error("quota exceeded");
    });
    (toggle.onPressedChange as () => void)();
    expect(errorSpy).toHaveBeenCalled();
    errorSpy.mockRestore();
  });

  it("honors a persisted closed state", () => {
    setFileData("contents");
    testState.getLocalStorageItem.mockReturnValue(false);
    const markup = renderPanel(baseProps());
    expect(markup).not.toContain("data-file-browser");
    expect(markup).toContain('aria-label="Show file explorer"');
  });

  it("falls back to open when reading the persisted state fails", () => {
    setFileData("contents");
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    testState.getLocalStorageItem.mockImplementation(() => {
      throw new Error("bad storage");
    });
    const markup = renderPanel(baseProps());
    expect(errorSpy).toHaveBeenCalled();
    expect(markup).toContain("data-file-browser");
    errorSpy.mockRestore();
  });
});

describe("markdown preview", () => {
  it("renders markdown formatted by default and offers the source toggle", () => {
    setFileData("# Title\n");
    const markup = renderPanel(baseProps({ relativePath: "README.md" }));
    expect(markup).not.toContain('aria-label="Show markdown source"');
    expect(ui.filter("ChatMarkdown")).toHaveLength(1);

    const markdownView = ui.find("FileEditorToolbar").markdownView as {
      rendered: boolean;
      onRenderedChange: (rendered: boolean) => void;
    };
    expect(markdownView.rendered).toBe(true);
    markdownView.onRenderedChange(false);
    expect(
      harness.setStateCalls.some(
        (call) =>
          typeof call.next === "object" &&
          call.next !== null &&
          (call.next as { path: string | null }).path === "README.md",
      ),
    ).toBe(true);

    markdownView.onRenderedChange(true);
    expect(
      harness.setStateCalls.some(
        (call) =>
          typeof call.next === "object" &&
          call.next !== null &&
          (call.next as { path: string | null }).path === null,
      ),
    ).toBe(true);
  });

  it("renders markdown and applies task list toggles", async () => {
    testState.sessionSnapshot = {
      save: { phase: "pending", canSave: true, confirmedRevision: 0 },
      canUndo: true,
      canRedo: true,
    };
    setFileData("- [ ] first\n- [x] second\n");
    const markup = renderPanel(baseProps({ relativePath: "README.md" }));
    expect(markup).toContain("data-chat-markdown");
    expect(ui.find("FileEditorToolbar")).toMatchObject({
      canSave: true,
      canUndo: false,
      canRedo: false,
      markdownView: { rendered: true },
    });

    const chatMarkdown = ui.find("ChatMarkdown");
    const onTaskListChange = chatMarkdown.onTaskListChange as (input: {
      markerOffset: number;
      checked: boolean;
    }) => void;

    // Checking the first task rewrites the document and schedules a save.
    onTaskListChange({ markerOffset: 2, checked: true });
    expect(testState.setProjectFileQueryData).toHaveBeenCalledWith(
      environmentId,
      "/workspace/demo",
      "README.md",
      "- [x] first\n- [x] second\n",
    );
    expect(testState.session!.changeOutsideEditor).toHaveBeenCalledWith(
      "- [x] first\n- [x] second\n",
    );
    const coordinator = testState.coordinators[testState.coordinators.length - 1]!;
    expect(coordinator.change).toHaveBeenCalledWith("- [x] first\n- [x] second\n");

    // A stale marker offset leaves the document untouched.
    testState.setProjectFileQueryData.mockReset();
    coordinator.change.mockReset();
    onTaskListChange({ markerOffset: 0, checked: true });
    expect(testState.setProjectFileQueryData).not.toHaveBeenCalled();
    expect(coordinator.change).not.toHaveBeenCalled();

    // Optimistic contents win over the query snapshot.
    testState.getOptimisticProjectFileQueryData.mockReturnValue({
      contents: "- [x] first\n- [x] second\n",
    });
    onTaskListChange({ markerOffset: 2, checked: false });
    expect(testState.setProjectFileQueryData).toHaveBeenCalledWith(
      environmentId,
      "/workspace/demo",
      "README.md",
      "- [ ] first\n- [x] second\n",
    );
    await flushPromises();
  });

  it("keeps the source view when a reveal targets a different request", () => {
    setFileData("# Title\n");
    harness.seedState(
      (initial) =>
        typeof initial === "object" &&
        initial !== null &&
        "path" in initial &&
        "revealRequestId" in initial,
      { path: "README.md", revealRequestId: 0 },
    );
    renderPanel(baseProps({ relativePath: "README.md", revealLine: 3, revealRequestId: 1 }));
    expect(ui.filter("ChatMarkdown")).toHaveLength(0);
    expect(ui.filter("File")).toHaveLength(1);
  });
});

describe("open in preview browser", () => {
  function browserProps() {
    return baseProps({ relativePath: "index.html" });
  }

  beforeEach(() => {
    setFileData("<html></html>");
    testState.isPreviewSupported = true;
    testState.isBrowserPreviewFile.mockReturnValue(true);
    testState.environmentHttpBaseUrl = "http://127.0.0.1:4100";
  });

  it("opens the file through the preview flow", async () => {
    const markup = renderPanel(browserProps());
    expect(markup).toContain('aria-label="Open file in preview browser"');

    const toggle = ui.byLabel("Toggle", "Open file in preview browser");
    (toggle.onPressedChange as () => void)();
    await flushPromises();
    expect(testState.openFileInPreview).toHaveBeenCalledWith({
      threadRef,
      filePath: "/workspace/demo/index.html",
      httpBaseUrl: "http://127.0.0.1:4100",
      createAssetUrl: testState.createAssetUrl,
      openPreview: testState.openPreview,
    });
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });

  it("reports failures with a stacked toast and ignores interrupts", async () => {
    testState.openFileInPreview.mockResolvedValue({
      _tag: "Failure",
      error: new Error("no preview server"),
    } as never);
    renderPanel(browserProps());
    (ui.byLabel("Toggle", "Open file in preview browser").onPressedChange as () => void)();
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith({
      stacked: true,
      type: "error",
      title: "Unable to open file in browser",
      description: "no preview server",
    });

    testState.toastAdd.mockReset();
    testState.openFileInPreview.mockResolvedValue({ _tag: "Failure", error: "nope" } as never);
    renderPanel(browserProps());
    (ui.byLabel("Toggle", "Open file in preview browser").onPressedChange as () => void)();
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith({
      stacked: true,
      type: "error",
      title: "Unable to open file in browser",
      description: "An error occurred.",
    });

    testState.toastAdd.mockReset();
    testState.openFileInPreview.mockResolvedValue({ _tag: "Interrupted" });
    renderPanel(browserProps());
    (ui.byLabel("Toggle", "Open file in preview browser").onPressedChange as () => void)();
    await flushPromises();
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });

  it("does nothing without an environment base URL", () => {
    testState.environmentHttpBaseUrl = null;
    renderPanel(browserProps());
    (ui.byLabel("Toggle", "Open file in preview browser").onPressedChange as () => void)();
    expect(testState.openFileInPreview).not.toHaveBeenCalled();
  });

  it("hides the action when the runtime does not support previews", () => {
    testState.isPreviewSupported = false;
    const markup = renderPanel(browserProps());
    expect(markup).not.toContain('aria-label="Open file in preview browser"');
  });
});

describe("file line reveal", () => {
  function revealProps() {
    return baseProps({ revealLine: 99, revealRequestId: 7 });
  }

  function renderTruncated(props = revealProps()) {
    setFileData("one\ntwo\r\nthree\rfour", { truncated: true, byteLength: 5_000_000 });
    renderPanel(props);
    return (ui.find("File").options as { onPostRender: PostRender }).onPostRender;
  }

  it("marks the clamped reveal line and skips non-virtualized instances", () => {
    const onPostRender = renderTruncated();
    const { container, lineElement, columnElement, previouslyMarked } = fakeFileContainer({});

    onPostRender(container, { file: { contents: "one\ntwo" } }, "mount");
    expect(previouslyMarked[0]!.attributes.size).toBe(0);
    expect(lineElement.attributes.has("data-file-link-reveal")).toBe(true);
    expect(columnElement.attributes.has("data-file-link-reveal")).toBe(true);
  });

  it("scrolls the virtualizer to center the reveal line once per request", () => {
    const onPostRender = renderTruncated();
    const scrollContainer = fakeScrollContainer();
    const { container } = fakeFileContainer({ scrollContainer, useShadowRoot: true });
    const instance = new pierre.VirtualizedFile({
      contents: "one\ntwo\r\nthree\rfour",
      height: 800,
      linePosition: { top: 500, height: 20 },
    });

    onPostRender(container, instance, "mount");
    expect(container.style.minHeight).toBe("800px");
    expect(rafCallbacks).toHaveLength(1);

    // While the frame is pending, repeated post-renders do not reschedule.
    onPostRender(container, instance, "update");
    expect(rafCallbacks).toHaveLength(1);

    rafCallbacks[0]!();
    // CRLF/CR aware clamping: 4 lines, so line 99 clamps to 4.
    expect(instance.getLinePosition).toHaveBeenCalledWith(4);
    // fileTop = 50 + 100 - 60 = 90; centered = 90 + 500 - 190 = 400.
    expect(scrollContainer.scrollTop).toBe(400);

    // The request is handled now — no further frames get scheduled.
    onPostRender(container, instance, "update");
    expect(rafCallbacks).toHaveLength(1);
  });

  it("cancels a pending reveal on unmount", () => {
    const onPostRender = renderTruncated();
    const { container } = fakeFileContainer({
      scrollContainer: fakeScrollContainer(),
      useShadowRoot: true,
    });
    const instance = new pierre.VirtualizedFile({ contents: "one", height: 100 });

    onPostRender(container, instance, "mount");
    expect(rafCallbacks).toHaveLength(1);
    onPostRender(container, instance, "unmount");
    expect(cancelledFrames).toEqual([1]);
  });

  it("bails out when the scroll container, connection, or line position is missing", () => {
    const onPostRender = renderTruncated();

    // No `.file-preview-virtualizer` ancestor.
    const detached = fakeFileContainer({ scrollContainer: null });
    const instance = new pierre.VirtualizedFile({ contents: "one", height: 100 });
    onPostRender(detached.container, instance, "mount");
    expect(rafCallbacks).toHaveLength(0);

    // Disconnected container at frame time.
    const disconnected = fakeFileContainer({ scrollContainer: fakeScrollContainer() });
    onPostRender(disconnected.container, instance, "mount");
    disconnected.container.isConnected = false;
    rafCallbacks[0]!();
    expect(instance.getLinePosition).not.toHaveBeenCalled();

    // Connected but the line has no measured position yet.
    const scrollContainer = fakeScrollContainer();
    const unmeasured = fakeFileContainer({ scrollContainer });
    const unmeasuredInstance = new pierre.VirtualizedFile({
      contents: "one",
      height: 100,
      linePosition: null,
    });
    const previousScrollTop = scrollContainer.scrollTop;
    // Fresh render so the previous handled-request bookkeeping does not interfere.
    const freshPostRender = renderTruncated();
    freshPostRender(unmeasured.container, unmeasuredInstance, "mount");
    rafCallbacks[rafCallbacks.length - 1]!();
    expect(scrollContainer.scrollTop).toBe(previousScrollTop);
  });

  it("clears the reveal highlight when no line is targeted", () => {
    setFileData("contents", { truncated: true, byteLength: 5_000_000 });
    renderPanel(baseProps({ revealLine: null }));
    const onPostRender = (ui.find("File").options as { onPostRender: PostRender }).onPostRender;
    const { container, lineElement } = fakeFileContainer({});
    container.style.minHeight = "500px";

    onPostRender(container, new pierre.VirtualizedFile({ contents: "contents" }), "mount");
    expect(container.style.minHeight).toBe("");
    expect(lineElement.attributes.size).toBe(0);
    expect(rafCallbacks).toHaveLength(0);
  });
});

describe("editable file surface", () => {
  const contents = "line one\nline two\nline three\n";

  function renderEditable(props = baseProps()) {
    setFileData(contents);
    return renderPanel(props);
  }

  it("queues editor changes without feeding them back through the query cache", () => {
    renderEditable();
    harness.runEffects();
    const editor = testState.editors[0]!;

    editor.options.onChange({ contents: "updated" }, undefined);
    expect(testState.setProjectFileQueryData).not.toHaveBeenCalled();
    const coordinator = testState.coordinators[testState.coordinators.length - 1]!;
    expect(coordinator.change).toHaveBeenCalledWith("updated");
    expect(testState.addReviewComment).not.toHaveBeenCalled();

    editor.options.onChange({ contents: "updated" }, [
      {
        lineNumber: 3,
        metadata: {
          entries: [
            { id: "comment-1", kind: "comment", startLine: 1, endLine: 3, text: "note" },
            { id: "draft-1", kind: "draft", startLine: 3, endLine: 3, text: "" },
          ],
        },
      },
    ]);
    expect(testState.addReviewComment).toHaveBeenCalledTimes(1);
    expect(testState.addReviewComment).toHaveBeenCalledWith(threadRef, {
      tag: "file-review-comment",
      id: "comment-1",
      filePath: "src/app.ts",
      startLine: 1,
      endLine: 3,
      text: "note",
      contents: "updated",
    });
  });

  it("retains editor history when switching away from and back to a tab", () => {
    setFileData("const a = 1;\n");
    renderPanel(baseProps({ relativePath: "src/a.ts" }));
    const sessionA = testState.session!;
    const editorA = sessionA.editor;

    setFileData("const b = 2;\n");
    renderPanel(baseProps({ relativePath: "src/b.ts" }));
    expect(testState.session).not.toBe(sessionA);

    editorA.canUndo = true;
    editorA.options.onAttach?.();

    setFileData("const a = 1;\n");
    renderPanel(baseProps({ relativePath: "src/a.ts" }));

    expect(testState.sessionCreations.filter((path) => path === "src/a.ts")).toHaveLength(1);
    expect(testState.session).toBe(sessionA);
    expect((ui.find("EditProvider").createEditor as () => unknown)()).toBe(editorA);
    const toolbar = ui.find("FileEditorToolbar");
    expect(toolbar.canUndo).toBe(true);
    (toolbar.onUndo as () => void)();
    expect(editorA.undo).toHaveBeenCalledOnce();
  });

  it("wires the save coordinator to the write command and pending callbacks", async () => {
    const onPendingChange = vi.fn();
    renderEditable(baseProps({ onPendingChange }));
    const coordinator = testState.coordinators[testState.coordinators.length - 1]!;

    coordinator.options.onPendingChange(true);
    expect(onPendingChange).toHaveBeenCalledWith("src/app.ts", true);

    await coordinator.options.persist("persisted");
    expect(testState.writeFile).toHaveBeenCalledWith({
      environmentId,
      input: { cwd: "/workspace/demo", relativePath: "src/app.ts", contents: "persisted" },
    });

    coordinator.options.onConfirmed("persisted");
    expect(testState.setProjectFileQueryData).toHaveBeenCalledWith(
      environmentId,
      "/workspace/demo",
      "src/app.ts",
      "persisted",
    );
    expect(testState.confirmProjectFileQueryData).toHaveBeenCalledWith(
      environmentId,
      "/workspace/demo",
      "src/app.ts",
      "persisted",
    );
  });

  it("starts a comment draft from a line selection", () => {
    renderEditable();
    const file = ui.find("File");
    const options = file.options as Record<string, unknown>;
    expect(options.enableGutterUtility).toBe(true);
    expect(options.enableLineSelection).toBe(true);

    harness.setStateCalls.length = 0;
    (options.onLineSelectionEnd as (range: unknown) => void)({ start: 5, end: 2 });

    const annotationUpdate = harness.setStateCalls.find(
      (call) => typeof call.next === "function" && Array.isArray(call.applied),
    );
    expect(annotationUpdate).toBeDefined();
    const annotations = annotationUpdate!.applied as Array<{
      lineNumber: number;
      metadata: { entries: Array<{ kind: string; startLine: number; endLine: number }> };
    }>;
    expect(annotations).toHaveLength(1);
    expect(annotations[0]!.lineNumber).toBe(5);
    expect(annotations[0]!.metadata.entries[0]).toMatchObject({
      kind: "draft",
      startLine: 2,
      endLine: 5,
    });

    // Clearing the selection does not add a draft.
    harness.setStateCalls.length = 0;
    (options.onLineSelectionEnd as (range: unknown) => void)(null);
    expect(harness.setStateCalls.every((call) => typeof call.next !== "function")).toBe(true);

    // The gutter utility and selection-change callbacks update the selection.
    (options.onGutterUtilityClick as (range: unknown) => void)({ start: 1, end: 1 });
    (options.onLineSelectionChange as (range: unknown) => void)({ start: 1, end: 2 });
    expect(
      harness.setStateCalls.filter(
        (call) =>
          typeof call.next === "object" && call.next !== null && "revealRequestId" in call.next,
      ).length,
    ).toBeGreaterThanOrEqual(2);
  });

  it("appends a draft to an existing annotation group on the same line", () => {
    harness.seedState(
      (initial) => Array.isArray(initial) && initial.length === 0,
      [
        {
          lineNumber: 5,
          metadata: {
            entries: [{ id: "comment-1", kind: "comment", startLine: 4, endLine: 5, text: "hi" }],
          },
        },
      ],
    );
    renderEditable();
    const options = ui.find("File").options as Record<string, unknown>;

    harness.setStateCalls.length = 0;
    (options.onLineSelectionEnd as (range: unknown) => void)({ start: 5, end: 5 });
    const annotationUpdate = harness.setStateCalls.find(
      (call) => typeof call.next === "function" && Array.isArray(call.applied),
    );
    const annotations = annotationUpdate!.applied as Array<{
      lineNumber: number;
      metadata: { entries: Array<{ kind: string }> };
    }>;
    expect(annotations).toHaveLength(1);
    expect(annotations[0]!.metadata.entries.map((entry) => entry.kind)).toEqual([
      "comment",
      "draft",
    ]);
  });

  it("disables gutter interactions while a draft comment is open", () => {
    harness.seedState(
      (initial) => Array.isArray(initial) && initial.length === 0,
      [
        {
          lineNumber: 2,
          metadata: {
            entries: [{ id: "draft-1", kind: "draft", startLine: 2, endLine: 2, text: "" }],
          },
        },
      ],
    );
    renderEditable();
    const options = ui.find("File").options as Record<string, unknown>;
    expect(options.enableGutterUtility).toBe(false);
    expect(options.enableLineSelection).toBe(false);
  });

  it("renders annotations and routes comment actions to the draft store", () => {
    const annotation = {
      lineNumber: 3,
      metadata: {
        entries: [{ id: "comment-1", kind: "comment", startLine: 1, endLine: 3, text: "note" }],
      },
    };
    harness.seedState((initial) => Array.isArray(initial) && initial.length === 0, [annotation]);
    renderEditable();
    const file = ui.find("File");
    expect(file.lineAnnotations).toEqual([annotation]);

    const renderAnnotation = file.renderAnnotation as (input: unknown) => React.ReactElement;
    const markup = renderToStaticMarkup(renderAnnotation(annotation));
    expect(markup).toContain('data-local-comment="comment"');
    expect(markup).toContain("L1 to L3: note");

    const localComment = ui.find("LocalCommentAnnotation");

    // Submitting an existing entry re-issues the review comment with new text.
    harness.setStateCalls.length = 0;
    (localComment.onComment as (text: string) => void)("updated note");
    expect(testState.addReviewComment).toHaveBeenCalledWith(threadRef, {
      tag: "file-review-comment",
      id: "comment-1",
      filePath: "src/app.ts",
      startLine: 1,
      endLine: 3,
      text: "updated note",
      contents,
    });
    const mapped = harness.setStateCalls.find(
      (call) => typeof call.next === "function" && Array.isArray(call.applied),
    );
    expect(
      (mapped!.applied as Array<{ metadata: { entries: Array<{ text: string }> } }>)[0]!.metadata
        .entries[0]!.text,
    ).toBe("updated note");

    // Cancelling removes the entry (and empty annotations vanish).
    harness.setStateCalls.length = 0;
    (localComment.onCancel as () => void)();
    expect(testState.removeReviewComment).toHaveBeenCalledWith(threadRef, "comment-1");
    const removed = harness.setStateCalls.find(
      (call) => typeof call.next === "function" && Array.isArray(call.applied),
    );
    expect(removed!.applied).toEqual([]);

    // Deleting behaves like cancelling.
    testState.removeReviewComment.mockReset();
    (localComment.onDelete as () => void)();
    expect(testState.removeReviewComment).toHaveBeenCalledWith(threadRef, "comment-1");
  });

  it("ignores submissions for unknown annotation entries", () => {
    renderEditable();
    const renderAnnotation = ui.find("File").renderAnnotation as (
      input: unknown,
    ) => React.ReactElement;
    renderToStaticMarkup(
      renderAnnotation({
        lineNumber: 9,
        metadata: {
          entries: [{ id: "ghost", kind: "draft", startLine: 9, endLine: 9, text: "" }],
        },
      }),
    );
    const localComment = ui.find("LocalCommentAnnotation");
    (localComment.onComment as (text: string) => void)("orphan");
    expect(testState.addReviewComment).not.toHaveBeenCalled();
  });

  it("schedules the post-render selection sync and cancels it on unmount", () => {
    renderEditable();
    const options = ui.find("File").options as { onPostRender: PostRender };
    const { container } = fakeFileContainer({});
    const instance = new pierre.VirtualizedFile({ contents });

    options.onPostRender(container, instance, "mount");
    expect(rafCallbacks).toHaveLength(1);
    // A second post-render cancels the previously scheduled frame.
    options.onPostRender(container, instance, "update");
    expect(cancelledFrames).toEqual([1]);
    expect(rafCallbacks).toHaveLength(2);

    rafCallbacks[1]!();
    expect(instance.setSelectedLines).toHaveBeenCalledWith(null, { notify: false });

    // Disconnected containers skip the selection sync.
    container.isConnected = false;
    options.onPostRender(container, instance, "update");
    rafCallbacks[rafCallbacks.length - 1]!();
    expect(instance.setSelectedLines).toHaveBeenCalledTimes(1);

    // Unmount cancels without rescheduling.
    const scheduled = rafCallbacks.length;
    options.onPostRender(container, instance, "unmount");
    expect(rafCallbacks).toHaveLength(scheduled);
  });

  it("passes a matching selection override through to the file", () => {
    harness.seedState((initial) => initial === null, {
      revealRequestId: 1,
      range: { start: 2, end: 4 },
    });
    renderEditable();
    expect(ui.find("File").selectedLines).toEqual({ start: 2, end: 4 });
  });

  it("ignores selection overrides from a previous reveal request", () => {
    harness.seedState((initial) => initial === null, {
      revealRequestId: 0,
      range: { start: 2, end: 4 },
    });
    renderEditable(baseProps({ revealRequestId: 5 }));
    expect(ui.find("File").selectedLines).toBeNull();
  });
});

describe("effects wiring", () => {
  const contents = "alpha\nbeta\n";

  function renderWithEffects() {
    setFileData(contents);
    renderPanel(baseProps());
  }

  function fakeSurface() {
    const listeners = new Map<string, (event: unknown) => void>();
    return {
      element: {
        addEventListener: (type: string, handler: (event: unknown) => void) => {
          listeners.set(type, handler);
        },
        removeEventListener: (type: string) => {
          listeners.delete(type);
        },
      },
      listeners,
    };
  }

  it("scrolls the current breadcrumb into view", () => {
    renderWithEffects();
    const scrollIntoView = vi.fn();
    harness.refs[0]!.current = {
      querySelector: () => ({ scrollIntoView }),
    };
    const cleanups = harness.runEffects();
    expect(scrollIntoView).toHaveBeenCalledWith({ block: "nearest", inline: "end" });
    for (const cleanup of cleanups) cleanup();
  });

  it("detaches surface handlers without disposing the retained session", () => {
    renderWithEffects();
    const cleanups = harness.runEffects();
    for (const cleanup of cleanups) cleanup();
    const coordinator = testState.coordinators[testState.coordinators.length - 1]!;
    expect(testState.session!.setEditorChangeHandler).toHaveBeenLastCalledWith(null);
    expect(coordinator.dispose).not.toHaveBeenCalled();
    expect(testState.editors[0]!.cleanUp).not.toHaveBeenCalled();
  });

  it("installs the editor dismissal handler on the surface", () => {
    renderWithEffects();
    const surface = fakeSurface();
    harness.refs[1]!.current = surface.element;
    harness.runEffects();
    expect(testState.installFileEditorDismissal).toHaveBeenCalledTimes(1);
    const input = testState.installFileEditorDismissal.mock.calls[0]![0] as {
      root: unknown;
      isBlocked: () => boolean;
      onDismiss: () => void;
    };
    expect(input.root).toBe(surface.element);
    expect(input.isBlocked()).toBe(false);
    harness.setStateCalls.length = 0;
    input.onDismiss();
    expect(harness.setStateCalls.length).toBeGreaterThan(0);
  });

  it("saves explicitly on the scoped mod+s chord", () => {
    renderWithEffects();
    const surface = fakeSurface();
    harness.refs[1]!.current = surface.element;
    const cleanups = harness.runEffects();
    const handler = surface.listeners.get("keydown");
    expect(handler).toBeDefined();

    const plain = {
      key: "s",
      metaKey: false,
      ctrlKey: false,
      altKey: false,
      preventDefault: vi.fn(),
    };
    handler!(plain);
    expect(plain.preventDefault).not.toHaveBeenCalled();

    const withAlt = {
      key: "s",
      metaKey: false,
      ctrlKey: true,
      altKey: true,
      preventDefault: vi.fn(),
    };
    handler!(withAlt);
    expect(withAlt.preventDefault).not.toHaveBeenCalled();

    const chord = {
      key: "S",
      metaKey: false,
      ctrlKey: true,
      altKey: false,
      preventDefault: vi.fn(),
    };
    handler!(chord);
    expect(chord.preventDefault).toHaveBeenCalled();
    expect(testState.session!.flush).toHaveBeenCalledOnce();

    for (const cleanup of cleanups) cleanup();
    expect(surface.listeners.has("keydown")).toBe(false);
  });

  it("begins exact and descendant mutation leases and propagates preparation failure", async () => {
    renderWithEffects();
    const exactCoordinator = testState.coordinators[testState.coordinators.length - 1]!;
    renderPanel(baseProps({ relativePath: "src/nested/child.ts" }));
    const descendantCoordinator = testState.coordinators[testState.coordinators.length - 1]!;
    const browser = ui.find("FileBrowserPanel");
    const onBeginPathMutation = browser.onBeginPathMutation as (request: {
      kind: "rename" | "delete" | "duplicate";
      fromRelativePath?: string;
      toRelativePath?: string;
      relativePath?: string;
    }) => Promise<{ release(): void } | null>;

    await expect(
      onBeginPathMutation({ kind: "delete", relativePath: "src/app.ts" }),
    ).resolves.toEqual(expect.objectContaining({ release: expect.any(Function) }));
    expect(exactCoordinator.settle).toHaveBeenCalledTimes(1);
    expect(descendantCoordinator.settle).not.toHaveBeenCalled();

    descendantCoordinator.settle.mockResolvedValueOnce("failed");
    await expect(onBeginPathMutation({ kind: "delete", relativePath: "src" })).resolves.toBeNull();
    expect(exactCoordinator.settle).toHaveBeenCalledTimes(2);
    expect(descendantCoordinator.settle).toHaveBeenCalledTimes(1);

    await expect(
      onBeginPathMutation({ kind: "delete", relativePath: "docs/readme.md" }),
    ).resolves.toEqual(expect.objectContaining({ release: expect.any(Function) }));
    expect(exactCoordinator.settle).toHaveBeenCalledTimes(2);
    expect(descendantCoordinator.settle).toHaveBeenCalledTimes(1);
    expect(testState.editingSessions.beginPathMutation).toHaveBeenCalledTimes(3);
  });

  it("renders a guarded read-only file surface and passes the same reason to the browser", () => {
    const reason = "Workspace unavailable. Retry detection or remove it from BiBCode.";
    setFileData("const retained = true;\n");
    const markup = renderPanel(baseProps({ workspaceUnavailable: reason }));

    expect(ui.find("FileEditorToolbar")).toMatchObject({ canSave: false, cleanStatus: reason });
    expect(ui.find("FileBrowserPanel")).toMatchObject({ workspaceUnavailable: reason });
    expect(markup).toContain('data-file="src/app.ts"');
    expect(testState.editingSessions.getOrCreate).not.toHaveBeenCalled();
  });
});
