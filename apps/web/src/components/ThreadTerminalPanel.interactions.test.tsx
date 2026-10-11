// @vitest-environment happy-dom

import { scopeThreadRef } from "@bibcode/client-runtime/environment";
import {
  createTerminalTranscriptRuntime,
  type TerminalTranscriptRuntime,
} from "@bibcode/client-runtime/state/terminal";
import {
  type ActivityActorSummary,
  type ActivitySnapshot,
  ActivityScopeId,
  EnvironmentId,
  ProjectId,
  ProviderDriverKind,
  ProviderInstanceId,
  ThreadId,
  type ResolvedKeybindingsConfig,
  type TerminalAttachStreamEvent,
  type TerminalLaunchCommand,
  type TerminalSessionSnapshot,
} from "@bibcode/contracts";
import type { TerminalFontPreference, TerminalThemePreference } from "@bibcode/contracts/settings";
import * as Cause from "effect/Cause";
import * as Option from "effect/Option";
import { AsyncResult } from "effect/unstable/reactivity";
import { act, type ReactElement, useSyncExternalStore } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { TerminalThemeMode } from "./terminalTheme";

interface FakeTerminalInstance {
  readonly options: Record<string, unknown>;
  readonly modes: {
    mouseTrackingMode: "none" | "x10" | "vt200" | "drag" | "any";
    bracketedPasteMode: boolean;
  };
  readonly textarea: HTMLTextAreaElement;
  cols: number;
  rows: number;
  readonly open: ReturnType<typeof vi.fn>;
  readonly focus: ReturnType<typeof vi.fn>;
  readonly refresh: ReturnType<typeof vi.fn>;
  readonly resize: ReturnType<typeof vi.fn>;
  readonly scrollToBottom: ReturnType<typeof vi.fn>;
  readonly clearTextureAtlas: ReturnType<typeof vi.fn>;
  readonly dispose: ReturnType<typeof vi.fn>;
  readonly loadAddon: ReturnType<typeof vi.fn>;
  readonly clearSelection: ReturnType<typeof vi.fn>;
  readonly csiHandlers: Map<string, (params: (number | number[])[]) => boolean>;
  readonly oscHandlers: Map<number, (data: string) => boolean>;
  readonly inputDisposable: { readonly dispose: ReturnType<typeof vi.fn> };
  readonly selectionDisposable: { readonly dispose: ReturnType<typeof vi.fn> };
  readonly linkDisposable: { readonly dispose: ReturnType<typeof vi.fn> };
  readonly bufferLines: Array<FakeTerminalBufferLine | undefined>;
  readonly writes: string[];
  readonly writesAfterDispose: string[];
  displayedText: string;
  resetCount: number;
  dataHandler: ((data: string) => void) | null;
  linkProvider: FakeTerminalLinkProvider | null;
  selectionHandler: (() => void) | null;
  keyHandler: ((event: KeyboardEvent) => boolean) | null;
  hasActiveSelection: boolean;
  selectionText: string;
  selectionPosition: { start: { y: number } } | null;
}

interface FakeTerminalBufferLine {
  readonly isWrapped?: boolean;
  translateToString(trimRight?: boolean, startColumn?: number, endColumn?: number): string;
}

interface FakeTerminalLink {
  readonly text: string;
  readonly activate: (event: MouseEvent) => void;
}

interface FakeTerminalLinkProvider {
  provideLinks(
    bufferLineNumber: number,
    callback: (links: ReadonlyArray<FakeTerminalLink> | undefined) => void,
  ): void;
}

interface FakeWebglCanvas {
  width: number;
  height: number;
  getBoundingClientRect(): { width: number; height: number };
}

interface FakeWebglContext {
  readonly getExtension: ReturnType<typeof vi.fn>;
  readonly canvas: FakeWebglCanvas | null;
}

interface FakeWebglAddonInstance {
  readonly kind: "webgl";
  readonly activateSpy: ReturnType<typeof vi.fn>;
  readonly disposeSpy: ReturnType<typeof vi.fn>;
  readonly listenerDisposeSpy: ReturnType<typeof vi.fn>;
  dispose(): void;
  onContextLoss(listener: () => void): { dispose(): void };
  triggerContextLoss(): void;
}

const xtermState = vi.hoisted(() => ({
  terminals: [] as FakeTerminalInstance[],
  fitAddons: [] as Array<{
    fit: ReturnType<typeof vi.fn>;
    proposeDimensions: ReturnType<typeof vi.fn>;
  }>,
  answerQueriesOnWrite: false,
  deferWrites: false,
  pendingWrites: [] as Array<() => void>,
  fitShouldThrow: false,
  proposedDimensions: { cols: 80, rows: 24 } as { cols: number; rows: number } | null,
  loadWebglShouldThrow: false,
}));

const webglState = vi.hoisted(() => ({
  instances: [] as FakeWebglAddonInstance[],
  importCount: 0,
  importGate: null as Promise<void> | null,
  resolveImport: null as (() => void) | null,
  rejectImport: null as ((reason: unknown) => void) | null,
  constructorShouldThrow: false,
  activateShouldThrow: false,
  listenerShouldThrow: false,
  listenerTriggersSynchronously: false,
  listenerDisposeShouldThrow: false,
  disposeShouldThrow: false,
  contextMode: "present" as
    | "present"
    | "missing-context"
    | "missing-extension"
    | "getter-throws"
    | "extension-throws"
    | "lose-throws"
    | "missing-canvas",
  canvas: null as FakeWebglCanvas | null,
  drawingViewport: null as { width: number; height: number } | null,
  glyphResolution: null as { width: number; height: number } | null,
  loseContextSpy: vi.fn(),
  events: [] as string[],
  WebglAddonConstructor: null as (new () => FakeWebglAddonInstance) | null,
}));

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class FitAddon {
    readonly fit = vi.fn();
    readonly proposeDimensions = vi.fn(() => xtermState.proposedDimensions ?? undefined);
    constructor() {
      if (xtermState.fitShouldThrow) {
        this.fit.mockImplementation(() => {
          throw new Error("container not measurable");
        });
      }
      xtermState.fitAddons.push(this);
    }
  },
}));

vi.mock("@xterm/addon-webgl", () => {
  class WebglAddon implements FakeWebglAddonInstance {
    readonly kind = "webgl" as const;
    readonly activateSpy = vi.fn((_terminal: unknown) => {
      if (webglState.activateShouldThrow) {
        throw new Error("sensitive activation failure");
      }
    });
    readonly disposeSpy = vi.fn(() => {
      webglState.events.push("addon.dispose");
      if (webglState.disposeShouldThrow) {
        throw new Error("sensitive dispose failure");
      }
    });
    readonly listenerDisposeSpy = vi.fn();
    private contextLossListener: (() => void) | null = null;

    constructor() {
      webglState.events.push("addon.construct");
      if (webglState.constructorShouldThrow) {
        throw new Error("sensitive construction failure");
      }
      webglState.instances.push(this);
    }

    activate(terminal: unknown): void {
      this.activateSpy(terminal);
      // xterm rounds the CSS box to 608px while the backing store stays at the
      // exact 1215 device pixels, which is the mismatch WebKit never corrects.
      const canvas: FakeWebglCanvas = {
        width: 1215,
        height: 600,
        getBoundingClientRect: () => ({ width: 608, height: 300 }),
      };
      webglState.canvas = canvas;
      const context: FakeWebglContext = {
        canvas: webglState.contextMode === "missing-canvas" ? null : canvas,
        getExtension: vi.fn((name: string) => {
          expect(name).toBe("WEBGL_lose_context");
          if (webglState.contextMode === "extension-throws") {
            throw new Error("sensitive extension failure");
          }
          if (webglState.contextMode === "missing-extension") {
            return null;
          }
          return {
            loseContext: () => {
              webglState.events.push("context.lose");
              webglState.loseContextSpy();
              if (webglState.contextMode === "lose-throws") {
                throw new Error("sensitive context release failure");
              }
            },
          };
        }),
      };
      webglState.drawingViewport = { width: canvas.width, height: canvas.height };
      webglState.glyphResolution = { width: canvas.width, height: canvas.height };

      if (webglState.contextMode === "getter-throws") {
        Object.defineProperty(this, "_renderer", {
          configurable: true,
          get: () => {
            throw new Error("sensitive context getter failure");
          },
        });
      } else {
        Object.defineProperty(this, "_renderer", {
          configurable: true,
          value:
            webglState.contextMode === "missing-context"
              ? undefined
              : {
                  _gl: context,
                  _rectangleRenderer: {
                    value: {
                      handleResize: () => undefined,
                    },
                  },
                  _glyphRenderer: {
                    value: {
                      handleResize: () => {
                        webglState.drawingViewport = {
                          width: canvas.width,
                          height: canvas.height,
                        };
                        webglState.glyphResolution = {
                          width: canvas.width,
                          height: canvas.height,
                        };
                      },
                    },
                  },
                },
        });
      }
    }

    dispose(): void {
      this.disposeSpy();
    }

    onContextLoss(listener: () => void): { dispose(): void } {
      if (webglState.listenerShouldThrow) {
        throw new Error("sensitive listener failure");
      }
      this.contextLossListener = listener;
      const disposable = {
        dispose: () => {
          this.listenerDisposeSpy();
          this.contextLossListener = null;
          if (webglState.listenerDisposeShouldThrow) {
            throw new Error("sensitive listener dispose failure");
          }
        },
      };
      if (webglState.listenerTriggersSynchronously) {
        listener();
      }
      return disposable;
    }

    triggerContextLoss(): void {
      this.contextLossListener?.();
    }
  }
  webglState.WebglAddonConstructor = WebglAddon;
  return { WebglAddon };
});

// Only the loader is faked; `webglContextFrom`/`webglCanvasFrom` stay real so
// the addon plumbing under test is the code that actually ships.
vi.mock("./terminalWebgl", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./terminalWebgl")>()),
  loadTerminalWebglAddon: async () => {
    webglState.importCount += 1;
    if (webglState.WebglAddonConstructor === null) {
      await import("@xterm/addon-webgl");
    }
    const importGate = webglState.importGate;
    if (importGate !== null) {
      await importGate;
    }
    webglState.events.push("module.resolve");
    return { WebglAddon: webglState.WebglAddonConstructor! };
  },
}));

vi.mock("@xterm/xterm", () => ({
  Terminal: class Terminal {
    readonly options: Record<string, unknown>;
    readonly textarea = document.createElement("textarea");
    rows = 24;
    cols = 80;
    readonly writes: string[] = [];
    readonly writesAfterDispose: string[] = [];
    displayedText = "";
    resetCount = 0;
    private disposed = false;
    readonly open = vi.fn((parent: HTMLElement) => {
      const screen = document.createElement("div");
      screen.className = "xterm-screen";
      parent.append(screen);
    });
    readonly focus = vi.fn();
    readonly refresh = vi.fn();
    readonly resize = vi.fn((cols: number, rows: number) => {
      this.cols = cols;
      this.rows = rows;
    });
    readonly csiHandlers = new Map<string, (params: (number | number[])[]) => boolean>();
    readonly oscHandlers = new Map<number, (data: string) => boolean>();
    readonly parser = {
      registerCsiHandler: (
        id: { prefix?: string; intermediates?: string; final: string },
        handler: (params: (number | number[])[]) => boolean,
      ) => {
        const key = `${id.prefix ?? ""}${id.intermediates ?? ""}${id.final}`;
        this.csiHandlers.set(key, handler);
        return { dispose: () => this.csiHandlers.delete(key) };
      },
      registerOscHandler: (code: number, handler: (data: string) => boolean) => {
        this.oscHandlers.set(code, handler);
        return { dispose: () => this.oscHandlers.delete(code) };
      },
      registerDcsHandler: () => ({ dispose() {} }),
    };
    readonly scrollToBottom = vi.fn();
    readonly clearTextureAtlas = vi.fn();
    readonly loadedAddons: Array<{ dispose?: () => void }> = [];
    readonly dispose = vi.fn(() => {
      this.disposed = true;
      webglState.events.push("terminal.dispose");
      for (const addon of [...this.loadedAddons].toReversed()) {
        addon.dispose?.();
      }
    });
    readonly loadAddon = vi.fn(
      (addon: { kind?: string; activate?: (terminal: unknown) => void; dispose?: () => void }) => {
        const originalDispose = addon.dispose;
        let disposed = false;
        if (originalDispose !== undefined) {
          addon.dispose = () => {
            if (disposed) return;
            disposed = true;
            originalDispose.call(addon);
            const index = this.loadedAddons.indexOf(addon);
            if (index >= 0) {
              this.loadedAddons.splice(index, 1);
            }
          };
        }
        this.loadedAddons.push(addon);
        if (addon.kind === "webgl" && xtermState.loadWebglShouldThrow) {
          throw new Error("sensitive load failure");
        }
        addon.activate?.(this);
      },
    );
    readonly clearSelection = vi.fn();
    readonly inputDisposable = { dispose: vi.fn() };
    readonly selectionDisposable = { dispose: vi.fn() };
    readonly linkDisposable = { dispose: vi.fn() };
    readonly bufferLines: Array<FakeTerminalBufferLine | undefined> = [];
    readonly buffer = {
      active: {
        viewportY: 0,
        baseY: 0,
        getLine: (index: number) => this.bufferLines[index],
      },
    };
    dataHandler: ((data: string) => void) | null = null;
    linkProvider: FakeTerminalLinkProvider | null = null;
    selectionHandler: (() => void) | null = null;
    keyHandler: ((event: KeyboardEvent) => boolean) | null = null;
    hasActiveSelection = false;
    modes: {
      mouseTrackingMode: "none" | "x10" | "vt200" | "drag" | "any";
      bracketedPasteMode: boolean;
    } = {
      mouseTrackingMode: "none",
      bracketedPasteMode: false,
    };
    selectionText = "";
    selectionPosition: { start: { y: number } } | null = null;

    constructor(options: Record<string, unknown>) {
      this.options = options;
      xtermState.terminals.push(this);
    }

    attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean) {
      this.keyHandler = handler;
    }

    registerLinkProvider(provider: FakeTerminalLinkProvider) {
      this.linkProvider = provider;
      return this.linkDisposable;
    }

    onData(handler: (data: string) => void) {
      this.dataHandler = handler;
      return this.inputDisposable;
    }

    onSelectionChange(handler: () => void) {
      this.selectionHandler = handler;
      return this.selectionDisposable;
    }

    hasSelection() {
      return this.hasActiveSelection;
    }

    getSelection() {
      return this.selectionText;
    }

    getSelectionPosition() {
      return this.selectionPosition;
    }

    write(value: string, callback?: () => void) {
      if (this.disposed) {
        this.writesAfterDispose.push(value);
        return;
      }
      const parse = () => {
        if (this.disposed) return;
        this.writes.push(value);
        if (xtermState.answerQueriesOnWrite) {
          if (value.includes("\x1b[6n") && !this.csiHandlers.get("n")?.([6]))
            this.dataHandler?.("\x1b[1;1R");
          if (value.includes("\x1b[c") && !this.csiHandlers.get("c")?.([0]))
            this.dataHandler?.("\x1b[?1;2c");
        }
        const resetParts = value.split("\u001bc");
        if (resetParts.length === 1) this.displayedText += value;
        else {
          this.resetCount += resetParts.length - 1;
          this.displayedText = resetParts.at(-1) ?? "";
        }
        callback?.();
      };
      if (xtermState.deferWrites) xtermState.pendingWrites.push(parse);
      else parse();
    }
  },
}));

const testState = vi.hoisted(() => ({
  serverConfig: {
    availableEditors: ["vscode"],
    environment: { platform: { os: "windows" } },
  } as {
    availableEditors: string[];
    environment: {
      platform: { os: string };
      capabilities?: {
        terminalOrderedInput?: boolean;
        terminalSizeOwnership?: boolean;
        terminalImagePaste?: boolean;
      };
    };
  } | null,
  session: {
    buffer: "",
    error: null as string | null,
    status: "running",
    version: 0,
    generation: 0,
    consoleTheme: null as TerminalThemeMode | null,
    transcriptRuntime: null as TerminalTranscriptRuntime | null,
  },
  attachedSessionInputs: [] as unknown[],
  writeCommand: vi.fn(),
  prepareInputCommand: vi.fn(),
  resetInputCommand: vi.fn(),
  reattachCommand: vi.fn(),
  resizeCommand: vi.fn(),
  restartCommand: vi.fn(),
  stageImagePasteCommand: vi.fn(),
  previewCommand: vi.fn(),
  openPath: vi.fn(),
  contextMenuShow: vi.fn(),
  shellOpenExternal: vi.fn(),
  localApiAvailable: false,
  previewSupported: true,
  // Assigned in beforeEach: Option is not initialized when vi.hoisted runs.
  preparedConnection: null as unknown as Option.Option<{ httpBaseUrl: string }>,
  createAssetUrl: vi.fn(),
  webglEnabled: false,
  resolvedTheme: "light" as TerminalThemeMode,
  terminalFontPreference: { mode: "bundled" } as TerminalFontPreference,
  terminalThemePreference: "app" as TerminalThemePreference,
  fontLoad: vi.fn(() => Promise.resolve()),
  activityStateTargets: [] as unknown[],
  activityState: null as unknown,
  activityListeners: new Set<() => void>(),
  enableTerminalAgentActivityByEnvironment: new Map<string, boolean>(),
  settingsListeners: new Set<() => void>(),
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: (atom: unknown) => {
    if (atom === "server-config") return testState.serverConfig;
    return useSyncExternalStore(
      (listener) => {
        testState.activityListeners.add(listener);
        return () => testState.activityListeners.delete(listener);
      },
      () => testState.activityState,
      () => testState.activityState,
    );
  },
}));
vi.mock("../state/activity", () => ({
  environmentActivity: {
    stateValueAtom: (target: unknown) => {
      testState.activityStateTargets.push(target);
      return { kind: "activity-state", target };
    },
  },
}));
vi.mock("../state/server", () => ({
  serverEnvironment: { configValueAtom: () => "server-config" },
}));
vi.mock("../state/preview", () => ({ previewEnvironment: { open: "preview-open" } }));
vi.mock("../state/terminal", () => ({
  terminalEnvironment: {
    write: "terminal-write",
    prepareInput: "terminal-prepare-input",
    resetInput: "terminal-reset-input",
    resize: "terminal-resize",
    restart: "terminal-restart",
    stageImagePaste: "terminal-stage-image-paste",
  },
}));
vi.mock("../state/use-atom-command", () => ({
  useAtomCommand: (command: string) => {
    if (command === "terminal-write") return testState.writeCommand;
    if (command === "terminal-prepare-input") return testState.prepareInputCommand;
    if (command === "terminal-reset-input") return testState.resetInputCommand;
    if (command === "terminal-resize") return testState.resizeCommand;
    if (command === "terminal-restart") return testState.restartCommand;
    if (command === "terminal-stage-image-paste") return testState.stageImagePasteCommand;
    return testState.previewCommand;
  },
}));
vi.mock("../state/terminalSessions", () => ({
  useAttachedTerminalSession: (input: unknown) => {
    testState.attachedSessionInputs.push(input);
    return { ...testState.session, reattach: testState.reattachCommand };
  },
}));
vi.mock("../hooks/useSettings", () => ({
  useEnvironmentSettings: (
    environmentId: string,
    selector: (settings: { enableTerminalAgentActivity: boolean }) => unknown,
  ) => {
    const enableTerminalAgentActivity = useSyncExternalStore(
      (listener) => {
        testState.settingsListeners.add(listener);
        return () => testState.settingsListeners.delete(listener);
      },
      () => testState.enableTerminalAgentActivityByEnvironment.get(environmentId) ?? false,
      () => testState.enableTerminalAgentActivityByEnvironment.get(environmentId) ?? false,
    );
    return selector({ enableTerminalAgentActivity });
  },
  usePrimarySettings: (
    selector: (settings: {
      terminal: { webglEnabled: boolean };
      terminalFontPreference: TerminalFontPreference;
      terminalThemePreference: TerminalThemePreference;
    }) => unknown,
  ) =>
    selector({
      terminal: { webglEnabled: testState.webglEnabled },
      terminalFontPreference: testState.terminalFontPreference,
      terminalThemePreference: testState.terminalThemePreference,
    }),
}));
vi.mock("../hooks/useTheme", () => ({
  useTheme: () => ({ resolvedTheme: testState.resolvedTheme }),
}));
vi.mock("../lib/terminalFont", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/terminalFont")>();
  return {
    ...actual,
    ensureBundledTerminalFontLoaded: () => testState.fontLoad(),
  };
});
vi.mock("../editorPreferences", () => ({
  useOpenInPreferredEditor: () => testState.openPath,
}));
vi.mock("~/localApi", () => ({
  readLocalApi: () =>
    testState.localApiAvailable
      ? {
          contextMenu: { show: testState.contextMenuShow },
          shell: { openExternal: testState.shellOpenExternal },
        }
      : undefined,
}));
vi.mock("~/browser/openLink", () => ({ openLink: vi.fn() }));
vi.mock("~/browser/linkNotices", () => ({
  showFileOutsideWorkspaceNotice: vi.fn(),
  showPreviewFailedNotice: vi.fn(),
}));
vi.mock("~/browser/openFileInPreview", () => ({ openFileInPreview: vi.fn() }));
vi.mock("~/previewStateStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("~/previewStateStore")>()),
  isPreviewSupportedInRuntime: () => testState.previewSupported,
  canPreviewAnyUrlInRuntime: () => testState.previewSupported,
}));
vi.mock("../state/session", () => ({
  readPreparedConnection: () => Option.getOrNull(testState.preparedConnection),
}));
vi.mock("../state/assets", () => ({ assetEnvironment: { createUrl: "asset-create-url" } }));
vi.mock("../state/use-atom-query-runner", () => ({
  useAtomQueryRunner: () => testState.createAssetUrl,
}));
vi.mock("~/components/ui/popover", () => ({
  Popover: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  PopoverTrigger: ({
    render,
    children,
  }: {
    render?: React.ReactNode;
    children?: React.ReactNode;
  }) => <>{render ?? children}</>,
  PopoverPopup: ({ children }: { children?: React.ReactNode }) => <span>{children}</span>,
}));

import ThreadTerminalPanel, {
  enqueueTerminalInput,
  releaseTerminalUiResources,
  TerminalViewport,
} from "./ThreadTerminalPanel";
import { decodeTerminalLaunchCommand } from "../lib/terminalLaunchCommand";
import { openLink } from "~/browser/openLink";
import { showFileOutsideWorkspaceNotice, showPreviewFailedNotice } from "~/browser/linkNotices";
import { openFileInPreview } from "~/browser/openFileInPreview";

const ENVIRONMENT_ID = EnvironmentId.make("terminal-interactions");
const THREAD_ID = ThreadId.make("thread-interactions");
const THREAD_REF = scopeThreadRef(ENVIRONMENT_ID, THREAD_ID);
const PROJECT_ID = ProjectId.make("project-terminal-interactions");
const EMPTY_KEYBINDINGS: ResolvedKeybindingsConfig = [];

type ViewportProps = Parameters<typeof TerminalViewport>[0];
type PanelProps = Parameters<typeof ThreadTerminalPanel>[0];

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mountedTrees: MountedTree[] = [];
const observerInstances: Array<{
  observe: ReturnType<typeof vi.fn>;
  disconnect: ReturnType<typeof vi.fn>;
  callback: MutationCallback;
}> = [];
const resizeObserverInstances: Array<{
  observe: ReturnType<typeof vi.fn>;
  disconnect: ReturnType<typeof vi.fn>;
  callback: ResizeObserverCallback;
}> = [];
const componentTimers = new Set<unknown>();
const componentTimerCallbacks = new Map<() => void, unknown>();
const animationFrames = new Map<number, FrameRequestCallback>();
let nextAnimationFrame = 1;
let beginComponentTimerCapture = () => 0;
let captureComponentTimers = (_startIndex: number) => undefined;
let assertComponentTimerCleanup = () => undefined;
let assertComponentListenerCleanup = () => undefined;

async function mount(element: ReactElement): Promise<MountedTree> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const mounted = { container, root };
  mountedTrees.push(mounted);
  const timerCallStart = beginComponentTimerCapture();
  await act(async () => root.render(element));
  captureComponentTimers(timerCallStart);
  return mounted;
}

async function unmount(mounted: MountedTree): Promise<void> {
  const index = mountedTrees.indexOf(mounted);
  if (index >= 0) mountedTrees.splice(index, 1);
  await act(async () => mounted.root.unmount());
  mounted.container.remove();
}

async function click(element: HTMLElement): Promise<void> {
  await act(async () => element.click());
}

async function publishTerminalAgentActivitySetting(
  environmentId: EnvironmentId,
  enabled: boolean,
): Promise<void> {
  testState.enableTerminalAgentActivityByEnvironment.set(environmentId, enabled);
  await act(async () => {
    for (const listener of testState.settingsListeners) listener();
    await Promise.resolve();
  });
}

function buttonByLabel(label: string): HTMLButtonElement {
  const button = document.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
  expect(button).not.toBeNull();
  return button!;
}

function viewportProps(overrides: Partial<ViewportProps> = {}): ViewportProps {
  return {
    threadRef: THREAD_REF,
    threadId: THREAD_ID,
    projectId: PROJECT_ID,
    terminalId: "term-1",
    terminalLabel: "Terminal 1",
    cwd: "/repo",
    visible: true,
    onSessionExited: vi.fn(),
    onAddTerminalContext: vi.fn(),
    focusRequestId: 0,
    autoFocus: false,
    resizeEpoch: 0,
    keybindings: EMPTY_KEYBINDINGS,
    ...overrides,
  };
}

function panelProps(overrides: Partial<PanelProps> = {}): PanelProps {
  return {
    owner: "right-panel",
    threadRef: THREAD_REF,
    threadId: THREAD_ID,
    projectId: PROJECT_ID,
    cwd: "/repo",
    terminalIds: ["term-1"],
    activeTerminalId: "term-1",
    terminalGroups: [{ id: "group-1", terminalIds: ["term-1"] }],
    activeTerminalGroupId: "group-1",
    focusRequestId: 0,
    onSplitTerminal: vi.fn(),
    onSplitTerminalVertical: vi.fn(),
    onNewTerminal: vi.fn(),
    onActiveTerminalChange: vi.fn(),
    onCloseTerminal: vi.fn(),
    onAddTerminalContext: vi.fn(),
    keybindings: EMPTY_KEYBINDINGS,
    ...overrides,
  };
}

function observedCommand(): TerminalLaunchCommand {
  return {
    executable: "codex",
    args: [],
    activity: {
      driverKind: ProviderDriverKind.make("codex"),
      providerInstanceId: ProviderInstanceId.make("codex-default"),
    },
  };
}

function activitySnapshot(
  terminalId: string,
  overrides: Partial<ActivitySnapshot> = {},
): ActivitySnapshot {
  const currentActor = {
    _tag: "actor",
    id: `actor-${terminalId}`,
    name: `Agent ${terminalId}`,
    status: "running",
    summary: null,
    startedAt: "2026-07-22T20:00:00.000Z",
    updatedAt: "2026-07-22T20:01:00.000Z",
    terminalAt: null,
    parentActorId: null,
    role: null,
    providerType: "codex",
  } as unknown as ActivityActorSummary;
  return {
    protocolVersion: 2,
    scopeId: `scope-${terminalId}`,
    scope: { _tag: "terminal", threadId: THREAD_ID, terminalId },
    revision: 1,
    provider: "codex",
    providerInstanceId: ProviderInstanceId.make("codex-default"),
    capabilities: {
      actors: true,
      attributedActivity: true,
      backgroundWork: false,
      historyRecovery: "full",
      terminalObservation: true,
      targetedActorCancellation: false,
    },
    observationState: "live",
    sections: {
      subagents: { state: "live", message: null, retryable: false },
      backgroundTasks: { state: "unsupported", message: null, retryable: false },
    },
    counts: {
      subagents: { active: 1, done: 0 },
      backgroundTasks: { active: 0, done: 0 },
    },
    actors: [currentActor],
    workItems: [],
    actorsHasMore: false,
    workItemsHasMore: false,
    control: {
      scopeId: `scope-${terminalId}`,
      revision: 0,
      actors: [],
      operations: [],
    },
    updatedAt: "2026-07-22T20:01:00.000Z",
    ...overrides,
  } as ActivitySnapshot;
}

async function publishActivitySnapshot(snapshot: ActivitySnapshot): Promise<void> {
  await act(async () => {
    testState.activityState = {
      snapshot: Option.some(snapshot),
      status: "live",
      error: Option.none(),
      recentEntries: new Map(),
    };
    for (const listener of testState.activityListeners) listener();
  });
}

function terminalBufferLine(text: string, isWrapped = false): FakeTerminalBufferLine {
  return {
    isWrapped,
    translateToString: (trimRight = false, startColumn = 0, endColumn?: number) => {
      const slice = text.slice(startColumn, endColumn);
      return trimRight ? slice.trimEnd() : slice;
    },
  };
}

interface Osc8LinkHandler {
  activate(
    event: MouseEvent,
    uri: string,
    range: { start: { x: number; y: number }; end: { x: number; y: number } },
  ): void;
  allowNonHttpProtocols: boolean;
}

/** Writes `visible` as an OSC 8 link on a new row and returns a click on it. */
function osc8Link(terminal: FakeTerminalInstance, visible: string, uri: string) {
  terminal.bufferLines.push(terminalBufferLine(`> ${visible} <`));
  const y = terminal.bufferLines.length;
  const range = { start: { x: 3, y }, end: { x: 2 + visible.length, y } };
  const handler = terminal.options.linkHandler as Osc8LinkHandler;
  return (event: MouseEvent) => handler.activate(event, uri, range);
}

function terminalSnapshot(
  history: string,
  status: TerminalSessionSnapshot["status"] = "running",
): TerminalSessionSnapshot {
  return {
    threadId: THREAD_ID,
    terminalId: "term-1",
    cwd: "/repo",
    worktreePath: null,
    status,
    pid: 1,
    history,
    oscColorResponderActive: false,
    firstAttachmentGrant: false,
    exitCode: null,
    exitSignal: null,
    label: "Terminal 1",
    updatedAt: "2026-07-18T00:00:00.000Z",
    sequence: 1,
  };
}

function snapshotEvent(history: string): TerminalAttachStreamEvent {
  return { type: "snapshot", snapshot: terminalSnapshot(history) };
}

function outputEvent(data: string): TerminalAttachStreamEvent {
  return {
    type: "output",
    threadId: THREAD_ID,
    terminalId: "term-1",
    sequence: 2,
    data,
  };
}

function restartedEvent(history: string): TerminalAttachStreamEvent {
  return {
    type: "restarted",
    threadId: THREAD_ID,
    terminalId: "term-1",
    sequence: 3,
    snapshot: { ...terminalSnapshot(history), sequence: 3 },
  };
}

function clearedEvent(): TerminalAttachStreamEvent {
  return {
    type: "cleared",
    threadId: THREAD_ID,
    terminalId: "term-1",
    sequence: 4,
  };
}

function trackedTranscriptRuntime(initialSnapshot: string): {
  readonly runtime: TerminalTranscriptRuntime;
  readonly detachRendererSpy: ReturnType<typeof vi.fn>;
} {
  const source = createTerminalTranscriptRuntime();
  source.ingest(snapshotEvent(initialSnapshot));
  const detachRendererSpy = vi.fn();
  return {
    runtime: {
      ingest: (event) => source.ingest(event),
      attachRenderer: (sink, sizeClaim) => {
        const attachment = source.attachRenderer(sink, sizeClaim);
        let detached = false;
        return {
          detach() {
            if (detached) return;
            detached = true;
            detachRendererSpy();
            attachment.detach();
          },
        };
      },
      snapshot: () => source.snapshot(),
      metadata: () => source.metadata(),
      subscribeMetadata: (listener) => source.subscribeMetadata(listener),
    },
    detachRendererSpy,
  };
}

async function flushAnimationFrames(): Promise<void> {
  await act(async () => {
    while (animationFrames.size > 0) {
      const callbacks = Array.from(animationFrames.values());
      animationFrames.clear();
      for (const callback of callbacks) callback(0);
      await Promise.resolve();
    }
  });
}

async function flushTerminalWrites(): Promise<void> {
  await act(async () => {
    while (xtermState.pendingWrites.length > 0) {
      xtermState.pendingWrites.shift()!();
      await Promise.resolve();
    }
  });
}

async function flushComponentFitTimers(): Promise<void> {
  await act(async () => {
    for (const [callback, timerId] of componentTimerCallbacks) {
      componentTimerCallbacks.delete(callback);
      window.clearTimeout(timerId as number);
      callback();
      await Promise.resolve();
    }
  });
}

function deferWebglImport(): void {
  webglState.importGate = new Promise<void>((resolve, reject) => {
    webglState.resolveImport = resolve;
    webglState.rejectImport = reject;
  });
}

async function settleWebgl(): Promise<void> {
  await act(async () => {
    for (let index = 0; index < 8; index += 1) {
      await Promise.resolve();
    }
  });
}

async function mountViewport(
  options: {
    readonly visible?: boolean;
    readonly initialSnapshot?: string;
    readonly publishRuntime?: boolean;
    readonly webglEnabled?: boolean;
    readonly deferWebglImport?: boolean;
    readonly webglContextMode?: typeof webglState.contextMode;
    readonly autoFocus?: boolean;
    readonly focusRequestId?: number;
    readonly terminalId?: string;
    readonly terminalFontPreference?: TerminalFontPreference;
  } = {},
) {
  testState.webglEnabled = options.webglEnabled ?? false;
  testState.terminalFontPreference = options.terminalFontPreference ?? { mode: "bundled" };
  webglState.contextMode = options.webglContextMode ?? "present";
  if (options.deferWebglImport === true) {
    deferWebglImport();
  }
  const tracked = trackedTranscriptRuntime(options.initialSnapshot ?? "");
  const onSessionExited = vi.fn();
  let props = viewportProps({
    terminalId: options.terminalId ?? "term-1",
    visible: options.visible ?? true,
    onSessionExited,
    autoFocus: options.autoFocus ?? false,
    focusRequestId: options.focusRequestId ?? 0,
  } as Partial<ViewportProps>);
  testState.session = {
    buffer: options.initialSnapshot ?? "",
    error: null,
    status: "running",
    version: 1,
    generation: tracked.runtime.metadata().generation,
    consoleTheme: null,
    transcriptRuntime: options.publishRuntime === false ? null : tracked.runtime,
  };
  const mounted = await mount(<TerminalViewport {...props} />);

  const rerender = async () => {
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
  };

  return {
    mounted,
    runtime: tracked.runtime,
    detachRendererSpy: tracked.detachRendererSpy,
    closeSessionSpy: onSessionExited,
    get webglAddon(): FakeWebglAddonInstance | null {
      return webglState.instances.at(-1) ?? null;
    },
    get webglImported(): boolean {
      return webglState.importCount > 0;
    },
    get fakeTerminal(): FakeTerminalInstance | null {
      return (
        xtermState.terminals.findLast(
          (terminal) =>
            terminal.dispose.mock.calls.length === 0 &&
            terminal.open.mock.calls.some(
              ([element]) => element instanceof Node && mounted.container.contains(element),
            ),
        ) ?? null
      );
    },
    async publishRuntime() {
      testState.session = {
        ...testState.session,
        generation: tracked.runtime.metadata().generation,
        transcriptRuntime: tracked.runtime,
      };
      await rerender();
    },
    async setTranscriptRuntime(runtime: TerminalTranscriptRuntime | null) {
      testState.session = {
        ...testState.session,
        ...(runtime === null ? {} : { generation: runtime.metadata().generation }),
        transcriptRuntime: runtime,
      };
      await rerender();
    },
    async replaceRuntime(snapshot = "reconnected") {
      const replacement = trackedTranscriptRuntime(snapshot);
      testState.session = {
        ...testState.session,
        generation: replacement.runtime.metadata().generation,
        transcriptRuntime: replacement.runtime,
      };
      await rerender();
      return replacement;
    },
    async setVisible(visible: boolean) {
      props = { ...props, visible } as ViewportProps;
      await rerender();
    },
    async setWebglEnabled(webglEnabled: boolean) {
      testState.webglEnabled = webglEnabled;
      await rerender();
    },
    async setCwd(cwd: string) {
      props = { ...props, cwd } as ViewportProps;
      await rerender();
    },
    async setTerminalFontPreference(terminalFontPreference: TerminalFontPreference) {
      testState.terminalFontPreference = terminalFontPreference;
      await rerender();
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });
    },
    async setDocumentVisible(documentVisible: boolean) {
      Object.defineProperty(document, "visibilityState", {
        configurable: true,
        value: documentVisible ? "visible" : "hidden",
      });
      await act(async () => document.dispatchEvent(new Event("visibilitychange")));
    },
    async requestFocus(focusRequestId: number) {
      props = { ...props, focusRequestId } as ViewportProps;
      await rerender();
    },
    async setAutoFocus(autoFocus: boolean) {
      props = { ...props, autoFocus } as ViewportProps;
      await rerender();
    },
    async triggerResizeEpoch() {
      props = { ...props, resizeEpoch: props.resizeEpoch + 1 } as ViewportProps;
      await rerender();
    },
    setFitDimensions(cols: number, rows: number) {
      const terminal = this.fakeTerminal;
      expect(terminal).not.toBeNull();
      terminal!.cols = cols;
      terminal!.rows = rows;
    },
    async rerenderUnrelated() {
      props = { ...props, terminalLabel: `${props.terminalLabel} updated` } as ViewportProps;
      await rerender();
    },
    async setSessionMetadata(
      metadata: Partial<Pick<typeof testState.session, "error" | "status">>,
    ) {
      testState.session = { ...testState.session, ...metadata };
      await rerender();
    },
    pointerDownOnSurface() {
      const surface = mounted.container.querySelector(
        `[data-terminal-xterm-mount="${props.terminalId}"]`,
      );
      expect(surface).toBeInstanceOf(HTMLElement);
      surface!.dispatchEvent(
        new PointerEvent("pointerdown", { bubbles: true, button: 0, pointerId: 1 }),
      );
    },
    pointerDownOutsideSurface() {
      mounted.container.dispatchEvent(
        new PointerEvent("pointerdown", { bubbles: true, button: 0, pointerId: 1 }),
      );
    },
    emitOutput(data: string) {
      tracked.runtime.ingest(outputEvent(data));
    },
    async emitRestart(snapshot: string) {
      tracked.runtime.ingest(restartedEvent(snapshot));
      testState.session = {
        ...testState.session,
        generation: tracked.runtime.metadata().generation,
      };
      await rerender();
    },
    settleWebgl,
    async resolveWebglImport() {
      webglState.resolveImport?.();
      webglState.importGate = null;
      webglState.resolveImport = null;
      webglState.rejectImport = null;
      await settleWebgl();
    },
    async rejectWebglImport() {
      webglState.rejectImport?.(new Error("sensitive import failure"));
      webglState.importGate = null;
      webglState.resolveImport = null;
      webglState.rejectImport = null;
      await settleWebgl();
    },
    flushFrame: flushAnimationFrames,
    flushMountFit: flushComponentFitTimers,
    resizeSpy: testState.resizeCommand,
  };
}

function provideTerminalLinks(
  terminal: FakeTerminalInstance,
  bufferLineNumber = 1,
): ReadonlyArray<FakeTerminalLink> | undefined {
  let providedLinks: ReadonlyArray<FakeTerminalLink> | undefined;
  terminal.linkProvider?.provideLinks(bufferLineNumber, (links) => {
    providedLinks = links;
  });
  return providedLinks;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.useRealTimers();
  xtermState.terminals = [];
  xtermState.fitAddons = [];
  xtermState.fitShouldThrow = false;
  xtermState.answerQueriesOnWrite = false;
  xtermState.deferWrites = false;
  xtermState.pendingWrites = [];
  xtermState.proposedDimensions = { cols: 80, rows: 24 };
  xtermState.loadWebglShouldThrow = false;
  webglState.instances = [];
  webglState.importCount = 0;
  webglState.importGate = null;
  webglState.resolveImport = null;
  webglState.rejectImport = null;
  webglState.constructorShouldThrow = false;
  webglState.activateShouldThrow = false;
  webglState.listenerShouldThrow = false;
  webglState.listenerTriggersSynchronously = false;
  webglState.listenerDisposeShouldThrow = false;
  webglState.disposeShouldThrow = false;
  webglState.contextMode = "present";
  webglState.canvas = null;
  webglState.drawingViewport = null;
  webglState.glyphResolution = null;
  webglState.loseContextSpy.mockReset();
  webglState.events = [];
  const defaultRuntime = createTerminalTranscriptRuntime();
  defaultRuntime.ingest(snapshotEvent(""));
  testState.session = {
    buffer: "",
    error: null,
    status: "running",
    version: 0,
    generation: defaultRuntime.metadata().generation,
    consoleTheme: null,
    transcriptRuntime: defaultRuntime,
  };
  testState.attachedSessionInputs.length = 0;
  testState.activityStateTargets.length = 0;
  testState.activityState = {
    snapshot: Option.none(),
    status: "empty",
    error: Option.none(),
    recentEntries: new Map(),
  };
  testState.activityListeners.clear();
  testState.enableTerminalAgentActivityByEnvironment.clear();
  testState.settingsListeners.clear();
  testState.writeCommand.mockReset().mockResolvedValue(AsyncResult.success(undefined));
  testState.prepareInputCommand.mockReset().mockResolvedValue(AsyncResult.success(undefined));
  testState.resetInputCommand.mockReset().mockResolvedValue(AsyncResult.success(undefined));
  testState.reattachCommand.mockReset();
  testState.resizeCommand.mockReset().mockResolvedValue(AsyncResult.success(undefined));
  testState.restartCommand.mockReset().mockResolvedValue(AsyncResult.success(undefined));
  testState.stageImagePasteCommand
    .mockReset()
    .mockResolvedValue(AsyncResult.success("/state/terminal-pastes/paste.png"));
  testState.previewCommand.mockReset().mockResolvedValue(AsyncResult.success(undefined));
  testState.openPath.mockReset().mockResolvedValue(AsyncResult.success(undefined));
  testState.contextMenuShow.mockReset().mockResolvedValue(undefined);
  testState.shellOpenExternal.mockReset().mockResolvedValue(undefined);
  vi.mocked(openLink).mockReset().mockReturnValue("app");
  vi.mocked(showFileOutsideWorkspaceNotice).mockReset();
  vi.mocked(showPreviewFailedNotice).mockReset();
  vi.mocked(openFileInPreview).mockReset().mockResolvedValue(AsyncResult.success(undefined));
  testState.localApiAvailable = false;
  testState.previewSupported = true;
  testState.createAssetUrl.mockReset();
  testState.preparedConnection = Option.some({ httpBaseUrl: "http://127.0.0.1:3773" });
  testState.webglEnabled = false;
  testState.resolvedTheme = "light";
  testState.serverConfig = {
    availableEditors: ["vscode"],
    environment: { platform: { os: "windows" } },
  };
  testState.terminalFontPreference = { mode: "bundled" };
  testState.fontLoad.mockReset().mockResolvedValue(undefined);
  observerInstances.length = 0;
  resizeObserverInstances.length = 0;
  componentTimers.clear();
  componentTimerCallbacks.clear();
  animationFrames.clear();
  nextAnimationFrame = 1;
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    value: "visible",
  });

  const addWindowListener = vi.spyOn(window, "addEventListener");
  const removeWindowListener = vi.spyOn(window, "removeEventListener");
  const addElementListener = vi.spyOn(HTMLElement.prototype, "addEventListener");
  const removeElementListener = vi.spyOn(HTMLElement.prototype, "removeEventListener");
  assertComponentListenerCleanup = () => {
    for (const [type, listener] of addWindowListener.mock.calls) {
      if (type === "focus") {
        expect(removeWindowListener).toHaveBeenCalledWith(type, listener);
      }
    }
    for (const [index, [type, listener]] of addElementListener.mock.calls.entries()) {
      const target = addElementListener.mock.instances[index];
      if (
        (type !== "pointerdown" && type !== "contextmenu") ||
        !(target instanceof HTMLElement) ||
        !target.classList.contains("overflow-hidden")
      ) {
        continue;
      }
      const matchingRemoval = removeElementListener.mock.calls.some(
        ([removedType, removedListener], removalIndex) =>
          removeElementListener.mock.instances[removalIndex] === target &&
          removedType === type &&
          removedListener === listener,
      );
      expect(matchingRemoval).toBe(true);
    }
  };

  vi.stubGlobal(
    "MutationObserver",
    class MutationObserver {
      readonly observe = vi.fn();
      readonly disconnect = vi.fn();
      readonly callback: MutationCallback;
      constructor(callback: MutationCallback) {
        this.callback = callback;
        observerInstances.push(this);
      }
    },
  );
  vi.stubGlobal(
    "ResizeObserver",
    class ResizeObserver {
      readonly observe = vi.fn();
      readonly disconnect = vi.fn();
      readonly callback: ResizeObserverCallback;
      constructor(callback: ResizeObserverCallback) {
        this.callback = callback;
        resizeObserverInstances.push(this);
      }
    },
  );
  const setTimeoutSpy = vi.spyOn(window, "setTimeout");
  const clearTimeoutSpy = vi.spyOn(window, "clearTimeout");
  beginComponentTimerCapture = () => setTimeoutSpy.mock.calls.length;
  captureComponentTimers = (startIndex) => {
    for (let index = startIndex; index < setTimeoutSpy.mock.calls.length; index += 1) {
      if (setTimeoutSpy.mock.calls[index]?.[1] !== 30) continue;
      const result = setTimeoutSpy.mock.results[index] as
        | { readonly type: string; readonly value?: unknown }
        | undefined;
      if (result?.type === "return") {
        componentTimers.add(result.value);
        const callback = setTimeoutSpy.mock.calls[index]?.[0];
        if (typeof callback === "function") {
          componentTimerCallbacks.set(callback, result.value);
        }
      }
    }
  };
  assertComponentTimerCleanup = () => {
    const clearedTimers = new Set<unknown>(clearTimeoutSpy.mock.calls.map(([timerId]) => timerId));
    for (const timerId of componentTimers) {
      if (clearedTimers.has(timerId)) componentTimers.delete(timerId);
    }
    expect(componentTimers.size).toBe(0);
  };
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    const id = nextAnimationFrame++;
    animationFrames.set(id, callback);
    return id;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => {
    animationFrames.delete(id);
  });
});

afterEach(async () => {
  while (mountedTrees.length > 0) await unmount(mountedTrees[0]!);
  for (const terminalId of [
    "term-1",
    "term-coalesced",
    "term-gated",
    "term-release",
    "term-release-stale",
    "term-remount",
    "term-split-a",
    "term-split-b",
  ]) {
    releaseTerminalUiResources(ENVIRONMENT_ID, THREAD_ID, terminalId);
  }
  assertComponentTimerCleanup();
  expect(animationFrames.size).toBe(0);
  assertComponentListenerCleanup();
  for (const observer of observerInstances) expect(observer.disconnect).toHaveBeenCalledOnce();
  for (const observer of resizeObserverInstances)
    expect(observer.disconnect).toHaveBeenCalledOnce();
  for (const terminal of xtermState.terminals) {
    expect(terminal.inputDisposable.dispose).toHaveBeenCalledOnce();
    expect(terminal.selectionDisposable.dispose).toHaveBeenCalledOnce();
    expect(terminal.linkDisposable.dispose).toHaveBeenCalledOnce();
    expect(terminal.dispose).toHaveBeenCalledOnce();
  }
  componentTimers.clear();
  componentTimerCallbacks.clear();
  animationFrames.clear();
  observerInstances.length = 0;
  resizeObserverInstances.length = 0;
  xtermState.terminals = [];
  xtermState.fitAddons = [];
  document.body.replaceChildren();
  Reflect.deleteProperty(document, "execCommand");
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  beginComponentTimerCapture = () => 0;
  captureComponentTimers = () => undefined;
  assertComponentTimerCleanup = () => undefined;
  assertComponentListenerCleanup = () => undefined;
});

describe("TerminalViewport provider activity mount", () => {
  it("does not create an activity subscription for an ordinary shell", async () => {
    await mount(<TerminalViewport {...viewportProps({ terminalId: "terminal-shell" })} />);

    expect(testState.activityStateTargets).toEqual([]);
    expect(document.querySelector('[data-testid="activity-dock"]')).toBeNull();
  });

  it("mounts a correlated provider dock in the exact terminal viewport", async () => {
    await publishTerminalAgentActivitySetting(ENVIRONMENT_ID, true);
    await publishActivitySnapshot(activitySnapshot("terminal-codex"));

    const mounted = await mount(
      <TerminalViewport
        {...viewportProps({
          terminalId: "terminal-codex",
          command: observedCommand(),
        })}
      />,
    );

    expect(testState.activityStateTargets).toEqual([
      {
        environmentId: ENVIRONMENT_ID,
        input: {
          _tag: "terminal",
          threadId: THREAD_ID,
          terminalId: "terminal-codex",
        },
      },
    ]);
    expect(
      mounted.container.querySelector(
        '[data-provider-terminal-activity-host="terminal-codex"] [data-testid="activity-dock"]',
      ),
    ).not.toBeNull();
  });

  it("does not mount duplicate live announcements for hidden provider panes", async () => {
    await publishTerminalAgentActivitySetting(ENVIRONMENT_ID, true);
    await publishActivitySnapshot(activitySnapshot("terminal-visible"));

    const mounted = await mount(
      <>
        <TerminalViewport
          {...viewportProps({
            terminalId: "terminal-visible",
            command: observedCommand(),
            visible: true,
          })}
        />
        <TerminalViewport
          {...viewportProps({
            terminalId: "terminal-hidden",
            command: observedCommand(),
            visible: false,
          })}
        />
      </>,
    );

    expect(testState.activityStateTargets).toEqual([
      {
        environmentId: ENVIRONMENT_ID,
        input: {
          _tag: "terminal",
          threadId: THREAD_ID,
          terminalId: "terminal-visible",
        },
      },
    ]);
    expect(mounted.container.querySelectorAll('[aria-live="polite"]')).toHaveLength(1);
    expect(
      mounted.container.querySelector('[data-provider-terminal-activity-host="terminal-hidden"]'),
    ).toBeNull();
  });

  it("keeps restart journals scoped to the terminal and rejects another terminal snapshot", async () => {
    await publishTerminalAgentActivitySetting(ENVIRONMENT_ID, true);
    const props = viewportProps({
      terminalId: "terminal-pinned",
      command: observedCommand(),
    });
    await publishActivitySnapshot(activitySnapshot("terminal-pinned"));
    const mounted = await mount(<TerminalViewport {...props} />);
    expect(mounted.container.querySelector('[data-testid="activity-dock"]')).not.toBeNull();

    await publishActivitySnapshot(activitySnapshot("terminal-other"));
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    expect(mounted.container.querySelector('[data-testid="activity-dock"]')).toBeNull();

    await publishActivitySnapshot(
      activitySnapshot("terminal-pinned", {
        scopeId: ActivityScopeId.make("scope-terminal-pinned-generation-2"),
        revision: 0,
        observationState: "stale",
      }),
    );
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    expect(mounted.container.querySelector('[data-testid="activity-dock"]')).not.toBeNull();
    expect(mounted.container.querySelector('[aria-label="Activity data stale"]')).not.toBeNull();
  });

  it("removes only the disabled environment host without remounting either terminal", async () => {
    const firstEnvironmentId = EnvironmentId.make("environment-terminal-activity-first");
    const secondEnvironmentId = EnvironmentId.make("environment-terminal-activity-second");
    const firstThreadId = ThreadId.make("thread-terminal-activity-first");
    const secondThreadId = ThreadId.make("thread-terminal-activity-second");
    const firstThreadRef = scopeThreadRef(firstEnvironmentId, firstThreadId);
    const secondThreadRef = scopeThreadRef(secondEnvironmentId, secondThreadId);
    const firstTerminalId = "terminal-activity-first";
    const secondTerminalId = "terminal-activity-second";
    await publishTerminalAgentActivitySetting(firstEnvironmentId, true);
    await publishTerminalAgentActivitySetting(secondEnvironmentId, true);
    const mounted = await mount(
      <>
        <ThreadTerminalPanel
          {...panelProps({
            threadRef: firstThreadRef,
            threadId: firstThreadId,
            terminalIds: [firstTerminalId],
            activeTerminalId: firstTerminalId,
            terminalGroups: [{ id: "group-first", terminalIds: [firstTerminalId] }],
            activeTerminalGroupId: "group-first",
            terminalCommandsById: new Map([[firstTerminalId, observedCommand()]]),
          })}
        />
        <ThreadTerminalPanel
          {...panelProps({
            threadRef: secondThreadRef,
            threadId: secondThreadId,
            terminalIds: [secondTerminalId],
            activeTerminalId: secondTerminalId,
            terminalGroups: [{ id: "group-second", terminalIds: [secondTerminalId] }],
            activeTerminalGroupId: "group-second",
            terminalCommandsById: new Map([[secondTerminalId, observedCommand()]]),
          })}
        />
      </>,
    );
    const firstTerminal = mounted.container.querySelector(
      `[data-terminal-xterm-mount="${firstTerminalId}"]`,
    );
    const secondTerminal = mounted.container.querySelector(
      `[data-terminal-xterm-mount="${secondTerminalId}"]`,
    );
    expect(firstTerminal).not.toBeNull();
    expect(secondTerminal).not.toBeNull();
    expect(
      mounted.container.querySelector(
        `[data-provider-terminal-activity-host="${firstTerminalId}"]`,
      ),
    ).not.toBeNull();
    expect(
      mounted.container.querySelector(
        `[data-provider-terminal-activity-host="${secondTerminalId}"]`,
      ),
    ).not.toBeNull();

    await publishTerminalAgentActivitySetting(firstEnvironmentId, false);

    expect(
      mounted.container.querySelector(
        `[data-provider-terminal-activity-host="${firstTerminalId}"]`,
      ),
    ).toBeNull();
    expect(
      mounted.container.querySelector(
        `[data-provider-terminal-activity-host="${secondTerminalId}"]`,
      ),
    ).not.toBeNull();
    expect(
      mounted.container.querySelector(`[data-terminal-xterm-mount="${firstTerminalId}"]`),
    ).toBe(firstTerminal);
    expect(
      mounted.container.querySelector(`[data-terminal-xterm-mount="${secondTerminalId}"]`),
    ).toBe(secondTerminal);

    await publishTerminalAgentActivitySetting(firstEnvironmentId, true);

    expect(
      mounted.container.querySelector(
        `[data-provider-terminal-activity-host="${firstTerminalId}"]`,
      ),
    ).not.toBeNull();
    expect(
      mounted.container.querySelector(`[data-terminal-xterm-mount="${firstTerminalId}"]`),
    ).toBe(firstTerminal);
  });
});

describe("TerminalViewport mounted lifecycle", () => {
  it.each([false, true])(
    "updates the font without recreating the terminal (WebGL: %s)",
    async (webglEnabled) => {
      const view = await mountViewport({ visible: true, webglEnabled });
      const terminal = view.fakeTerminal!;
      const fitAddon = xtermState.fitAddons[0]!;
      await view.flushFrame();
      await view.flushMountFit();

      testState.resizeCommand.mockClear();
      terminal.refresh.mockClear();
      terminal.scrollToBottom.mockClear();
      terminal.clearTextureAtlas.mockClear();
      fitAddon.fit.mockClear();
      testState.fontLoad.mockClear();
      fitAddon.fit.mockImplementation(() => {
        terminal.cols += 1;
      });

      await view.setTerminalFontPreference({ mode: "custom", family: "Maple Mono" });

      expect(xtermState.terminals).toHaveLength(1);
      expect(terminal.dispose).not.toHaveBeenCalled();
      expect(view.detachRendererSpy).not.toHaveBeenCalled();
      expect(terminal.options.fontFamily).toBe('"Maple Mono", monospace');
      expect(testState.fontLoad).toHaveBeenCalledOnce();
      expect(terminal.clearTextureAtlas).toHaveBeenCalledOnce();
      expect(fitAddon.fit).toHaveBeenCalledOnce();
      expect(terminal.scrollToBottom).toHaveBeenCalledOnce();
      expect(terminal.refresh).toHaveBeenCalledWith(0, terminal.rows - 1);
      expect(testState.resizeCommand).toHaveBeenCalledWith({
        environmentId: ENVIRONMENT_ID,
        input: { threadId: THREAD_ID, terminalId: "term-1", cols: 81, rows: 24 },
      });
    },
  );

  it("issues one resize for the initial geometry and skips unchanged fits from either path", async () => {
    const view = await mountViewport({ visible: true });

    await view.flushFrame();
    await view.flushMountFit();

    expect(view.resizeSpy).toHaveBeenCalledOnce();
    expect(view.resizeSpy).toHaveBeenLastCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-1", cols: 80, rows: 24 },
    });

    await view.triggerResizeEpoch();
    await view.flushFrame();

    expect(view.resizeSpy).toHaveBeenCalledOnce();
  });

  it("measures a renderer before attaching and supplies its fitted dimensions", async () => {
    const view = await mountViewport({ publishRuntime: false });
    expect(view.fakeTerminal).not.toBeNull();
    expect(testState.attachedSessionInputs.at(-1)).toMatchObject({
      attach: true,
      terminal: { cols: 80, rows: 24 },
    });
  });

  it("attaches without dimensions when geometry is not measurable", async () => {
    xtermState.proposedDimensions = null;
    await mountViewport({ publishRuntime: false });
    const attached = testState.attachedSessionInputs.at(-1) as {
      attach: boolean;
      terminal: { cols?: number; rows?: number };
    };
    expect(attached.attach).toBe(true);
    expect(attached.terminal.cols).toBeUndefined();
    expect(attached.terminal.rows).toBeUndefined();
  });

  it("mirrors the broadcast size, ignores layout changes, and offers an explicit fit claim", async () => {
    testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    const view = await mountViewport();
    await act(async () =>
      view.runtime.ingest({
        type: "snapshot",
        snapshot: {
          ...terminalSnapshot("prompt"),
          size: { cols: 151, rows: 50, sizeClaim: "another-window" },
        },
      }),
    );
    expect(view.fakeTerminal?.cols).toBe(151);
    expect(view.fakeTerminal?.rows).toBe(50);
    expect(view.resizeSpy).not.toHaveBeenCalled();
    const observer = resizeObserverInstances[0]!;
    await act(async () => {
      observer.callback([], observer as never);
    });
    await view.flushFrame();
    expect(view.resizeSpy).not.toHaveBeenCalled();
    const fit = Array.from(view.mounted.container.querySelectorAll("button")).find(
      (button) => button.textContent === "Fit to this window",
    );
    expect(fit).toBeDefined();
    await act(async () => fit!.click());
    expect(view.resizeSpy).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: {
        threadId: THREAD_ID,
        terminalId: "term-1",
        cols: 80,
        rows: 24,
        sizeClaim: expect.any(String),
      },
    });
  });

  it("claims focused attachments at equal size and resizes on layout only while the broadcast owner", async () => {
    testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    const view = await mountViewport();
    await act(async () =>
      view.runtime.ingest({
        type: "snapshot",
        snapshot: {
          ...terminalSnapshot("prompt"),
          size: { cols: 80, rows: 24, sizeClaim: "previous-owner" },
        },
      }),
    );
    const claim = view.resizeSpy.mock.calls.at(-1)![0].input.sizeClaim as string;
    expect(claim).toEqual(expect.any(String));
    expect(claim).not.toBe("previous-owner");
    await act(async () =>
      view.runtime.ingest({
        type: "resized",
        threadId: THREAD_ID,
        terminalId: "term-1",
        size: { cols: 80, rows: 24, sizeClaim: claim },
      }),
    );
    expect(view.mounted.container.textContent).not.toContain("Fit to this window");
    xtermState.proposedDimensions = { cols: 91, rows: 42 };
    view.resizeSpy.mockClear();
    await view.triggerResizeEpoch();
    await view.flushFrame();
    expect(view.resizeSpy).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-1", cols: 91, rows: 42, sizeClaim: claim },
    });
    await act(async () =>
      view.runtime.ingest({
        type: "resized",
        threadId: THREAD_ID,
        terminalId: "term-1",
        size: { cols: 151, rows: 50, sizeClaim: "next-owner" },
      }),
    );
    view.resizeSpy.mockClear();
    await view.triggerResizeEpoch();
    await view.flushFrame();
    expect(view.resizeSpy).not.toHaveBeenCalled();
    expect(view.fakeTerminal?.cols).toBe(151);
  });

  it.each(["focus", "pointer", "keypress"] as const)(
    "claims an equal-size mirror on terminal %s and deduplicates while the resize RPC is pending",
    async (trigger) => {
      testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
      vi.spyOn(document, "hasFocus").mockReturnValue(false);
      const view = await mountViewport();
      await act(async () =>
        view.runtime.ingest({
          type: "snapshot",
          snapshot: {
            ...terminalSnapshot("prompt"),
            size: { cols: 80, rows: 24, sizeClaim: "other" },
          },
        }),
      );
      const mount = view.mounted.container.querySelector<HTMLElement>(
        "[data-terminal-xterm-mount]",
      )!;
      const interact = () => {
        if (trigger === "focus") mount.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
        if (trigger === "pointer")
          mount.dispatchEvent(new PointerEvent("pointerdown", { button: 0, bubbles: true }));
        if (trigger === "keypress")
          view.fakeTerminal!.keyHandler?.(new KeyboardEvent("keydown", { key: "a" }));
      };
      expect(view.mounted.container.textContent).not.toContain("Fit to this window");
      await act(async () => {
        interact();
        interact();
        interact();
      });
      expect(view.resizeSpy).toHaveBeenCalledTimes(1);
      const claim = view.resizeSpy.mock.calls[0]![0].input.sizeClaim as string;
      expect(claim).toBe(
        (testState.attachedSessionInputs.at(-1) as { terminal: { sizeClaim: string } }).terminal
          .sizeClaim,
      );
      await act(async () =>
        view.runtime.ingest({
          type: "resized",
          threadId: THREAD_ID,
          terminalId: "term-1",
          size: { cols: 80, rows: 24, sizeClaim: claim },
        }),
      );
      await act(async () => interact());
      expect(view.resizeSpy).toHaveBeenCalledTimes(1);
      await act(async () =>
        view.runtime.ingest({
          type: "resized",
          threadId: THREAD_ID,
          terminalId: "term-1",
          size: { cols: 80, rows: 24, sizeClaim: "other-again" },
        }),
      );
      await act(async () => interact());
      expect(view.resizeSpy).toHaveBeenCalledTimes(2);
    },
  );

  it("claims null ownership immediately without focus and keeps replying before its echo", async () => {
    testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    const view = await mountViewport();
    let settle!: (value: unknown) => void;
    view.resizeSpy.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          settle = resolve;
        }),
    );
    await act(async () =>
      view.runtime.ingest({
        type: "snapshot",
        snapshot: { ...terminalSnapshot("prompt"), size: { cols: 151, rows: 50, sizeClaim: null } },
      }),
    );
    expect(view.resizeSpy).toHaveBeenCalledTimes(1);
    expect(view.fakeTerminal!.csiHandlers.get("n")!([6])).toBe(false);
    expect(view.mounted.container.textContent).not.toContain("Fit to this window");
    await act(async () =>
      view.runtime.ingest({
        type: "resized",
        threadId: THREAD_ID,
        terminalId: "term-1",
        size: { cols: 151, rows: 50, sizeClaim: null },
      }),
    );
    expect(view.resizeSpy).toHaveBeenCalledTimes(1);
    const claim = view.resizeSpy.mock.calls[0]![0].input.sizeClaim as string;
    await act(async () =>
      view.runtime.ingest({
        type: "resized",
        threadId: THREAD_ID,
        terminalId: "term-1",
        size: { cols: 80, rows: 24, sizeClaim: claim },
      }),
    );
    await act(async () => settle(AsyncResult.success(undefined)));
    await act(async () =>
      view.runtime.ingest({
        type: "resized",
        threadId: THREAD_ID,
        terminalId: "term-1",
        size: { cols: 80, rows: 24, sizeClaim: null },
      }),
    );
    expect(view.resizeSpy).toHaveBeenCalledTimes(2);
  });

  it("clears a failed claim slot and restores the different-size mirror action", async () => {
    testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    const view = await mountViewport();
    await act(async () =>
      view.runtime.ingest({
        type: "snapshot",
        snapshot: {
          ...terminalSnapshot("prompt"),
          size: { cols: 151, rows: 50, sizeClaim: "other" },
        },
      }),
    );
    let settle!: (value: unknown) => void;
    view.resizeSpy.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          settle = resolve;
        }),
    );
    const mount = view.mounted.container.querySelector<HTMLElement>("[data-terminal-xterm-mount]")!;
    await act(async () => mount.dispatchEvent(new FocusEvent("focusin", { bubbles: true })));
    expect(view.mounted.container.textContent).not.toContain("Fit to this window");
    await act(async () =>
      mount.dispatchEvent(new PointerEvent("pointerdown", { button: 0, bubbles: true })),
    );
    expect(view.resizeSpy).toHaveBeenCalledTimes(1);
    await act(async () => settle(AsyncResult.failure(Cause.fail(new Error("resize failed")))));
    expect(view.mounted.container.textContent).toContain("Fit to this window");
    await act(async () =>
      view.fakeTerminal!.keyHandler?.(new KeyboardEvent("keydown", { key: "a" })),
    );
    expect(view.resizeSpy).toHaveBeenCalledTimes(2);
  });

  it("retires a stale pending claim when a restarted snapshot begins a new generation", async () => {
    testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    const view = await mountViewport();
    let finishOld!: (value: unknown) => void;
    view.resizeSpy.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishOld = resolve;
        }),
    );
    await act(async () =>
      view.runtime.ingest({
        type: "snapshot",
        snapshot: { ...terminalSnapshot("old"), size: { cols: 151, rows: 50, sizeClaim: null } },
      }),
    );
    expect(view.resizeSpy).toHaveBeenCalledTimes(1);
    view.resizeSpy.mockImplementationOnce(() => new Promise(() => {}));
    await act(async () =>
      view.runtime.ingest({
        type: "restarted",
        threadId: THREAD_ID,
        terminalId: "term-1",
        snapshot: { ...terminalSnapshot("new"), size: { cols: 151, rows: 50, sizeClaim: null } },
      }),
    );
    expect(view.resizeSpy).toHaveBeenCalledTimes(2);
    await act(async () => finishOld(AsyncResult.failure(Cause.fail(new Error("old connection")))));
    await act(async () =>
      view.fakeTerminal!.keyHandler?.(new KeyboardEvent("keydown", { key: "a" })),
    );
    expect(view.resizeSpy).toHaveBeenCalledTimes(2);
  });

  it("answers first-attachment history queries once and guards a later remount", async () => {
    testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    xtermState.answerQueriesOnWrite = true;
    const view = await mountViewport();
    const claim = (testState.attachedSessionInputs.at(-1) as { terminal: { sizeClaim: string } })
      .terminal.sizeClaim;
    await act(async () =>
      view.runtime.ingest({
        type: "snapshot",
        snapshot: {
          ...terminalSnapshot("\x1b[6n\x1b[c"),
          firstAttachmentGrant: true,
          size: { cols: 80, rows: 24, sizeClaim: claim },
        },
      }),
    );
    await view.flushFrame();
    const written = () =>
      testState.writeCommand.mock.calls.map(([request]) => request.input.data).join("");
    expect(written()).toBe("\x1b[1;1R\x1b[?1;2c");
    await view.setTranscriptRuntime(null);
    await view.setTranscriptRuntime(view.runtime);
    await view.flushFrame();
    expect(written()).toBe("\x1b[1;1R\x1b[?1;2c");
    await view.setVisible(false);
    await view.setVisible(true);
    await view.flushFrame();
    expect(written()).toBe("\x1b[1;1R\x1b[?1;2c");
  });

  it("sends keystrokes through the input scheduler before the first snapshot", async () => {
    const view = await mountViewport({ publishRuntime: false });
    await act(async () => view.fakeTerminal!.dataHandler!("echo ready\r"));
    await view.flushFrame();
    expect(testState.writeCommand.mock.calls.map(([request]) => request.input.data).join("")).toBe(
      "echo ready\r",
    );
  });

  it("releases a successful claim without waiting for an echo and keeps a pending RPC through an echo", async () => {
    testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    const view = await mountViewport();
    await act(async () =>
      view.runtime.ingest({
        type: "snapshot",
        snapshot: {
          ...terminalSnapshot(""),
          size: { cols: 151, rows: 50, sizeClaim: "other" },
        },
      }),
    );
    let settle!: (value: unknown) => void;
    view.resizeSpy.mockImplementation(
      () =>
        new Promise((resolve) => {
          settle = resolve;
        }),
    );
    const keypress = () =>
      view.fakeTerminal!.keyHandler?.(new KeyboardEvent("keydown", { key: "a" }));
    await act(async () => keypress());
    await act(async () => keypress());
    expect(view.resizeSpy).toHaveBeenCalledTimes(1);
    await act(async () => settle(AsyncResult.success(undefined)));
    await act(async () => keypress());
    expect(view.resizeSpy).toHaveBeenCalledTimes(2);
    const claim = view.resizeSpy.mock.calls[1]![0].input.sizeClaim as string;
    await act(async () =>
      view.runtime.ingest({
        type: "resized",
        threadId: THREAD_ID,
        terminalId: "term-1",
        size: { cols: 80, rows: 24, sizeClaim: claim },
      }),
    );
    xtermState.proposedDimensions = { cols: 91, rows: 42 };
    await view.triggerResizeEpoch();
    await view.flushFrame();
    expect(view.resizeSpy).toHaveBeenCalledTimes(2);
    await act(async () => settle(AsyncResult.success(undefined)));
    expect(view.resizeSpy).toHaveBeenCalledTimes(3);
    expect(view.resizeSpy.mock.calls[2]![0].input).toMatchObject({
      cols: 91,
      rows: 42,
      sizeClaim: claim,
    });
  });

  it("does not flash the notice between successful claim completion and asynchronous echo parsing", async () => {
    testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    xtermState.deferWrites = true;
    const view = await mountViewport();
    await act(async () =>
      view.runtime.ingest({
        type: "snapshot",
        snapshot: {
          ...terminalSnapshot(""),
          size: { cols: 151, rows: 50, sizeClaim: "other" },
        },
      }),
    );
    await flushTerminalWrites();
    expect(view.mounted.container.textContent).toContain("Sized for another window.");
    let settle!: (value: unknown) => void;
    view.resizeSpy.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          settle = resolve;
        }),
    );
    await act(async () => view.pointerDownOnSurface());
    const claim = view.resizeSpy.mock.calls.at(-1)![0].input.sizeClaim as string;
    await act(async () =>
      view.runtime.ingest({
        type: "resized",
        threadId: THREAD_ID,
        terminalId: "term-1",
        size: { cols: 80, rows: 24, sizeClaim: claim },
      }),
    );
    await act(async () => settle(AsyncResult.success(undefined)));
    // The RPC is settled, but xterm's ordered size barrier has not parsed yet.
    expect(view.fakeTerminal!.cols).toBe(151);
    expect(view.mounted.container.textContent).not.toContain("Sized for another window.");
    await flushTerminalWrites();
    expect(view.fakeTerminal!.cols).toBe(80);
    expect(view.mounted.container.textContent).not.toContain("Sized for another window.");
  });

  it.each(["size", "owner", "generation"] as const)(
    "clears no-echo claim notice suppression on the next %s change",
    async (change) => {
      testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
      vi.spyOn(document, "hasFocus").mockReturnValue(false);
      xtermState.deferWrites = true;
      const view = await mountViewport();
      const size = { cols: 151, rows: 50, sizeClaim: "other" };
      await act(async () =>
        view.runtime.ingest({ type: "snapshot", snapshot: { ...terminalSnapshot(""), size } }),
      );
      await flushTerminalWrites();
      await act(async () => view.pointerDownOnSurface());
      expect(view.resizeSpy).toHaveBeenCalledTimes(1);
      expect(view.mounted.container.textContent).not.toContain("Sized for another window.");
      await act(async () =>
        view.runtime.ingest({ type: "resized", threadId: THREAD_ID, terminalId: "term-1", size }),
      );
      await flushTerminalWrites();
      expect(view.mounted.container.textContent).not.toContain("Sized for another window.");
      await act(async () =>
        view.runtime.ingest(
          change === "generation"
            ? {
                type: "restarted",
                threadId: THREAD_ID,
                terminalId: "term-1",
                snapshot: { ...terminalSnapshot(""), size },
              }
            : {
                type: "resized",
                threadId: THREAD_ID,
                terminalId: "term-1",
                size: {
                  ...size,
                  ...(change === "size" ? { cols: 152 } : { sizeClaim: "next-owner" }),
                },
              },
        ),
      );
      await flushTerminalWrites();
      expect(view.mounted.container.textContent).toContain("Sized for another window.");
    },
  );

  it("keeps dismissal through visibility changes and component remount, then clears it on retirement", async () => {
    testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    const view = await mountViewport();
    await act(async () =>
      view.runtime.ingest({
        type: "snapshot",
        snapshot: {
          ...terminalSnapshot(""),
          size: { cols: 151, rows: 50, sizeClaim: "other" },
        },
      }),
    );
    const dismiss = view.mounted.container.querySelector<HTMLButtonElement>(
      '[aria-label="Dismiss terminal size notice"]',
    )!;
    await act(async () => dismiss.click());
    await view.setVisible(false);
    await view.setVisible(true);
    expect(view.mounted.container.textContent).not.toContain("Sized for another window.");
    await view.setDocumentVisible(false);
    await view.setDocumentVisible(true);
    expect(view.mounted.container.textContent).not.toContain("Sized for another window.");
    await unmount(view.mounted);
    const remounted = await mountViewport({ publishRuntime: false });
    await remounted.setTranscriptRuntime(view.runtime);
    expect(remounted.mounted.container.textContent).not.toContain("Sized for another window.");
    await unmount(remounted.mounted);
    releaseTerminalUiResources(ENVIRONMENT_ID, THREAD_ID, "term-1");
    const reopened = await mountViewport({ publishRuntime: false });
    await reopened.setTranscriptRuntime(view.runtime);
    expect(reopened.mounted.container.textContent).toContain("Sized for another window.");
  });

  it("a surviving renderer answers restarted startup queries live and guards the later remount", async () => {
    testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    xtermState.answerQueriesOnWrite = true;
    xtermState.deferWrites = true;
    const view = await mountViewport();
    await flushTerminalWrites();
    await act(async () =>
      view.runtime.ingest({
        type: "restarted",
        threadId: THREAD_ID,
        terminalId: "term-1",
        snapshot: {
          ...terminalSnapshot(""),
          size: { cols: 80, rows: 24, sizeClaim: null },
        },
      }),
    );
    await flushTerminalWrites();
    await act(async () => view.emitOutput("\x1b[6n\x1b[c"));
    await view.flushFrame();
    await flushTerminalWrites();
    await view.flushFrame();
    const written = () =>
      testState.writeCommand.mock.calls.map(([request]) => request.input.data).join("");
    expect(written()).toBe("\x1b[1;1R\x1b[?1;2c");
    await view.setVisible(false);
    await view.setVisible(true);
    await flushTerminalWrites();
    await view.flushFrame();
    expect(written()).toBe("\x1b[1;1R\x1b[?1;2c");
  });

  it("keeps a dismissed size notice hidden until the applied size or owner changes", async () => {
    testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    const view = await mountViewport();
    const resized = async (cols: number, sizeClaim: string) =>
      act(async () =>
        view.runtime.ingest({
          type: "resized",
          threadId: THREAD_ID,
          terminalId: "term-1",
          size: { cols, rows: 50, sizeClaim },
        }),
      );
    await resized(151, "other");
    const dismiss = () =>
      view.mounted.container.querySelector<HTMLButtonElement>(
        '[aria-label="Dismiss terminal size notice"]',
      );
    expect(dismiss()).not.toBeNull();
    await act(async () => dismiss()!.click());
    expect(view.mounted.container.textContent).not.toContain("Sized for another window.");
    await view.triggerResizeEpoch();
    await view.flushFrame();
    await resized(151, "other");
    expect(dismiss()).toBeNull();
    await resized(150, "other");
    expect(dismiss()).not.toBeNull();
    await act(async () => dismiss()!.click());
    await resized(150, "next-owner");
    expect(dismiss()).not.toBeNull();
  });

  it("skips measuring and resize requests for owner keypresses at the applied size", async () => {
    testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
    const view = await mountViewport();
    const claim = (testState.attachedSessionInputs.at(-1) as { terminal: { sizeClaim: string } })
      .terminal.sizeClaim;
    await act(async () =>
      view.runtime.ingest({
        type: "snapshot",
        snapshot: {
          ...terminalSnapshot(""),
          size: { cols: 80, rows: 24, sizeClaim: claim },
        },
      }),
    );
    xtermState.fitAddons[0]!.proposeDimensions.mockClear();
    await act(async () => {
      for (let index = 0; index < 100; index += 1)
        view.fakeTerminal!.keyHandler?.(new KeyboardEvent("keydown", { key: "a" }));
    });
    expect(xtermState.fitAddons[0]!.proposeDimensions).not.toHaveBeenCalled();
    expect(view.resizeSpy).not.toHaveBeenCalled();
  });

  it("answers startup DSR and DA immediately when its attach snapshot names this renderer", async () => {
    testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: true };
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    const view = await mountViewport();
    const claim = (testState.attachedSessionInputs.at(-1) as { terminal: { sizeClaim: string } })
      .terminal.sizeClaim;
    await act(async () =>
      view.runtime.ingest({
        type: "snapshot",
        snapshot: { ...terminalSnapshot(""), size: { cols: 80, rows: 24, sizeClaim: claim } },
      }),
    );
    const terminal = view.fakeTerminal!;
    expect(terminal.csiHandlers.get("n")!([6])).toBe(false);
    expect(terminal.csiHandlers.get("c")!([0])).toBe(false);
    await act(async () => {
      terminal.dataHandler!("\x1b[1;1R");
      terminal.dataHandler!("\x1b[?1;2c");
    });
    await view.flushFrame();
    expect(testState.writeCommand.mock.calls.map(([request]) => request.input.data).join("")).toBe(
      "\x1b[1;1R\x1b[?1;2c",
    );
    expect(view.resizeSpy).not.toHaveBeenCalled();
    expect(view.mounted.container.textContent).not.toContain("Fit to this window");
  });

  it.each([true, false])(
    "uses snapshot OSC responder activity with ownership capability %s",
    async (enabled) => {
      testState.serverConfig!.environment.capabilities = { terminalSizeOwnership: enabled };
      const view = await mountViewport();
      const claim =
        (testState.attachedSessionInputs.at(-1) as { terminal: { sizeClaim?: string } }).terminal
          .sizeClaim ?? "legacy";
      for (const active of [true, false]) {
        await act(async () =>
          view.runtime.ingest({
            type: "snapshot",
            snapshot: {
              ...terminalSnapshot(""),
              oscColorResponderActive: active,
              size: { cols: 80, rows: 24, sizeClaim: claim },
            },
          }),
        );
        for (const code of [10, 11, 12])
          expect(view.fakeTerminal!.oscHandlers.get(code)!("?")).toBe(active);
        expect(view.fakeTerminal!.csiHandlers.get("?n")!([996])).toBe(true);
      }
    },
  );

  it("refits when the terminal element changes size without a window resize", async () => {
    const view = await mountViewport({ visible: true });
    await view.flushFrame();
    await view.flushMountFit();
    view.resizeSpy.mockClear();

    const observer = resizeObserverInstances[0]!;
    expect(observer.observe).toHaveBeenCalledOnce();
    view.setFitDimensions(120, 40);
    observer.callback([], observer as never);
    await view.flushFrame();

    expect(view.resizeSpy).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-1", cols: 120, rows: 40 },
    });
  });

  it("performs one trailing renderer refresh after element resize settles", async () => {
    const view = await mountViewport({ visible: true });
    await view.flushFrame();
    await view.flushMountFit();
    const terminal = view.fakeTerminal!;
    terminal.clearTextureAtlas.mockClear();
    terminal.refresh.mockClear();
    let runTrailingRefresh: (() => void) | null = null;
    const trailingTimerSpy = vi.spyOn(window, "setTimeout").mockImplementation((handler, delay) => {
      if (delay === 50 && typeof handler === "function") {
        runTrailingRefresh = handler;
      }
      return 1 as unknown as ReturnType<typeof setTimeout>;
    });

    const observer = resizeObserverInstances[0]!;
    observer.callback([], observer as never);
    observer.callback([], observer as never);
    await view.flushFrame();

    expect(trailingTimerSpy.mock.calls.filter(([, delay]) => delay === 50)).toHaveLength(1);
    expect(terminal.clearTextureAtlas).not.toHaveBeenCalled();
    expect(terminal.refresh).not.toHaveBeenCalled();
    expect(runTrailingRefresh).not.toBeNull();
    await act(async () => runTrailingRefresh?.());

    expect(terminal.clearTextureAtlas).toHaveBeenCalledOnce();
    expect(terminal.refresh).toHaveBeenCalledWith(0, terminal.rows - 1);
  });

  it("issues one resize when either exact dimension changes and preserves real transitions", async () => {
    const view = await mountViewport({ visible: true });
    await view.flushFrame();
    await view.flushMountFit();

    view.setFitDimensions(120, 24);
    await view.triggerResizeEpoch();
    await view.flushFrame();
    view.setFitDimensions(120, 40);
    await view.triggerResizeEpoch();
    await view.flushFrame();
    view.setFitDimensions(80, 24);
    await view.triggerResizeEpoch();
    await view.flushFrame();

    expect(view.resizeSpy.mock.calls.map(([request]) => request.input)).toEqual([
      { threadId: THREAD_ID, terminalId: "term-1", cols: 80, rows: 24 },
      { threadId: THREAD_ID, terminalId: "term-1", cols: 120, rows: 24 },
      { threadId: THREAD_ID, terminalId: "term-1", cols: 120, rows: 40 },
      { threadId: THREAD_ID, terminalId: "term-1", cols: 80, rows: 24 },
    ]);
  });

  it("allows the same exact geometry to retry after a known resize failure", async () => {
    testState.resizeCommand
      .mockResolvedValueOnce(AsyncResult.failure(Cause.fail(new Error("resize rejected"))))
      .mockResolvedValue(AsyncResult.success(undefined));
    const view = await mountViewport({ visible: true });

    await view.flushFrame();
    await view.triggerResizeEpoch();
    await view.flushFrame();
    await view.flushMountFit();

    expect(view.resizeSpy).toHaveBeenCalledTimes(2);
    expect(view.resizeSpy.mock.calls[0]?.[0].input).toEqual(
      view.resizeSpy.mock.calls[1]?.[0].input,
    );
  });

  it("does not let an older failed resize clear a newer successful geometry", async () => {
    let resolveFirst!: (result: unknown) => void;
    testState.resizeCommand
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveFirst = resolve;
          }),
      )
      .mockResolvedValue(AsyncResult.success(undefined));
    const view = await mountViewport({ visible: true });
    await view.flushFrame();

    view.setFitDimensions(120, 40);
    await view.triggerResizeEpoch();
    await view.flushFrame();
    resolveFirst(AsyncResult.failure(Cause.fail(new Error("stale resize rejected"))));
    await act(async () => Promise.resolve());

    await view.triggerResizeEpoch();
    await view.flushFrame();

    expect(view.resizeSpy).toHaveBeenCalledTimes(2);
  });

  it("does not resize while hidden or from document-hidden stale callbacks", async () => {
    const hidden = await mountViewport({ visible: false });
    await hidden.flushFrame();
    await hidden.flushMountFit();
    expect(hidden.resizeSpy).not.toHaveBeenCalled();
    await unmount(hidden.mounted);

    const view = await mountViewport({ visible: true });
    await view.flushFrame();
    expect(view.resizeSpy).toHaveBeenCalledOnce();

    await view.triggerResizeEpoch();
    const staleResizeFrame = Array.from(animationFrames.values())[0]!;
    await view.setDocumentVisible(false);
    staleResizeFrame(0);
    await act(async () => Promise.resolve());
    await view.triggerResizeEpoch();
    await view.flushFrame();

    expect(view.resizeSpy).toHaveBeenCalledOnce();
    expect(view.fakeTerminal).toBeNull();
  });

  it("resets the exact-size guard when teardown mounts a replacement renderer", async () => {
    const view = await mountViewport({ visible: true });
    await view.flushFrame();
    await view.flushMountFit();
    expect(view.resizeSpy).toHaveBeenCalledOnce();

    await view.setVisible(false);
    await view.setVisible(true);
    await view.flushFrame();
    await view.flushMountFit();

    expect(view.resizeSpy).toHaveBeenCalledTimes(2);
    expect(view.resizeSpy.mock.calls[0]?.[0].input).toEqual(
      view.resizeSpy.mock.calls[1]?.[0].input,
    );
  });

  it("generation-guards a failed resize completion from a replaced renderer", async () => {
    let resolveFirst!: (result: unknown) => void;
    testState.resizeCommand
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveFirst = resolve;
          }),
      )
      .mockResolvedValue(AsyncResult.success(undefined));
    const view = await mountViewport({ visible: true });
    await view.flushFrame();

    await view.setVisible(false);
    await view.setVisible(true);
    await view.flushFrame();
    resolveFirst(AsyncResult.failure(Cause.fail(new Error("old renderer rejected"))));
    await act(async () => Promise.resolve());
    await view.triggerResizeEpoch();
    await view.flushFrame();

    expect(view.resizeSpy).toHaveBeenCalledTimes(2);
  });

  it("keeps exact-size guards isolated between split terminal viewports", async () => {
    const firstProps = viewportProps({ terminalId: "term-split-a" });
    const secondProps = viewportProps({ terminalId: "term-split-b" });
    const mounted = await mount(
      <>
        <TerminalViewport {...firstProps} />
        <TerminalViewport {...secondProps} />
      </>,
    );
    await flushAnimationFrames();
    await flushComponentFitTimers();

    expect(testState.resizeCommand.mock.calls.map(([request]) => request.input.terminalId)).toEqual(
      ["term-split-a", "term-split-b"],
    );

    xtermState.terminals[0]!.cols = 132;
    await act(async () =>
      mounted.root.render(
        <>
          <TerminalViewport {...firstProps} resizeEpoch={1} />
          <TerminalViewport {...secondProps} resizeEpoch={1} />
        </>,
      ),
    );
    await flushAnimationFrames();

    expect(
      testState.resizeCommand.mock.calls.map(([request]) => [
        request.input.terminalId,
        request.input.cols,
        request.input.rows,
      ]),
    ).toEqual([
      ["term-split-a", 80, 24],
      ["term-split-b", 80, 24],
      ["term-split-a", 132, 24],
    ]);
  });

  it("does not resize for focus, theme, output, metadata, or WebGL changes", async () => {
    const view = await mountViewport({
      visible: true,
      autoFocus: true,
      webglEnabled: true,
      deferWebglImport: true,
    });
    await view.flushFrame();
    await view.flushMountFit();
    expect(view.resizeSpy).toHaveBeenCalledOnce();

    await view.requestFocus(1);
    view.emitOutput("background output\n");
    await view.setSessionMetadata({ error: "background diagnostic" });
    const observer = observerInstances.at(-1)!;
    observer.callback([], observer as never);
    await view.resolveWebglImport();
    await view.setWebglEnabled(false);
    await view.setWebglEnabled(true);
    await view.settleWebgl();
    await view.rerenderUnrelated();
    await view.flushFrame();

    expect(view.resizeSpy).toHaveBeenCalledOnce();
  });

  it("focuses the active renderer only after its initial fit frame", async () => {
    const view = await mountViewport({ visible: true, autoFocus: true });
    const terminal = view.fakeTerminal!;

    expect(xtermState.fitAddons[0]?.fit).toHaveBeenCalled();
    expect(terminal.focus).not.toHaveBeenCalled();

    await view.flushFrame();

    expect(terminal.focus).toHaveBeenCalledOnce();
  });

  it("focuses after a delayed attach runtime mounts the active renderer", async () => {
    const view = await mountViewport({
      visible: true,
      autoFocus: true,
      publishRuntime: false,
    });
    expect(view.fakeTerminal?.displayedText).toBe("");

    await view.publishRuntime();
    const terminal = view.fakeTerminal!;

    expect(xtermState.fitAddons[0]?.fit).toHaveBeenCalled();
    expect(terminal.focus).not.toHaveBeenCalled();

    await view.flushFrame();
    expect(terminal.focus).toHaveBeenCalledOnce();
  });

  it("keeps the same WebGL context when the transcript reattaches to the mounted terminal", async () => {
    const view = await mountViewport({ webglEnabled: true });
    await view.settleWebgl();
    const terminal = view.fakeTerminal;
    const addon = view.webglAddon!;
    expect(webglState.instances).toHaveLength(1);
    await view.replaceRuntime();
    await view.settleWebgl();
    expect(view.fakeTerminal).toBe(terminal);
    expect(view.webglAddon).toBe(addon);
    expect(webglState.instances).toHaveLength(1);
    expect(addon.disposeSpy).not.toHaveBeenCalled();
  });

  it("does not refocus a fulfilled activation when the transcript runtime reconnects", async () => {
    const view = await mountViewport({
      visible: true,
      autoFocus: true,
      focusRequestId: 7,
    });
    const initialTerminal = view.fakeTerminal!;
    await view.flushFrame();
    expect(initialTerminal.focus).toHaveBeenCalledOnce();
    initialTerminal.focus.mockClear();

    await view.replaceRuntime();
    const replacementTerminal = view.fakeTerminal!;
    expect(replacementTerminal).toBe(initialTerminal);
    await view.flushFrame();

    expect(initialTerminal.focus).not.toHaveBeenCalled();
    expect(replacementTerminal.focus).not.toHaveBeenCalled();

    await view.requestFocus(8);
    await view.flushFrame();
    expect(replacementTerminal.focus).toHaveBeenCalledOnce();
  });

  it("focuses pointer-down on the terminal surface without clearing selection", async () => {
    const view = await mountViewport({ visible: true, autoFocus: false });
    const terminal = view.fakeTerminal!;
    terminal.hasActiveSelection = true;
    terminal.clearSelection.mockClear();

    view.pointerDownOutsideSurface();
    expect(terminal.focus).not.toHaveBeenCalled();

    view.pointerDownOnSurface();

    expect(terminal.focus).toHaveBeenCalledOnce();
    expect(terminal.clearSelection).not.toHaveBeenCalled();
  });

  it("does not focus an inactive renderer when its activation token changes", async () => {
    const view = await mountViewport({ visible: true, autoFocus: false });
    const terminal = view.fakeTerminal!;

    await view.requestFocus(1);
    await view.flushFrame();

    expect(terminal.focus).not.toHaveBeenCalled();

    await view.setAutoFocus(true);
    expect(terminal.focus).not.toHaveBeenCalled();
    await view.flushFrame();
    expect(terminal.focus).toHaveBeenCalledOnce();
  });

  it("keeps an ineligible focus request outstanding until the pane becomes eligible", async () => {
    const view = await mountViewport({ visible: true, autoFocus: false, focusRequestId: 7 });
    const terminal = view.fakeTerminal!;

    await view.flushFrame();
    expect(terminal.focus).not.toHaveBeenCalled();

    await view.setAutoFocus(true);
    await view.flushFrame();
    expect(terminal.focus).toHaveBeenCalledOnce();

    terminal.focus.mockClear();
    await view.setAutoFocus(false);
    await view.setAutoFocus(true);
    await view.flushFrame();
    expect(terminal.focus).not.toHaveBeenCalled();
  });

  it("does not steal focus for output, metadata, theme, or unrelated rerenders", async () => {
    const view = await mountViewport({ visible: true, autoFocus: true });
    const terminal = view.fakeTerminal!;
    await view.flushFrame();
    terminal.focus.mockClear();

    view.emitOutput("background output\n");
    await view.flushFrame();
    await view.setSessionMetadata({ error: "background diagnostic" });
    await view.setSessionMetadata({ status: "closed" });
    const observer = observerInstances.at(-1)!;
    observer.callback([], observer as never);
    await view.rerenderUnrelated();
    await view.flushFrame();

    expect(terminal.focus).not.toHaveBeenCalled();
  });

  it("does not steal focus when WebGL resolves, toggles, or loses context", async () => {
    const view = await mountViewport({
      visible: true,
      autoFocus: true,
      webglEnabled: true,
      deferWebglImport: true,
    });
    const terminal = view.fakeTerminal!;
    await view.flushFrame();
    terminal.focus.mockClear();

    await view.resolveWebglImport();
    await view.setWebglEnabled(false);
    await view.setWebglEnabled(true);
    await view.settleWebgl();
    view.webglAddon!.triggerContextLoss();

    expect(terminal.focus).not.toHaveBeenCalled();
  });

  it("generation-guards canceled focus frames across rapid activation tokens", async () => {
    const view = await mountViewport({ visible: true, autoFocus: true });
    const terminal = view.fakeTerminal!;
    await view.flushFrame();
    terminal.focus.mockClear();

    await view.requestFocus(1);
    const staleFocusFrame = Array.from(animationFrames.values())[0]!;
    await view.requestFocus(2);

    staleFocusFrame(0);
    expect(terminal.focus).not.toHaveBeenCalled();

    await view.flushFrame();
    expect(terminal.focus).toHaveBeenCalledOnce();
  });

  it("generation-guards a canceled focus frame across hide and renderer replacement", async () => {
    const view = await mountViewport({ visible: true, autoFocus: true });
    const firstTerminal = view.fakeTerminal!;
    await view.flushFrame();
    firstTerminal.focus.mockClear();

    await view.requestFocus(1);
    const staleFocusFrame = Array.from(animationFrames.values())[0]!;
    await view.setVisible(false);
    await view.setVisible(true);
    const replacementTerminal = view.fakeTerminal!;

    staleFocusFrame(0);
    expect(firstTerminal.focus).not.toHaveBeenCalled();
    expect(replacementTerminal.focus).not.toHaveBeenCalled();

    await view.flushFrame();
    expect(replacementTerminal.focus).toHaveBeenCalledOnce();
  });

  it("generation-guards a canceled focus frame while the document is hidden", async () => {
    const view = await mountViewport({ visible: true, autoFocus: true });
    const terminal = view.fakeTerminal!;
    await view.flushFrame();
    terminal.focus.mockClear();

    await view.requestFocus(1);
    const staleFocusFrame = Array.from(animationFrames.values())[0]!;
    await view.setDocumentVisible(false);

    staleFocusFrame(0);

    expect(terminal.focus).not.toHaveBeenCalled();
    expect(view.fakeTerminal).toBeNull();

    await view.setDocumentVisible(true);
    const replacementTerminal = view.fakeTerminal!;
    expect(replacementTerminal.focus).not.toHaveBeenCalled();
    await view.flushFrame();
    expect(replacementTerminal.focus).toHaveBeenCalledOnce();
  });

  it("keeps cursor blink and a visible cursor theme in light and dark mode", async () => {
    const lightView = await mountViewport({ visible: true });
    const lightTerminal = lightView.fakeTerminal!;
    const lightTheme = lightTerminal.options.theme as { readonly cursor?: unknown };

    expect(lightTerminal.options.cursorBlink).toBe(true);
    expect(lightTheme.cursor).toEqual(expect.stringMatching(/\S/));
    await unmount(lightView.mounted);

    testState.resolvedTheme = "dark";
    document.documentElement.classList.add("dark");
    try {
      const darkView = await mountViewport({ visible: true });
      const darkTerminal = darkView.fakeTerminal!;
      const darkTheme = darkTerminal.options.theme as { readonly cursor?: unknown };

      expect(darkTerminal.options.cursorBlink).toBe(true);
      expect(darkTheme.cursor).toEqual(expect.stringMatching(/\S/));
    } finally {
      document.documentElement.classList.remove("dark");
    }
  });

  it("attaches a deferred import while its generation stays current", async () => {
    const view = await mountViewport({
      visible: true,
      webglEnabled: true,
      deferWebglImport: true,
    });
    await view.resolveWebglImport();
    expect(webglState.instances).toHaveLength(1);
  });

  it("reattaches webgl to the replacement terminal after launch parameters change", async () => {
    const view = await mountViewport({ visible: true, webglEnabled: true });
    await settleWebgl();
    expect(webglState.instances).toHaveLength(1);

    // A cwd change recreates the xterm instance; the replacement must get its
    // own webgl addon instead of silently falling back to the DOM renderer.
    await view.setCwd("C:\\somewhere\\else");
    await settleWebgl();

    expect(xtermState.terminals).toHaveLength(2);
    expect(webglState.instances).toHaveLength(2);
  });

  it("merges launch command env into the spawn env with runtime env winning", async () => {
    testState.attachedSessionInputs.length = 0;
    await mount(
      <TerminalViewport
        {...viewportProps({
          command: decodeTerminalLaunchCommand({
            executable: "opencode",
            args: [],
            env: { OPENCODE_CONFIG_CONTENT: '{"theme":"system"}', SHARED: "command" },
          })!,
          runtimeEnv: { SHARED: "runtime" },
        })}
      />,
    );

    const spec = testState.attachedSessionInputs.at(-1) as {
      terminal: { env?: Record<string, string> };
    };
    expect(spec.terminal.env).toMatchObject({
      OPENCODE_CONFIG_CONTENT: '{"theme":"system"}',
      SHARED: "runtime",
    });
  });

  it("supplies resolved theme colors as reserved OSC env for the PTY responder", async () => {
    testState.attachedSessionInputs.length = 0;
    testState.resolvedTheme = "light";
    document.documentElement.classList.add("dark");
    await mount(<TerminalViewport {...viewportProps()} />);
    const lightSpec = testState.attachedSessionInputs.at(-1) as {
      terminal: { env?: Record<string, string> };
    };
    expect(lightSpec.terminal.env).toMatchObject({
      BIBCODE_OSC_BACKGROUND: "255,255,255",
      BIBCODE_OSC_FOREGROUND: "28,33,41",
    });

    testState.attachedSessionInputs.length = 0;
    testState.resolvedTheme = "dark";
    try {
      await mount(<TerminalViewport {...viewportProps({ terminalId: "term-osc-dark" })} />);
    } finally {
      document.documentElement.classList.remove("dark");
    }
    const darkSpec = testState.attachedSessionInputs.at(-1) as {
      terminal: { env?: Record<string, string> };
    };
    expect(darkSpec.terminal.env).toMatchObject({ BIBCODE_OSC_BACKGROUND: "14,18,24" });
  });

  it("limits the persistent Windows console theme to Codex", async () => {
    testState.attachedSessionInputs.length = 0;
    testState.resolvedTheme = "light";
    await mount(
      <TerminalViewport
        {...viewportProps({
          terminalId: "term-codex-windows-console-theme",
          command: decodeTerminalLaunchCommand({
            executable: "codex.exe",
            args: [],
            env: {
              BIBCODE_OSC_BACKGROUND: "0,0,0",
              BIBCODE_WINDOWS_CONSOLE_THEME: "command",
            },
          })!,
          runtimeEnv: {
            BIBCODE_OSC_FOREGROUND: "0,0,0",
            BIBCODE_WINDOWS_CONSOLE_THEME: "runtime",
          },
        })}
      />,
    );
    const codexSpec = testState.attachedSessionInputs.at(-1) as {
      terminal: { env?: Record<string, string> };
    };
    expect(codexSpec.terminal.env).toMatchObject({
      BIBCODE_OSC_BACKGROUND: "255,255,255",
      BIBCODE_OSC_FOREGROUND: "28,33,41",
      BIBCODE_OSC_CURSOR: "38,56,78",
      BIBCODE_WINDOWS_CONSOLE_THEME: "light",
    });

    for (const [terminalId, executable, args, expected] of [
      ["term-cursor-windows-console-theme", "cursor-agent", ["--yolo"], undefined],
      [
        "term-claude-windows-console-theme",
        "claude",
        ["--dangerously-skip-permissions"],
        undefined,
      ],
      ["term-opencode-windows-console-theme", "opencode", [], undefined],
    ] as const) {
      testState.attachedSessionInputs.length = 0;
      await mount(
        <TerminalViewport
          {...viewportProps({
            terminalId,
            command: decodeTerminalLaunchCommand({ executable, args: [...args] })!,
          })}
        />,
      );
      const spec = testState.attachedSessionInputs.at(-1) as {
        terminal: { env?: Record<string, string> };
      };
      expect(spec.terminal.env?.BIBCODE_WINDOWS_CONSOLE_THEME).toBe(expected);
    }
  });

  it("records the Codex console theme marker on a non-Windows host", async () => {
    testState.serverConfig = {
      availableEditors: ["vscode"],
      environment: { platform: { os: "darwin" } },
    };
    testState.attachedSessionInputs.length = 0;
    await mount(
      <TerminalViewport
        {...viewportProps({
          terminalId: "term-codex-darwin-console-theme",
          command: decodeTerminalLaunchCommand({ executable: "codex", args: [] })!,
        })}
      />,
    );

    const spec = testState.attachedSessionInputs.at(-1) as {
      terminal: { env?: Record<string, string> };
    };
    expect(spec.terminal.env).toMatchObject({
      BIBCODE_WINDOWS_CONSOLE_THEME: "light",
    });
  });

  it("waits for authoritative host config before attaching Codex", async () => {
    testState.serverConfig = null;
    testState.resolvedTheme = "dark";
    const props = viewportProps({
      terminalId: "term-codex-config-loading",
      command: decodeTerminalLaunchCommand({ executable: "codex.exe", args: [] })!,
    });
    const mounted = await mount(<TerminalViewport {...props} />);

    expect(testState.attachedSessionInputs.at(-1)).toMatchObject({ attach: false });

    testState.serverConfig = {
      availableEditors: ["vscode"],
      environment: { platform: { os: "windows" } },
    };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    expect(testState.attachedSessionInputs.at(-1)).toMatchObject({
      attach: true,
      terminal: {
        env: expect.objectContaining({ BIBCODE_WINDOWS_CONSOLE_THEME: "dark" }),
      },
    });
  });

  it("uses the current theme when a hidden Windows Codex terminal first attaches", async () => {
    testState.resolvedTheme = "dark";
    const command = decodeTerminalLaunchCommand({ executable: "codex.exe", args: [] })!;
    const hiddenProps = viewportProps({
      terminalId: "term-hidden-codex-theme",
      command,
      visible: false,
    });
    const mounted = await mount(<TerminalViewport {...hiddenProps} />);
    expect(testState.attachedSessionInputs.at(-1)).toMatchObject({ attach: false });

    testState.resolvedTheme = "light";
    await act(async () =>
      mounted.root.render(<TerminalViewport {...hiddenProps} visible={true} />),
    );

    expect(testState.attachedSessionInputs.at(-1)).toMatchObject({
      attach: true,
      terminal: {
        env: expect.objectContaining({
          BIBCODE_OSC_BACKGROUND: "255,255,255",
          BIBCODE_WINDOWS_CONSOLE_THEME: "light",
        }),
      },
    });
  });

  it("attaches the replacement generation after a deferred import is disabled and enabled", async () => {
    const view = await mountViewport({
      visible: true,
      webglEnabled: true,
      deferWebglImport: true,
    });
    await view.setWebglEnabled(false);
    await view.setWebglEnabled(true);
    await view.resolveWebglImport();
    expect(webglState.importCount).toBe(2);
    expect(webglState.instances).toHaveLength(1);
  });

  it("attaches the replacement generation after a deferred import is hidden and shown", async () => {
    const view = await mountViewport({
      visible: true,
      webglEnabled: true,
      deferWebglImport: true,
    });
    await view.setVisible(false);
    await view.setVisible(true);
    await view.resolveWebglImport();
    expect(webglState.importCount).toBe(2);
    expect(webglState.instances).toHaveLength(1);
  });

  it("ignores a rejected hidden generation and loads WebGL after the viewport is shown", async () => {
    const diagnosticSpy = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const view = await mountViewport({
      visible: true,
      webglEnabled: true,
      deferWebglImport: true,
    });
    const rejectStaleImport = webglState.rejectImport!;

    await view.setVisible(false);
    webglState.importGate = null;
    webglState.resolveImport = null;
    webglState.rejectImport = null;
    await view.setVisible(true);
    await view.settleWebgl();
    const currentAddon = view.webglAddon!;

    rejectStaleImport(new Error("sensitive stale import failure"));
    await view.settleWebgl();

    expect(webglState.importCount).toBe(2);
    expect(webglState.instances).toHaveLength(1);
    expect(currentAddon.disposeSpy).not.toHaveBeenCalled();
    expect(diagnosticSpy).not.toHaveBeenCalled();

    await view.setWebglEnabled(false);
    await view.setWebglEnabled(true);
    await view.settleWebgl();

    expect(webglState.importCount).toBe(3);
    expect(webglState.instances).toHaveLength(2);
    expect(view.webglAddon).not.toBe(currentAddon);
    expect(diagnosticSpy).not.toHaveBeenCalled();
  });

  it("ignores a rejected disabled generation and loads WebGL after the setting is enabled", async () => {
    const diagnosticSpy = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const view = await mountViewport({
      visible: true,
      webglEnabled: true,
      deferWebglImport: true,
    });

    await view.setWebglEnabled(false);
    await view.rejectWebglImport();
    await view.setWebglEnabled(true);
    await view.settleWebgl();

    expect(webglState.importCount).toBe(2);
    expect(webglState.instances).toHaveLength(1);
    expect(view.webglAddon).not.toBeNull();
    expect(diagnosticSpy).not.toHaveBeenCalled();
  });

  it("guards a late import across hide, disable, and rapid remount generations", async () => {
    const view = await mountViewport({
      visible: true,
      webglEnabled: true,
      deferWebglImport: true,
    });
    const firstTerminal = view.fakeTerminal!;

    await view.setWebglEnabled(false);
    await view.setWebglEnabled(true);
    await view.setVisible(false);
    await view.setVisible(true);
    const staleRemountTerminal = view.fakeTerminal!;
    await view.setVisible(false);
    await view.setVisible(true);
    const currentRemountTerminal = view.fakeTerminal!;

    await view.resolveWebglImport();

    expect(firstTerminal.loadAddon).not.toHaveBeenCalledWith(
      expect.objectContaining({ kind: "webgl" }),
    );
    expect(staleRemountTerminal.loadAddon).not.toHaveBeenCalledWith(
      expect.objectContaining({ kind: "webgl" }),
    );
    expect(webglState.importCount).toBe(4);
    expect(webglState.events).toContain("addon.construct");
    expect(webglState.instances).toHaveLength(1);
    expect(currentRemountTerminal.loadAddon).toHaveBeenCalledWith(webglState.instances[0]);
  });

  it("loads the WebGL addon only for a visible terminal when the setting is on", async () => {
    const view = await mountViewport({ visible: true, webglEnabled: true });

    await view.settleWebgl();

    expect(view.webglAddon).not.toBeNull();
    expect(view.fakeTerminal?.loadAddon).toHaveBeenCalledWith(view.webglAddon);
  });

  it("never imports WebGL while the setting is off or the viewport is hidden", async () => {
    const disabled = await mountViewport({ visible: true, webglEnabled: false });
    await disabled.settleWebgl();
    expect(disabled.webglImported).toBe(false);
    await unmount(disabled.mounted);

    const hidden = await mountViewport({ visible: false, webglEnabled: true });
    await hidden.settleWebgl();
    expect(hidden.webglImported).toBe(false);
  });

  it("falls back once on context loss and does not retry in the same viewport lifetime", async () => {
    const diagnosticSpy = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const view = await mountViewport({ visible: true, webglEnabled: true });
    await view.settleWebgl();
    const addon = view.webglAddon!;

    addon.triggerContextLoss();
    addon.triggerContextLoss();
    await view.setWebglEnabled(false);
    await view.setWebglEnabled(true);
    await view.settleWebgl();

    expect(addon.disposeSpy).toHaveBeenCalledOnce();
    expect(diagnosticSpy).toHaveBeenCalledOnce();
    expect(diagnosticSpy).toHaveBeenCalledWith(
      "[terminal] WebGL renderer unavailable; using the standard renderer.",
    );
    expect(webglState.instances).toHaveLength(1);
    expect(view.fakeTerminal).not.toBeNull();
  });

  it("contains synchronous context loss even when late listener cleanup throws", async () => {
    const diagnosticSpy = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    webglState.listenerTriggersSynchronously = true;
    webglState.listenerDisposeShouldThrow = true;
    const view = await mountViewport({ visible: true, webglEnabled: true });
    const terminal = view.fakeTerminal!;

    await view.settleWebgl();
    const addon = view.webglAddon!;

    expect(addon.disposeSpy).toHaveBeenCalledOnce();
    expect(addon.listenerDisposeSpy).toHaveBeenCalledOnce();
    expect(webglState.loseContextSpy).toHaveBeenCalledOnce();
    expect(diagnosticSpy).toHaveBeenCalledOnce();
    expect(view.fakeTerminal).toBe(terminal);
    expect(terminal.dispose).not.toHaveBeenCalled();
  });

  it("keeps the WebGL viewport and shader resolution aligned when correcting the backing store", async () => {
    // A Retina ratio; at dpr 1 the CSS box and the backing store cannot disagree.
    vi.stubGlobal("devicePixelRatio", 2);
    const view = await mountViewport({ visible: true, webglEnabled: true });
    await view.settleWebgl();
    const canvas = webglState.canvas!;
    const observer = resizeObserverInstances.find(
      (candidate) => candidate.observe.mock.calls[0]?.[0] === canvas,
    )!;
    expect(observer).toBeDefined();

    observer.callback([], observer as unknown as ResizeObserver);

    // 608 CSS px at dpr 2 is 1216 device px, not the 1215 xterm left behind.
    expect(canvas.width).toBe(1216);
    expect(canvas.height).toBe(600);
    expect(webglState.drawingViewport).toEqual({ width: 1216, height: 600 });
    expect(webglState.glyphResolution).toEqual({ width: 1216, height: 600 });

    await view.setVisible(false);
    expect(observer.disconnect).toHaveBeenCalled();
  });

  it("skips the correction when the addon exposes no measurable canvas", async () => {
    const view = await mountViewport({
      visible: true,
      webglEnabled: true,
      webglContextMode: "missing-canvas",
    });
    await view.settleWebgl();

    expect(view.webglAddon!.disposeSpy).not.toHaveBeenCalled();
    expect(
      resizeObserverInstances.some(
        (candidate) => candidate.observe.mock.calls[0]?.[0] === webglState.canvas,
      ),
    ).toBe(false);
  });

  it("releases the real WebGL context before disposing xterm on hide", async () => {
    const view = await mountViewport({ visible: true, webglEnabled: true });
    await view.settleWebgl();
    const addon = view.webglAddon!;

    await view.setVisible(false);

    expect(addon.disposeSpy).toHaveBeenCalledOnce();
    expect(webglState.loseContextSpy).toHaveBeenCalledOnce();
    expect(
      webglState.events.filter((event) =>
        ["addon.dispose", "context.lose", "terminal.dispose"].includes(event),
      ),
    ).toEqual(["addon.dispose", "context.lose", "terminal.dispose"]);
  });

  it("toggles WebGL without recreating xterm, detaching the producer, or losing history", async () => {
    const view = await mountViewport({
      visible: true,
      webglEnabled: false,
      initialSnapshot: "retained history",
    });
    const terminal = view.fakeTerminal!;
    const snapshot = view.runtime.snapshot();

    await view.setWebglEnabled(true);
    await view.settleWebgl();
    const firstAddon = view.webglAddon!;
    await view.setWebglEnabled(false);

    expect(view.fakeTerminal).toBe(terminal);
    expect(firstAddon.disposeSpy).toHaveBeenCalledOnce();
    expect(webglState.loseContextSpy).toHaveBeenCalledOnce();
    expect(view.detachRendererSpy).not.toHaveBeenCalled();
    expect(view.closeSessionSpy).not.toHaveBeenCalled();
    expect(view.runtime.snapshot()).toBe(snapshot);

    await view.setWebglEnabled(true);
    await view.settleWebgl();

    expect(view.fakeTerminal).toBe(terminal);
    expect(xtermState.terminals).toHaveLength(1);
    expect(webglState.instances).toHaveLength(2);
    expect(view.webglAddon).not.toBe(firstAddon);
    expect(view.detachRendererSpy).not.toHaveBeenCalled();
    expect(view.closeSessionSpy).not.toHaveBeenCalled();
    expect(view.runtime.snapshot()).toBe(snapshot);
  });

  it.each([
    [
      "construction",
      () => {
        webglState.constructorShouldThrow = true;
      },
    ],
    [
      "xterm load",
      () => {
        xtermState.loadWebglShouldThrow = true;
      },
    ],
    [
      "activation",
      () => {
        webglState.activateShouldThrow = true;
      },
    ],
    [
      "context-listener registration",
      () => {
        webglState.listenerShouldThrow = true;
      },
    ],
  ])(
    "falls back after a %s failure, diagnoses without raw errors, and does not retry",
    async (_failure, configureFailure) => {
      const diagnosticSpy = vi.spyOn(console, "warn").mockImplementation(() => undefined);
      configureFailure();
      const view = await mountViewport({ visible: true, webglEnabled: true });
      const terminal = view.fakeTerminal!;

      await view.settleWebgl();
      const importCountAfterFailure = webglState.importCount;
      await view.setWebglEnabled(false);
      await view.setWebglEnabled(true);
      await view.settleWebgl();

      expect(view.fakeTerminal).toBe(terminal);
      expect(terminal.dispose).not.toHaveBeenCalled();
      expect(webglState.importCount).toBe(importCountAfterFailure);
      expect(diagnosticSpy).toHaveBeenCalledOnce();
      expect(diagnosticSpy).toHaveBeenCalledWith(
        "[terminal] WebGL renderer unavailable; using the standard renderer.",
      );
      expect(diagnosticSpy.mock.calls.flat().join(" ")).not.toContain("sensitive");
    },
  );

  it("falls back after an import rejection and keeps the diagnostic error-free", async () => {
    const diagnosticSpy = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const view = await mountViewport({
      visible: true,
      webglEnabled: true,
      deferWebglImport: true,
    });
    const terminal = view.fakeTerminal!;

    await view.rejectWebglImport();
    await view.setWebglEnabled(false);
    await view.setWebglEnabled(true);
    await view.settleWebgl();

    expect(view.fakeTerminal).toBe(terminal);
    expect(webglState.importCount).toBe(1);
    expect(webglState.instances).toHaveLength(0);
    expect(diagnosticSpy).toHaveBeenCalledOnce();
    expect(diagnosticSpy.mock.calls.flat().join(" ")).not.toContain("sensitive");
  });

  it("allows a fresh viewport component to retry after a prior component failed", async () => {
    const diagnosticSpy = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    webglState.constructorShouldThrow = true;
    const failed = await mountViewport({ visible: true, webglEnabled: true });
    await failed.settleWebgl();
    await unmount(failed.mounted);

    webglState.constructorShouldThrow = false;
    const retried = await mountViewport({ visible: true, webglEnabled: true });
    await retried.settleWebgl();

    expect(retried.webglAddon).not.toBeNull();
    expect(webglState.importCount).toBe(2);
    expect(diagnosticSpy).toHaveBeenCalledOnce();
  });

  it.each([
    ["missing-context", 0],
    ["missing-extension", 0],
    ["getter-throws", 0],
    ["extension-throws", 0],
    ["lose-throws", 1],
  ] as const)(
    "keeps xterm usable when WebGL cleanup encounters %s",
    async (webglContextMode, expectedLoseCalls) => {
      const diagnosticSpy = vi.spyOn(console, "warn").mockImplementation(() => undefined);
      const view = await mountViewport({
        visible: true,
        webglEnabled: true,
        webglContextMode,
      });
      await view.settleWebgl();
      const terminal = view.fakeTerminal!;
      const addon = view.webglAddon!;

      await expect(view.setVisible(false)).resolves.toBeUndefined();

      expect(addon.disposeSpy).toHaveBeenCalledOnce();
      expect(webglState.loseContextSpy).toHaveBeenCalledTimes(expectedLoseCalls);
      expect(terminal.dispose).toHaveBeenCalledOnce();
      expect(view.closeSessionSpy).not.toHaveBeenCalled();
      expect(diagnosticSpy).not.toHaveBeenCalled();
    },
  );

  it("continues context release and terminal cleanup when addon disposal throws", async () => {
    const diagnosticSpy = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    webglState.disposeShouldThrow = true;
    const view = await mountViewport({ visible: true, webglEnabled: true });
    await view.settleWebgl();
    const terminal = view.fakeTerminal!;
    const addon = view.webglAddon!;

    await expect(view.setVisible(false)).resolves.toBeUndefined();

    expect(addon.disposeSpy).toHaveBeenCalledOnce();
    expect(webglState.loseContextSpy).toHaveBeenCalledOnce();
    expect(terminal.dispose).toHaveBeenCalledOnce();
    expect(diagnosticSpy).not.toHaveBeenCalled();
  });

  it("does not create xterm while hidden and creates it when shown", async () => {
    const view = await mountViewport({ visible: false });
    expect(view.fakeTerminal).toBeNull();
    expect(observerInstances).toHaveLength(0);
    expect(testState.attachedSessionInputs.at(-1)).toMatchObject({ attach: false });

    await view.setVisible(true);

    expect(view.fakeTerminal).not.toBeNull();
    expect(view.fakeTerminal?.open).toHaveBeenCalled();
    expect(testState.attachedSessionInputs.at(-1)).toMatchObject({ attach: true });
  });

  it("measures xterm before attach and waits for the producer before hydrating it", async () => {
    const view = await mountViewport({ visible: true, publishRuntime: false });
    const terminal = view.fakeTerminal!;
    expect(terminal.open).toHaveBeenCalledOnce();
    expect(terminal.writes).toEqual([]);

    await view.publishRuntime();

    expect(view.fakeTerminal).toBe(terminal);
    expect(terminal.resetCount).toBe(1);
  });

  it("renders attach failures even when no transcript runtime was published", async () => {
    const view = await mountViewport({ visible: true, publishRuntime: false });
    testState.session = {
      ...testState.session,
      error: "attach failed",
      status: "error",
      transcriptRuntime: null,
    };

    await view.setTranscriptRuntime(null);

    const alert = view.mounted.container.querySelector('[role="alert"]');
    expect(alert?.textContent).toContain("attach failed");
    expect(view.fakeTerminal?.displayedText).toBe("");
  });

  it("disposes xterm and the renderer attachment when hidden without closing the session", async () => {
    const view = await mountViewport({ visible: true });
    const terminal = view.fakeTerminal!;
    testState.session = {
      ...testState.session,
      status: "closed",
      transcriptRuntime: null,
    };

    await view.setVisible(false);

    expect(terminal.dispose).toHaveBeenCalledOnce();
    expect(view.detachRendererSpy).toHaveBeenCalledOnce();
    expect(view.closeSessionSpy).not.toHaveBeenCalled();
    expect(view.fakeTerminal).toBeNull();
  });

  it("seeds the bounded snapshot and applies live deltas through the imperative sink", async () => {
    const view = await mountViewport({ visible: true, initialSnapshot: "boot\n" });
    const terminal = view.fakeTerminal!;
    expect(terminal.writes).toContain("boot\n");

    view.emitOutput("a");
    expect(terminal.writes).not.toContain("a");
    await view.flushFrame();

    expect(terminal.writes.at(-1)).toBe("a");
  });

  it("drops a queued old-generation delta before an authoritative reset", async () => {
    const view = await mountViewport({ visible: true, initialSnapshot: "old\n" });
    const terminal = view.fakeTerminal!;

    view.emitOutput("stale");
    await view.emitRestart("fresh\n");
    await view.flushFrame();

    const resetIndex = terminal.writes.lastIndexOf("\u001bc");
    expect(resetIndex).toBeGreaterThanOrEqual(0);
    expect(terminal.writes.slice(resetIndex)).toEqual(["\u001bc", "fresh\n"]);
    expect(terminal.writes).not.toContain("stale");
  });

  it("reopening reconstructs the transcript and resumes live output with no gap or duplicate (C1, invariant 3)", async () => {
    const initialSnapshot = "\u001b[36mprompt λ$ \u001b[0m";
    const beforeHideDelta = "printf 'héllo 😀'\r\nhéllo 😀\r\n";
    let serverHistory = initialSnapshot;
    const initialRuntime = createTerminalTranscriptRuntime();
    initialRuntime.ingest(snapshotEvent(initialSnapshot));
    const view = await mountViewport({ visible: true, publishRuntime: false });

    await view.setTranscriptRuntime(initialRuntime);
    const firstRenderer = view.fakeTerminal!;
    serverHistory += beforeHideDelta;
    initialRuntime.ingest(outputEvent(beforeHideDelta));
    await view.flushFrame();
    expect(firstRenderer.displayedText).toBe(serverHistory);

    await view.setVisible(false);
    await view.setTranscriptRuntime(null);
    const disposedWriteCount = firstRenderer.writes.length;
    const hiddenDelta = "\u001b[33mfile-α\r\nfile-β\u001b[0m\r\n";
    serverHistory += hiddenDelta;

    // The hidden client has no runtime producer. Server history advances independently.
    expect(firstRenderer.writes).toHaveLength(disposedWriteCount);
    expect(firstRenderer.writesAfterDispose).toEqual([]);
    expect(view.closeSessionSpy).not.toHaveBeenCalled();

    await view.setVisible(true);
    expect(view.fakeTerminal?.displayedText).toBe("");

    const replacementRuntime = createTerminalTranscriptRuntime();
    replacementRuntime.ingest(snapshotEvent(serverHistory));
    const beforeRendererRegistration = "boundary-雪\r\n";
    serverHistory += beforeRendererRegistration;
    replacementRuntime.ingest(outputEvent(beforeRendererRegistration));

    await view.setTranscriptRuntime(replacementRuntime);
    const replacementRenderer = view.fakeTerminal!;
    const showSnapshot = serverHistory;

    expect(replacementRenderer.displayedText).toBe(showSnapshot);
    expect(replacementRenderer.resetCount).toBe(1);
    expect(replacementRenderer.writes.filter((value) => value === showSnapshot)).toHaveLength(1);

    const postShowDelta = "done 🚀\u001b[2K\r\n";
    serverHistory += postShowDelta;
    replacementRuntime.ingest(outputEvent(`done ${postShowDelta[5]}`));
    replacementRuntime.ingest(outputEvent(postShowDelta.slice(6)));
    await view.flushFrame();

    expect(replacementRenderer.displayedText).toBe(serverHistory);
    expect(replacementRenderer.writes.filter((value) => value === postShowDelta)).toHaveLength(1);
    expect(new TextEncoder().encode(replacementRenderer.displayedText)).toEqual(
      new TextEncoder().encode(serverHistory),
    );
    expect(firstRenderer.writes).toHaveLength(disposedWriteCount);
    expect(firstRenderer.writesAfterDispose).toEqual([]);
    expect(view.closeSessionSpy).not.toHaveBeenCalled();
  });

  it("releases renderer resources while the document is backgrounded and restores them once", async () => {
    const view = await mountViewport({ visible: true, initialSnapshot: "ready" });
    const first = view.fakeTerminal!;

    await view.setDocumentVisible(false);
    expect(first.dispose).toHaveBeenCalledOnce();
    expect(view.fakeTerminal).toBeNull();
    expect(testState.attachedSessionInputs.at(-1)).toMatchObject({ attach: false });

    await view.setDocumentVisible(true);
    expect(view.fakeTerminal).not.toBeNull();
    expect(xtermState.terminals).toHaveLength(2);
    expect(testState.attachedSessionInputs.at(-1)).toMatchObject({ attach: true });
  });

  it("does not accumulate renderer listeners, observers, or disposals across rapid hide/show", async () => {
    const addWindowListener = vi.spyOn(window, "addEventListener");
    const removeWindowListener = vi.spyOn(window, "removeEventListener");
    const addDocumentListener = vi.spyOn(document, "addEventListener");
    const removeDocumentListener = vi.spyOn(document, "removeEventListener");
    const view = await mountViewport({ visible: true });
    const first = view.fakeTerminal!;

    await view.setVisible(false);
    await view.setVisible(true);
    const second = view.fakeTerminal!;
    await view.setVisible(false);
    await view.setVisible(false);

    expect(first.dispose).toHaveBeenCalledOnce();
    expect(second.dispose).toHaveBeenCalledOnce();
    expect(view.detachRendererSpy).toHaveBeenCalledTimes(2);
    const focusAdds = addWindowListener.mock.calls.filter(([type]) => type === "focus");
    const focusRemoves = removeWindowListener.mock.calls.filter(([type]) => type === "focus");
    expect(focusRemoves).toHaveLength(focusAdds.length);
    const visibilityAdds = addDocumentListener.mock.calls.filter(
      ([type]) => type === "visibilitychange",
    );
    const visibilityRemoves = removeDocumentListener.mock.calls.filter(
      ([type]) => type === "visibilitychange",
    );
    expect(visibilityRemoves).toHaveLength(visibilityAdds.length);
    expect(observerInstances).toHaveLength(2);
    for (const observer of observerInstances) expect(observer.disconnect).toHaveBeenCalledOnce();
  });

  it("opens xterm in the rendered mount and disposes effects and listeners on unmount", async () => {
    const removeWindowListener = vi.spyOn(window, "removeEventListener");
    const mounted = await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const observer = observerInstances[0]!;

    expect(terminal.open).toHaveBeenCalledWith(
      mounted.container.querySelector('[data-terminal-xterm-mount="term-1"]'),
    );
    expect(xtermState.fitAddons[0]?.fit).toHaveBeenCalled();
    expect(observer.observe).toHaveBeenCalledWith(
      document.documentElement,
      expect.objectContaining({ attributes: true }),
    );

    await unmount(mounted);

    expect(terminal.inputDisposable.dispose).toHaveBeenCalledOnce();
    expect(terminal.selectionDisposable.dispose).toHaveBeenCalledOnce();
    expect(terminal.linkDisposable.dispose).toHaveBeenCalledOnce();
    expect(terminal.dispose).toHaveBeenCalledOnce();
    expect(observer.disconnect).toHaveBeenCalledOnce();
    expect(removeWindowListener).toHaveBeenCalledWith("focus", expect.any(Function));
    expect(animationFrames.size).toBe(0);
  });

  it("routes xterm data and renders live output without a React output rerender", async () => {
    const props = viewportProps({ autoFocus: true });
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;
    const runtime = testState.session.transcriptRuntime!;

    await act(async () => terminal.dataHandler?.("pwd\r"));
    expect(testState.writeCommand).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-1", data: "pwd\r" },
    });

    runtime.ingest(outputEvent("ready"));
    await flushAnimationFrames();
    expect(terminal.writes).toContain("ready");

    await act(async () => mounted.root.render(<TerminalViewport {...props} focusRequestId={1} />));

    await flushAnimationFrames();
    expect(terminal.focus).toHaveBeenCalled();
  });

  it("coalesces rapid xterm data into one write", async () => {
    await mount(<TerminalViewport {...viewportProps({ terminalId: "term-coalesced" })} />);
    const terminal = xtermState.terminals[0]!;

    terminal.dataHandler?.("a");
    terminal.dataHandler?.("b");
    terminal.dataHandler?.("c");
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(testState.writeCommand).toHaveBeenCalledOnce();
    expect(testState.writeCommand).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-coalesced", data: "abc" },
    });
  });

  it("prepares legacy input again after a fresh attachment resets its binding", async () => {
    const props = viewportProps({ terminalId: "term-gated" });
    const mounted = await mount(<TerminalViewport {...props} />);
    testState.prepareInputCommand.mockClear();
    testState.resetInputCommand.mockClear();
    testState.session = { ...testState.session, generation: testState.session.generation + 1 };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    expect(testState.resetInputCommand).toHaveBeenCalledOnce();
    expect(testState.prepareInputCommand).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-gated" },
    });
    expect(testState.resetInputCommand.mock.invocationCallOrder[0]).toBeLessThan(
      testState.prepareInputCommand.mock.invocationCallOrder[0]!,
    );
  });

  it("pipelines interactive and programmatic input on an ordered server before acknowledgement", async () => {
    testState.serverConfig!.environment.capabilities = { terminalOrderedInput: true };
    let completeFirst!: (result: unknown) => void;
    testState.writeCommand.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          completeFirst = resolve;
        }),
    );
    await mount(<TerminalViewport {...viewportProps({ terminalId: "term-gated" })} />);
    const terminal = xtermState.terminals[0]!;
    terminal.dataHandler?.("a");
    await act(async () => Promise.resolve());
    enqueueTerminalInput({
      environmentId: ENVIRONMENT_ID,
      threadId: THREAD_ID,
      terminalId: "term-gated",
      data: "b",
      fallbackError: "input failed",
      write: (data) =>
        testState.writeCommand({
          environmentId: ENVIRONMENT_ID,
          input: { threadId: THREAD_ID, terminalId: "term-gated", data },
        }),
    });
    await act(async () => Promise.resolve());
    expect(testState.writeCommand.mock.calls.map(([request]) => request.input.data)).toEqual([
      "a",
      "b",
    ]);
    expect(testState.prepareInputCommand).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-gated" },
    });
    completeFirst(AsyncResult.success(undefined));
    await act(async () => Promise.resolve());
  });

  it("pauses failed ordered input until a fresh attachment and offers reconnect without restarting the process", async () => {
    testState.serverConfig!.environment.capabilities = { terminalOrderedInput: true };
    testState.writeCommand.mockResolvedValueOnce(
      AsyncResult.failure(Cause.fail(new Error("Input was not delivered."))),
    );
    const props = viewportProps({ terminalId: "term-gated" });
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;
    await act(async () => terminal.dataHandler?.("failed"));
    await act(async () => terminal.dataHandler?.("must not send"));
    expect(testState.writeCommand).toHaveBeenCalledTimes(1);
    const reconnect = Array.from(mounted.container.querySelectorAll("button")).find((button) =>
      button.textContent?.includes("Reconnect input"),
    );
    expect(reconnect).toBeDefined();
    await act(async () => reconnect!.click());
    expect(testState.reattachCommand).toHaveBeenCalledOnce();
    expect(testState.restartCommand).not.toHaveBeenCalled();
    testState.session = { ...testState.session, generation: testState.session.generation + 1 };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    await act(async () => terminal.dataHandler?.("fresh"));
    expect(testState.writeCommand.mock.calls.map(([request]) => request.input.data)).toEqual([
      "failed",
      "fresh",
    ]);
  });

  it("keeps terminal writes single-flight and drops dependent input after failure", async () => {
    let resolveFirstWrite!: (result: unknown) => void;
    let activeWrites = 0;
    let peakActiveWrites = 0;
    testState.writeCommand.mockImplementation(
      () =>
        new Promise((resolve) => {
          activeWrites += 1;
          peakActiveWrites = Math.max(peakActiveWrites, activeWrites);
          resolveFirstWrite = (result) => {
            activeWrites -= 1;
            resolve(result);
          };
        }),
    );
    await mount(<TerminalViewport {...viewportProps({ terminalId: "term-gated" })} />);
    const terminal = xtermState.terminals[0]!;

    terminal.dataHandler?.("a");
    await act(async () => Promise.resolve());
    terminal.dataHandler?.("b");
    terminal.dataHandler?.("c");
    await act(async () => Promise.resolve());

    expect(testState.writeCommand).toHaveBeenCalledOnce();
    expect(peakActiveWrites).toBe(1);

    resolveFirstWrite(AsyncResult.failure(Cause.fail(new Error("first write failed"))));
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(testState.writeCommand).toHaveBeenCalledOnce();
    expect(terminal.writes).toContain("\r\n[terminal] first write failed\r\n");
  });

  it("treats an interrupted write as uncertain and drops dependent input quietly", async () => {
    let resolveFirstWrite!: (result: unknown) => void;
    testState.writeCommand.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveFirstWrite = resolve;
        }),
    );
    await mount(<TerminalViewport {...viewportProps({ terminalId: "term-interrupted" })} />);
    const terminal = xtermState.terminals[0]!;

    terminal.dataHandler?.("a");
    await act(async () => Promise.resolve());
    terminal.dataHandler?.("b");
    terminal.dataHandler?.("c");

    resolveFirstWrite(AsyncResult.failure(Cause.interrupt(1)));
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(testState.writeCommand).toHaveBeenCalledOnce();
    expect(terminal.writes.some((value) => value.includes("[terminal]"))).toBe(false);
  });

  it("serializes synthesized input behind an in-flight interactive write", async () => {
    let resolveFirstWrite!: (result: unknown) => void;
    testState.writeCommand
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveFirstWrite = resolve;
          }),
      )
      .mockResolvedValueOnce(AsyncResult.success(undefined));
    await mount(<TerminalViewport {...viewportProps({ terminalId: "term-programmatic" })} />);
    const terminal = xtermState.terminals[0]!;

    terminal.dataHandler?.("interactive");
    await act(async () => Promise.resolve());
    enqueueTerminalInput({
      environmentId: ENVIRONMENT_ID,
      threadId: THREAD_ID,
      terminalId: "term-programmatic",
      data: "exit\n",
      fallbackError: "Terminal exit fallback failed",
      write: (data) =>
        testState.writeCommand({
          environmentId: ENVIRONMENT_ID,
          input: { threadId: THREAD_ID, terminalId: "term-programmatic", data },
        }),
    });
    await act(async () => Promise.resolve());

    expect(testState.writeCommand).toHaveBeenCalledOnce();

    resolveFirstWrite(AsyncResult.success(undefined));
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(testState.writeCommand).toHaveBeenCalledTimes(2);
    expect(testState.writeCommand).toHaveBeenNthCalledWith(2, {
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-programmatic", data: "exit\n" },
    });
  });

  it("resets pending input only when the attached runtime generation increments", async () => {
    let resolveFirstWrite!: (result: unknown) => void;
    testState.writeCommand.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveFirstWrite = resolve;
        }),
    );
    const props = viewportProps();
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;
    const runtime = testState.session.transcriptRuntime!;

    terminal.dataHandler?.("a");
    await act(async () => Promise.resolve());
    terminal.dataHandler?.("b");
    await act(async () => Promise.resolve());
    expect(testState.writeCommand).toHaveBeenCalledOnce();

    runtime.ingest(restartedEvent("fresh"));
    testState.session = {
      ...testState.session,
      generation: runtime.metadata().generation,
    };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    resolveFirstWrite(AsyncResult.success(undefined));
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(testState.writeCommand).toHaveBeenCalledOnce();
  });

  it("does not reset pending input for a cleared transcript", async () => {
    let resolveFirstWrite!: (result: unknown) => void;
    testState.writeCommand
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveFirstWrite = resolve;
          }),
      )
      .mockResolvedValueOnce(AsyncResult.success(undefined));
    const props = viewportProps();
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;
    const runtime = testState.session.transcriptRuntime!;

    terminal.dataHandler?.("a");
    await act(async () => Promise.resolve());
    terminal.dataHandler?.("b");
    await act(async () => Promise.resolve());
    runtime.ingest(clearedEvent());
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    resolveFirstWrite(AsyncResult.success(undefined));
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(testState.writeCommand).toHaveBeenCalledTimes(2);
    expect(testState.writeCommand).toHaveBeenNthCalledWith(2, {
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-1", data: "b" },
    });
  });

  it("creates a fresh scheduler after the terminal input key is released", async () => {
    const first = await mount(
      <TerminalViewport {...viewportProps({ terminalId: "term-release" })} />,
    );
    await unmount(first);
    releaseTerminalUiResources(ENVIRONMENT_ID, THREAD_ID, "term-release");

    testState.writeCommand.mockResolvedValueOnce(
      AsyncResult.failure(Cause.fail(new Error("new scheduler failure"))),
    );
    await mount(<TerminalViewport {...viewportProps({ terminalId: "term-release" })} />);
    const terminal = xtermState.terminals[1]!;
    terminal.dataHandler?.("x");
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(terminal.writes).toContain("\r\n[terminal] new scheduler failure\r\n");
  });

  it("isolates a replacement binding from an in-flight result released with the old key", async () => {
    let resolveStaleWrite!: (result: unknown) => void;
    testState.writeCommand
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveStaleWrite = resolve;
          }),
      )
      .mockResolvedValueOnce(AsyncResult.failure(Cause.fail("no details")));

    const first = await mount(
      <TerminalViewport {...viewportProps({ terminalId: "term-release-stale" })} />,
    );
    const oldTerminal = xtermState.terminals[0]!;
    oldTerminal.dataHandler?.("old");
    await act(async () => Promise.resolve());
    await unmount(first);
    releaseTerminalUiResources(ENVIRONMENT_ID, THREAD_ID, "term-release-stale");

    await mount(<TerminalViewport {...viewportProps({ terminalId: "term-release-stale" })} />);
    const newTerminal = xtermState.terminals[1]!;
    const clearEvent = new KeyboardEvent("keydown", {
      key: "l",
      ctrlKey: true,
      cancelable: true,
    });
    expect(newTerminal.keyHandler?.(clearEvent)).toBe(false);
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(newTerminal.writes).toContain("\r\n[terminal] Failed to clear terminal\r\n");

    resolveStaleWrite(AsyncResult.failure(Cause.fail(new Error("stale write failed"))));
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(oldTerminal.writes).not.toContain("\r\n[terminal] stale write failed\r\n");
    expect(newTerminal.writes).not.toContain("\r\n[terminal] stale write failed\r\n");
    expect(
      newTerminal.writes.filter((value) => value === "\r\n[terminal] Failed to clear terminal\r\n"),
    ).toHaveLength(1);
  });

  it("retargets the retained scheduler across an unmount and keeps fallback metadata aligned", async () => {
    let resolveFirstWrite!: (result: unknown) => void;
    let activeWrites = 0;
    let peakActiveWrites = 0;
    const trackWrite = <T,>(write: () => Promise<T>): Promise<T> => {
      activeWrites += 1;
      peakActiveWrites = Math.max(peakActiveWrites, activeWrites);
      return write().finally(() => {
        activeWrites -= 1;
      });
    };
    testState.writeCommand
      .mockImplementationOnce(() =>
        trackWrite(
          () =>
            new Promise((resolve) => {
              resolveFirstWrite = resolve;
            }),
        ),
      )
      .mockImplementationOnce(() =>
        trackWrite(async () =>
          AsyncResult.failure(Cause.fail(new Error("remounted write failed"))),
        ),
      )
      .mockImplementationOnce(() =>
        trackWrite(async () => AsyncResult.failure(Cause.fail("no details"))),
      )
      .mockImplementationOnce(() =>
        trackWrite(async () => AsyncResult.failure(Cause.fail("no details"))),
      );

    const first = await mount(
      <TerminalViewport {...viewportProps({ terminalId: "term-remount" })} />,
    );
    const oldTerminal = xtermState.terminals[0]!;
    oldTerminal.dataHandler?.("a");
    await act(async () => Promise.resolve());
    expect(testState.writeCommand).toHaveBeenCalledOnce();

    await unmount(first);
    await mount(<TerminalViewport {...viewportProps({ terminalId: "term-remount" })} />);
    const newTerminal = xtermState.terminals[1]!;
    newTerminal.dataHandler?.("b");
    newTerminal.dataHandler?.("c");
    await act(async () => Promise.resolve());
    expect(testState.writeCommand).toHaveBeenCalledOnce();

    resolveFirstWrite(AsyncResult.success(undefined));
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(peakActiveWrites).toBe(1);
    expect(testState.writeCommand).toHaveBeenNthCalledWith(2, {
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-remount", data: "bc" },
    });
    expect(oldTerminal.writes).not.toContain("\r\n[terminal] remounted write failed\r\n");
    expect(
      newTerminal.writes.filter((value) => value === "\r\n[terminal] remounted write failed\r\n"),
    ).toHaveLength(1);

    const clearEvent = new KeyboardEvent("keydown", {
      key: "l",
      ctrlKey: true,
      cancelable: true,
    });
    expect(newTerminal.keyHandler?.(clearEvent)).toBe(false);
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(
      newTerminal.writes.filter((value) => value === "\r\n[terminal] Failed to clear terminal\r\n"),
    ).toHaveLength(1);

    newTerminal.dataHandler?.("later");
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(
      newTerminal.writes.filter((value) => value === "\r\n[terminal] Terminal write failed\r\n"),
    ).toHaveLength(1);
  });

  it("keeps queued input draining while hidden and reports later failures to the current renderer", async () => {
    let resolveFirstWrite!: (result: unknown) => void;
    testState.writeCommand
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveFirstWrite = resolve;
          }),
      )
      .mockResolvedValueOnce(AsyncResult.success(undefined))
      .mockResolvedValueOnce(
        AsyncResult.failure(Cause.fail(new Error("current renderer failure"))),
      );
    const view = await mountViewport({ visible: true });
    const firstTerminal = view.fakeTerminal!;

    firstTerminal.dataHandler?.("a");
    await act(async () => Promise.resolve());
    firstTerminal.dataHandler?.("b");
    await act(async () => Promise.resolve());
    expect(testState.writeCommand).toHaveBeenCalledOnce();

    await view.setVisible(false);
    resolveFirstWrite(AsyncResult.success(undefined));
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(testState.writeCommand).toHaveBeenCalledTimes(2);
    expect(testState.writeCommand).toHaveBeenNthCalledWith(2, {
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-1", data: "b" },
    });
    expect(firstTerminal.writes).not.toContain("\r\n[terminal] Terminal write failed\r\n");

    await view.setVisible(true);
    const currentTerminal = view.fakeTerminal!;
    currentTerminal.dataHandler?.("c");
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(currentTerminal.writes).toContain("\r\n[terminal] current renderer failure\r\n");
    expect(firstTerminal.writes).not.toContain("\r\n[terminal] current renderer failure\r\n");
  });

  it("applies authoritative resets and reports metadata errors and closure once", async () => {
    const onSessionExited = vi.fn();
    const props = viewportProps({ onSessionExited });
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;
    const runtime = testState.session.transcriptRuntime!;

    runtime.ingest(outputEvent("abcdef"));
    await flushAnimationFrames();
    runtime.ingest(restartedEvent("xy"));
    testState.session = {
      ...testState.session,
      error: "stream failed",
      status: "closed",
      generation: runtime.metadata().generation,
    };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    expect(terminal.writes).toContain("\u001bc");
    expect(terminal.writes).toContain("xy");
    expect(terminal.writes).toContain("\r\n[terminal] stream failed\r\n");
    expect(terminal.writes).toContain("\r\n[terminal] Terminal closed\r\n");
    expect(onSessionExited).toHaveBeenCalledOnce();

    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    expect(onSessionExited).toHaveBeenCalledOnce();
    expect(
      terminal.writes.filter((value) => value === "\r\n[terminal] stream failed\r\n"),
    ).toHaveLength(1);
  });

  it("reports an error that is already present on the first authoritative publication once", async () => {
    const props = viewportProps();
    testState.session = {
      ...testState.session,
      error: "initial stream failure",
      status: "error",
    };
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;

    expect(terminal.writes).toContain("\r\n[terminal] initial stream failure\r\n");
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    expect(
      terminal.writes.filter((value) => value === "\r\n[terminal] initial stream failure\r\n"),
    ).toHaveLength(1);
  });

  it("handles an already-exited first authoritative publication once", async () => {
    const onSessionExited = vi.fn();
    const props = viewportProps({ onSessionExited });
    testState.session = { ...testState.session, status: "exited" };
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;

    expect(terminal.writes).toContain("\r\n[terminal] Process exited\r\n");
    expect(onSessionExited).toHaveBeenCalledOnce();
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    expect(onSessionExited).toHaveBeenCalledOnce();
  });

  it("reports write and keyboard-command failures while allowing ordinary keys", async () => {
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    testState.writeCommand.mockResolvedValueOnce(
      AsyncResult.failure(Cause.fail(new Error("write denied"))),
    );
    await act(async () => terminal.dataHandler?.("blocked"));
    await act(async () => Promise.resolve());
    expect(terminal.writes).toContain("\r\n[terminal] write denied\r\n");

    testState.writeCommand.mockResolvedValueOnce(AsyncResult.failure(Cause.fail("no details")));
    const clearEvent = new KeyboardEvent("keydown", { key: "l", ctrlKey: true, cancelable: true });
    expect(terminal.keyHandler?.(clearEvent)).toBe(false);
    await act(async () => Promise.resolve());
    expect(testState.writeCommand).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-1", data: "\u000c" },
    });
    expect(terminal.writes).toContain("\r\n[terminal] Failed to clear terminal\r\n");
    expect(terminal.keyHandler?.(new KeyboardEvent("keydown", { key: "a" }))).toBe(true);
  });

  it("routes macOS navigation and delete shortcuts to terminal input", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel" });
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;

    const moveWord = new KeyboardEvent("keydown", {
      key: "ArrowLeft",
      altKey: true,
      cancelable: true,
    });
    const deleteLine = new KeyboardEvent("keydown", {
      key: "Backspace",
      metaKey: true,
      cancelable: true,
    });
    expect(terminal.keyHandler?.(moveWord)).toBe(false);
    expect(terminal.keyHandler?.(deleteLine)).toBe(false);
    await act(async () => Promise.resolve());

    expect(moveWord.defaultPrevented).toBe(true);
    expect(deleteLine.defaultPrevented).toBe(true);
    expect(testState.writeCommand).toHaveBeenCalledOnce();
    expect(testState.writeCommand).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-1", data: "\u001bb\u0015" },
    });
  });

  it("sends ESC CR for Shift+Enter instead of a bare carriage return", async () => {
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;

    const shiftEnter = new KeyboardEvent("keydown", {
      key: "Enter",
      shiftKey: true,
      cancelable: true,
    });
    expect(terminal.keyHandler?.(shiftEnter)).toBe(false);
    await act(async () => Promise.resolve());

    // preventDefault on keydown suppresses xterm's keypress fallback, which would otherwise still
    // emit "\r" (CoreBrowserTerminal._keyPress).
    expect(shiftEnter.defaultPrevented).toBe(true);
    expect(testState.writeCommand).toHaveBeenCalledOnce();
    expect(testState.writeCommand).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-1", data: "\u001b\r" },
    });

    // Plain Enter still belongs to xterm.
    expect(terminal.keyHandler?.(new KeyboardEvent("keydown", { key: "Enter" }))).toBe(true);
  });

  it.each([
    ["Linux", "c", { ctrlKey: true }],
    ["Linux", "v", { ctrlKey: true }],
    ["Linux", "V", { ctrlKey: true, shiftKey: true }],
    ["Win32", "c", { ctrlKey: true }],
    ["Win32", "v", { ctrlKey: true }],
    ["Win32", "V", { ctrlKey: true, shiftKey: true }],
    ["MacIntel", "c", { metaKey: true }],
    ["MacIntel", "v", { metaKey: true }],
    ["MacIntel", "V", { metaKey: true, shiftKey: true }],
  ] as const)(
    "leaves native clipboard events available for %s %s %j",
    async (platform, key, modifiers) => {
      vi.stubGlobal("navigator", { platform });
      await mount(<TerminalViewport {...viewportProps()} />);
      const terminal = xtermState.terminals[0]!;
      terminal.hasActiveSelection = true;
      terminal.selectionText = "selected terminal output";
      const event = new KeyboardEvent("keydown", { key, ...modifiers, cancelable: true });

      expect(terminal.keyHandler?.(event)).toBe(false);
      expect(event.defaultPrevented).toBe(false);
      expect(testState.writeCommand).not.toHaveBeenCalled();
    },
  );

  it("copies with Ctrl+Shift+C through the native clipboard while preserving pending text", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.hasActiveSelection = true;
    terminal.selectionText = "selected terminal output";
    terminal.textarea.value = "pending input";
    terminal.textarea.setSelectionRange(3, 5);
    let copiedText: string | undefined;
    const copy = vi.fn((command: string) => {
      expect(command).toBe("copy");
      copiedText = terminal.textarea.value.slice(
        terminal.textarea.selectionStart,
        terminal.textarea.selectionEnd,
      );
      return true;
    });
    Object.defineProperty(document, "execCommand", { configurable: true, value: copy });
    const event = new KeyboardEvent("keydown", {
      key: "C",
      ctrlKey: true,
      shiftKey: true,
      cancelable: true,
    });

    expect(terminal.keyHandler?.(event)).toBe(false);
    expect(event.defaultPrevented).toBe(true);
    expect(copiedText).toBe("selected terminal output");
    expect(terminal.textarea.value).toBe("pending input");
    expect(terminal.textarea.selectionStart).toBe(3);
    expect(terminal.textarea.selectionEnd).toBe(5);
    expect(testState.writeCommand).not.toHaveBeenCalled();
  });

  it.each([
    ["Linux", "none", {}, { shiftKey: false, altKey: false }],
    ["Linux", "drag", {}, { shiftKey: true, altKey: false }],
    ["Linux", "any", { shiftKey: true }, { shiftKey: false, altKey: false }],
    ["MacIntel", "drag", {}, { shiftKey: false, altKey: true }],
    ["MacIntel", "drag", { altKey: true }, { shiftKey: false, altKey: false }],
  ] as const)(
    "on %s with %s mouse tracking, a primary drag %j selects unless the modifier sends it to the app",
    async (platform, mouseTrackingMode, modifiers, seen) => {
      vi.stubGlobal("navigator", { platform });
      const mounted = await mount(<TerminalViewport {...viewportProps()} />);
      const terminal = xtermState.terminals[0]!;
      terminal.modes.mouseTrackingMode = mouseTrackingMode;
      const surface = mounted.container.querySelector("[data-terminal-xterm-mount]");
      expect(surface).toBeInstanceOf(HTMLElement);
      const target = document.createElement("div");
      surface!.append(target);
      let observed: { shiftKey: boolean; altKey: boolean } | null = null;
      // xterm's own listeners sit below the mount and read the force-selection modifier.
      target.addEventListener("mousedown", (event) => {
        observed = { shiftKey: event.shiftKey, altKey: event.altKey };
      });
      target.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 0, ...modifiers }));

      expect(observed).toEqual(seen);
      if (platform === "MacIntel") {
        expect(terminal.options).toMatchObject({ macOptionClickForcesSelection: true });
      }
    },
  );

  it("keeps the inverted modifier for the drag and release of the same gesture", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    const mounted = await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.modes.mouseTrackingMode = "drag";
    const surface = mounted.container.querySelector("[data-terminal-xterm-mount]")!;
    const target = document.createElement("div");
    surface.append(target);
    const seen: Array<[string, boolean]> = [];
    const record = (event: MouseEvent) => seen.push([event.type, event.shiftKey]);
    target.addEventListener("mousedown", record);
    document.addEventListener("mousemove", record);
    document.addEventListener("mouseup", record);
    try {
      target.dispatchEvent(
        new MouseEvent("mousedown", { bubbles: true, button: 0, shiftKey: true }),
      );
      target.dispatchEvent(new MouseEvent("mousemove", { bubbles: true, shiftKey: true }));
      target.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, button: 0, shiftKey: true }));
      target.dispatchEvent(new MouseEvent("mousemove", { bubbles: true, shiftKey: true }));
    } finally {
      document.removeEventListener("mousemove", record);
      document.removeEventListener("mouseup", record);
    }

    expect(seen).toEqual([
      ["mousedown", false],
      ["mousemove", false],
      ["mouseup", false],
      ["mousemove", true],
    ]);
  });

  it("does not carry a synthesized Option past the mousedown of a macOS selection", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel" });
    const mounted = await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.modes.mouseTrackingMode = "drag";
    const surface = mounted.container.querySelector("[data-terminal-xterm-mount]")!;
    const target = document.createElement("div");
    surface.append(target);
    const seen: Array<[string, boolean]> = [];
    const record = (event: MouseEvent) => seen.push([event.type, event.altKey]);
    target.addEventListener("mousedown", record);
    document.addEventListener("mouseup", record);
    try {
      target.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 0 }));
      target.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, button: 0 }));
    } finally {
      document.removeEventListener("mouseup", record);
    }

    expect(seen).toEqual([
      ["mousedown", true],
      ["mouseup", false],
    ]);
  });

  // xterm clears its selection on every mouse report it sends the app, so a
  // selection must not leak hover (any-motion tracking) to the report listener
  // on `.xterm`, while link detection on `.xterm-screen` still sees it.
  it.each([
    ["any", true, 0, false],
    ["any", true, 1, true],
    ["any", false, 0, true],
    ["drag", true, 0, true],
  ] as const)(
    "with %s mouse tracking and selection=%s, a move with buttons=%s is reported: %s",
    async (mouseTrackingMode, selected, buttons, reported) => {
      vi.stubGlobal("navigator", { platform: "Linux" });
      const mounted = await mount(<TerminalViewport {...viewportProps()} />);
      const terminal = xtermState.terminals[0]!;
      terminal.modes.mouseTrackingMode = mouseTrackingMode;
      terminal.hasActiveSelection = selected;
      const screen = mounted.container.querySelector(".xterm-screen")!;
      const target = document.createElement("div");
      screen.append(target);
      const linkDetection = vi.fn();
      const reports = vi.fn();
      screen.addEventListener("mousemove", linkDetection);
      screen.parentElement!.addEventListener("mousemove", reports);

      target.dispatchEvent(new MouseEvent("mousemove", { bubbles: true, buttons }));

      expect(linkDetection).toHaveBeenCalledOnce();
      expect(reports).toHaveBeenCalledTimes(reported ? 1 : 0);
    },
  );

  it.each([
    ["Linux", true, {}, { shiftKey: true, altKey: false }],
    ["Linux", true, { shiftKey: true }, { shiftKey: true, altKey: false }],
    ["Linux", false, {}, { shiftKey: false, altKey: false }],
    ["MacIntel", true, {}, { shiftKey: false, altKey: true }],
    ["MacIntel", true, { altKey: true }, { shiftKey: false, altKey: true }],
  ] as const)(
    "on %s a right-click with selection=%s and %j is kept from the app: %j",
    async (platform, selected, modifiers, seen) => {
      vi.stubGlobal("navigator", { platform });
      const mounted = await mount(<TerminalViewport {...viewportProps()} />);
      const terminal = xtermState.terminals[0]!;
      terminal.modes.mouseTrackingMode = "vt200";
      terminal.hasActiveSelection = selected;
      const target = document.createElement("div");
      mounted.container.querySelector("[data-terminal-xterm-mount]")!.append(target);
      let observed: { shiftKey: boolean; altKey: boolean } | null = null;
      target.addEventListener("mousedown", (event) => {
        observed = { shiftKey: event.shiftKey, altKey: event.altKey };
      });

      target.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 2, ...modifiers }));

      expect(observed).toEqual(seen);
    },
  );

  it("keeps Ctrl+C as an interrupt without a selection and consumes explicit copy", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const interrupt = new KeyboardEvent("keydown", { key: "c", ctrlKey: true });
    const copy = new KeyboardEvent("keydown", {
      key: "C",
      ctrlKey: true,
      shiftKey: true,
      cancelable: true,
    });
    expect(terminal.keyHandler?.(interrupt)).toBe(true);
    expect(terminal.keyHandler?.(copy)).toBe(false);
    expect(copy.defaultPrevented).toBe(true);
  });

  it.each([false, true])(
    "preserves input and reports native copy failure (throws: %s)",
    async (throws) => {
      vi.stubGlobal("navigator", { platform: "Linux" });
      await mount(<TerminalViewport {...viewportProps()} />);
      const terminal = xtermState.terminals[0]!;
      terminal.hasActiveSelection = true;
      terminal.selectionText = "selected terminal output";
      terminal.textarea.value = "pending input";
      Object.defineProperty(document, "execCommand", {
        configurable: true,
        value: () => {
          if (throws) throw new Error("clipboard denied");
          return false;
        },
      });

      terminal.keyHandler?.(
        new KeyboardEvent("keydown", { key: "C", ctrlKey: true, shiftKey: true }),
      );

      expect(terminal.textarea.value).toBe("pending input");
      expect(terminal.writes).toContain("\r\n[terminal] Copy failed. Try Ctrl+C.\r\n");
      expect(testState.writeCommand).not.toHaveBeenCalled();
    },
  );

  it("keeps selection focused and offers Add to chat only on explicit context menu", async () => {
    testState.localApiAvailable = true;
    testState.contextMenuShow.mockResolvedValue("add-to-chat");
    const onAddTerminalContext = vi.fn();
    const mounted = await mount(
      <TerminalViewport {...viewportProps({ onAddTerminalContext, terminalLabel: "Build" })} />,
    );
    const terminal = xtermState.terminals[0]!;
    terminal.hasActiveSelection = true;
    terminal.selectionText = "\r\nfirst\r\nsecond\r\n";
    terminal.selectionPosition = { start: { y: 4 } };
    const mountElement = mounted.container.querySelector<HTMLElement>(
      '[data-terminal-xterm-mount="term-1"]',
    )!;

    await act(async () => {
      mountElement.dispatchEvent(
        new PointerEvent("pointerdown", { bubbles: true, button: 0, pointerId: 1 }),
      );
      window.dispatchEvent(
        new MouseEvent("mouseup", {
          bubbles: true,
          button: 0,
          detail: 1,
          clientX: 30,
          clientY: 40,
        }),
      );
      await new Promise((resolve) => window.setTimeout(resolve, 2));
      for (const callback of animationFrames.values()) callback(0);
      animationFrames.clear();
      await Promise.resolve();
    });

    expect(testState.contextMenuShow).not.toHaveBeenCalled();
    expect(terminal.clearSelection).toHaveBeenCalledTimes(1);
    await act(async () => {
      mountElement.dispatchEvent(
        new MouseEvent("contextmenu", {
          bubbles: true,
          cancelable: true,
          button: 2,
          clientX: 30,
          clientY: 40,
        }),
      );
    });

    expect(testState.contextMenuShow).toHaveBeenCalledWith(
      [
        { id: "copy", label: "Copy" },
        { id: "add-to-chat", label: "Add to chat" },
      ],
      expect.objectContaining({ x: expect.any(Number), y: expect.any(Number) }),
    );
    expect(onAddTerminalContext).toHaveBeenCalledWith({
      terminalId: "term-1",
      terminalLabel: "Build",
      lineStart: 5,
      lineEnd: 6,
      text: "first\nsecond",
    });
    expect(terminal.clearSelection).toHaveBeenCalled();
    expect(terminal.focus).toHaveBeenCalled();

    terminal.hasActiveSelection = false;
    terminal.selectionHandler?.();
  });

  it.each([
    ["the clipboard API", "resolve", true, null],
    ["execCommand after a rejected clipboard write", "reject", true, null],
    ["a visible failure when both refuse", "reject", false, "Copy failed. Try Cmd+C."],
  ] as const)(
    "copies the selection from the context menu through %s",
    async (_label, clipboardResult, execCommandResult, failure) => {
      testState.localApiAvailable = true;
      testState.contextMenuShow.mockResolvedValue("copy");
      const writeText = vi.fn(() =>
        clipboardResult === "resolve" ? Promise.resolve() : Promise.reject(new Error("denied")),
      );
      vi.stubGlobal("navigator", { platform: "MacIntel", clipboard: { writeText } });
      const execCommand = vi.fn(() => execCommandResult);
      Object.defineProperty(document, "execCommand", { configurable: true, value: execCommand });
      const onAddTerminalContext = vi.fn();
      const mounted = await mount(
        <TerminalViewport {...viewportProps({ onAddTerminalContext })} />,
      );
      const terminal = xtermState.terminals[0]!;
      terminal.hasActiveSelection = true;
      terminal.selectionText = "\nfirst\nsecond\n";
      terminal.selectionPosition = { start: { y: 0 } };
      terminal.textarea.value = "pending";
      const clearsBefore = terminal.clearSelection.mock.calls.length;

      await act(async () => {
        mounted.container
          .querySelector('[data-terminal-xterm-mount="term-1"]')!
          .dispatchEvent(
            new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2 }),
          );
        await new Promise((resolve) => window.setTimeout(resolve, 0));
      });

      expect(writeText).toHaveBeenCalledWith("\nfirst\nsecond\n");
      expect(execCommand).toHaveBeenCalledTimes(clipboardResult === "resolve" ? 0 : 1);
      expect(terminal.textarea.value).toBe("pending");
      expect(onAddTerminalContext).not.toHaveBeenCalled();
      expect(terminal.clearSelection).toHaveBeenCalledTimes(clearsBefore);
      expect(terminal.focus).toHaveBeenCalled();
      if (failure === null) {
        expect(terminal.writes.join("")).not.toContain("Copy failed");
      } else {
        expect(terminal.writes).toContain(`\r\n[terminal] ${failure}\r\n`);
      }
    },
  );

  it("drops a menu copy whose viewport is torn down while the clipboard write is pending", async () => {
    testState.localApiAvailable = true;
    testState.contextMenuShow.mockResolvedValue("copy");
    let rejectWrite!: (error: Error) => void;
    const writeText = vi.fn(
      () =>
        new Promise<void>((_resolve, reject) => {
          rejectWrite = reject;
        }),
    );
    vi.stubGlobal("navigator", { platform: "Linux", clipboard: { writeText } });
    const execCommand = vi.fn(() => true);
    Object.defineProperty(document, "execCommand", { configurable: true, value: execCommand });
    const mounted = await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.hasActiveSelection = true;
    terminal.selectionText = "selected";
    terminal.selectionPosition = { start: { y: 0 } };
    await act(async () => {
      mounted.container
        .querySelector('[data-terminal-xterm-mount="term-1"]')!
        .dispatchEvent(
          new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2 }),
        );
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });
    expect(writeText).toHaveBeenCalledWith("selected");

    await unmount(mounted);
    const focusCalls = terminal.focus.mock.calls.length;
    await act(async () => {
      rejectWrite(new Error("denied"));
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });

    expect(execCommand).not.toHaveBeenCalled();
    expect(terminal.focus).toHaveBeenCalledTimes(focusCalls);
    expect(terminal.writes.join("")).not.toContain("Copy failed");
  });

  describe("image paste", () => {
    const withCapabilities = (capabilities: { terminalImagePaste?: boolean }) => {
      testState.serverConfig = {
        availableEditors: ["vscode"],
        environment: { platform: { os: "windows" }, capabilities },
      };
    };
    const sentData = () =>
      testState.writeCommand.mock.calls
        .map(([request]) => (request as { input: { data: string } }).input.data)
        .join("");
    const pasteEvent = (text: string, files: File[]) => {
      const event = new Event("paste", { bubbles: true, cancelable: true });
      Object.defineProperty(event, "clipboardData", {
        value: { getData: (type: string) => (type === "text/plain" ? text : ""), files },
      });
      return event;
    };
    const png = () =>
      new File([new Uint8Array([137, 80, 78, 71])], "image.png", { type: "image/png" });
    const pasteInto = async (event: Event) => {
      const mounted = await mount(<TerminalViewport {...viewportProps()} />);
      const screen = mounted.container.querySelector(".xterm-screen")!;
      const xtermPaste = vi.fn();
      screen.addEventListener("paste", xtermPaste);
      await act(async () => {
        screen.dispatchEvent(event);
        await new Promise((resolve) => window.setTimeout(resolve, 0));
      });
      return { terminal: xtermState.terminals[0]!, xtermPaste };
    };

    it("uploads an image-only paste and pastes the path of its server copy", async () => {
      withCapabilities({ terminalImagePaste: true });
      const image = png();
      const event = pasteEvent("", [image]);

      const { xtermPaste } = await pasteInto(event);

      expect(event.defaultPrevented).toBe(true);
      expect(xtermPaste).not.toHaveBeenCalled();
      expect(testState.stageImagePasteCommand).toHaveBeenCalledWith(
        {
          environmentId: ENVIRONMENT_ID,
          input: { file: image, name: "image.png", mimeType: "image/png" },
        },
        { signal: expect.any(AbortSignal) },
      );
      expect(sentData()).toBe("/state/terminal-pastes/paste.png");
    });

    it("brackets the path and holds input typed while the image uploads", async () => {
      withCapabilities({ terminalImagePaste: true });
      let finishUpload!: (result: unknown) => void;
      testState.stageImagePasteCommand.mockReturnValue(
        new Promise((resolve) => {
          finishUpload = resolve;
        }),
      );
      const mounted = await mount(<TerminalViewport {...viewportProps()} />);
      const terminal = xtermState.terminals[0]!;
      terminal.modes.bracketedPasteMode = true;
      await act(async () => {
        mounted.container.querySelector(".xterm-screen")!.dispatchEvent(pasteEvent("", [png()]));
        terminal.dataHandler?.(" what is this?\r");
        await new Promise((resolve) => window.setTimeout(resolve, 0));
      });
      expect(sentData()).toBe("");

      await act(async () => {
        finishUpload(AsyncResult.success("/state/terminal-pastes/paste.png"));
        await new Promise((resolve) => window.setTimeout(resolve, 0));
      });

      expect(sentData()).toBe("\x1b[200~/state/terminal-pastes/paste.png\x1b[201~ what is this?\r");
    });

    it("shows the upload and lets Cancel release the input held behind it", async () => {
      withCapabilities({ terminalImagePaste: true });
      testState.stageImagePasteCommand.mockImplementation(
        (_request: unknown, { signal }: { signal: AbortSignal }) =>
          new Promise((resolve) => {
            signal.addEventListener("abort", () => resolve(AsyncResult.failure(Cause.interrupt())));
          }),
      );
      const mounted = await mount(<TerminalViewport {...viewportProps()} />);
      const terminal = xtermState.terminals[0]!;
      await act(async () => {
        mounted.container.querySelector(".xterm-screen")!.dispatchEvent(pasteEvent("", [png()]));
        terminal.dataHandler?.("typed");
        await new Promise((resolve) => window.setTimeout(resolve, 0));
      });
      const status = mounted.container.querySelector('[role="status"]');
      expect(status?.textContent).toContain("Uploading pasted image");
      expect(sentData()).toBe("");

      await click(
        [...status!.querySelectorAll("button")].find((button) => button.textContent === "Cancel")!,
      );
      await act(async () => {
        await new Promise((resolve) => window.setTimeout(resolve, 0));
      });

      expect(mounted.container.querySelector('[role="status"]')).toBeNull();
      expect(sentData()).toBe("typed");
      expect(terminal.writes.join("")).not.toContain("Image paste failed");
    });

    it("does not upload or paste into an unavailable workspace", async () => {
      withCapabilities({ terminalImagePaste: true });
      const mounted = await mount(
        <TerminalViewport
          {...viewportProps()}
          workspaceUnavailable="Workspace folder is missing"
        />,
      );
      const event = pasteEvent("", [png()]);
      await act(async () => {
        mounted.container.querySelector(".xterm-screen")!.dispatchEvent(event);
        await new Promise((resolve) => window.setTimeout(resolve, 0));
      });

      expect(event.defaultPrevented).toBe(true);
      expect(testState.stageImagePasteCommand).not.toHaveBeenCalled();
      expect(sentData()).toBe("");
    });

    it("aborts a pending upload when the viewport is torn down", async () => {
      withCapabilities({ terminalImagePaste: true });
      testState.stageImagePasteCommand.mockReturnValue(new Promise(() => undefined));
      const mounted = await mount(<TerminalViewport {...viewportProps()} />);
      await act(async () => {
        mounted.container.querySelector(".xterm-screen")!.dispatchEvent(pasteEvent("", [png()]));
      });
      const { signal } = testState.stageImagePasteCommand.mock.calls[0]![1] as {
        signal: AbortSignal;
      };
      expect(signal.aborted).toBe(false);

      await unmount(mounted);

      expect(signal.aborted).toBe(true);
    });

    it("leaves text pastes to xterm even when files are attached", async () => {
      withCapabilities({ terminalImagePaste: true });
      const event = pasteEvent("echo hi", [png()]);

      const { terminal, xtermPaste } = await pasteInto(event);

      expect(event.defaultPrevented).toBe(false);
      expect(xtermPaste).toHaveBeenCalledOnce();
      expect(testState.stageImagePasteCommand).not.toHaveBeenCalled();
      expect(sentData()).toBe("");
    });

    it.each([
      [
        "an older server",
        {},
        png(),
        "Image paste needs a newer BiBCode server on this environment.",
      ],
      [
        "an unsupported image type",
        { terminalImagePaste: true },
        new File(["<svg/>"], "drawing.svg", { type: "image/svg+xml" }),
        "Image paste supports PNG, JPEG, GIF and WebP images.",
      ],
    ] as const)(
      "explains %s instead of pasting nothing",
      async (_label, capabilities, image, message) => {
        withCapabilities(capabilities);
        const event = pasteEvent("", [image]);

        const { terminal } = await pasteInto(event);

        expect(event.defaultPrevented).toBe(true);
        expect(testState.stageImagePasteCommand).not.toHaveBeenCalled();
        expect(terminal.writes).toContain(`\r\n[terminal] ${message}\r\n`);
      },
    );

    it("reports a failed upload in the terminal", async () => {
      withCapabilities({ terminalImagePaste: true });
      testState.stageImagePasteCommand.mockResolvedValue(
        AsyncResult.failure(Cause.fail(new Error("Attachment exceeds upload size limit"))),
      );

      const { terminal } = await pasteInto(pasteEvent("", [png()]));

      expect(sentData()).toBe("");
      expect(terminal.writes).toContain(
        "\r\n[terminal] Image paste failed: Attachment exceeds upload size limit\r\n",
      );
    });
  });

  it("leaves context menus alone when selection UI is unavailable", async () => {
    const mounted = await mount(<TerminalViewport {...viewportProps()} />);
    await flushAnimationFrames();
    const terminal = xtermState.terminals[0]!;
    const mountElement = mounted.container.querySelector<HTMLElement>(
      '[data-terminal-xterm-mount="term-1"]',
    )!;

    terminal.hasActiveSelection = true;
    terminal.selectionText = "selected";
    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2 });
    await act(async () => {
      mountElement.dispatchEvent(event);
    });
    expect(event.defaultPrevented).toBe(false);
    expect(testState.contextMenuShow).not.toHaveBeenCalled();
  });

  it("rejects incomplete or stale native selection actions", async () => {
    testState.localApiAvailable = true;
    let resolveMenu!: (value: string) => void;
    testState.contextMenuShow.mockImplementation(
      () =>
        new Promise<string>((resolve) => {
          resolveMenu = resolve;
        }),
    );
    const onAddTerminalContext = vi.fn();
    const mounted = await mount(<TerminalViewport {...viewportProps({ onAddTerminalContext })} />);
    const terminal = xtermState.terminals[0]!;
    const mountElement = mounted.container.querySelector<HTMLElement>(
      '[data-terminal-xterm-mount="term-1"]',
    )!;

    const requestSelectionAction = async () => {
      await act(async () => {
        mountElement.dispatchEvent(
          new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2 }),
        );
        await Promise.resolve();
      });
    };

    await requestSelectionAction();
    expect(testState.contextMenuShow).not.toHaveBeenCalled();

    terminal.hasActiveSelection = true;
    terminal.selectionText = "selected";
    terminal.selectionPosition = null;
    await requestSelectionAction();
    expect(testState.contextMenuShow).not.toHaveBeenCalled();

    terminal.selectionText = "\r\n";
    terminal.selectionPosition = { start: { y: 0 } };
    await requestSelectionAction();
    expect(testState.contextMenuShow).not.toHaveBeenCalled();

    terminal.selectionText = "selected";
    await requestSelectionAction();
    expect(testState.contextMenuShow).toHaveBeenCalledOnce();

    await requestSelectionAction();
    expect(testState.contextMenuShow).toHaveBeenCalledOnce();
    mountElement.dispatchEvent(
      new PointerEvent("pointerdown", { bubbles: true, button: 0, pointerId: 2 }),
    );
    resolveMenu("add-to-chat");
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(onAddTerminalContext).not.toHaveBeenCalled();
  });

  it("refreshes theme and refits when the viewport changes", async () => {
    const props = viewportProps();
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;
    const observer = observerInstances[0]!;
    testState.resolvedTheme = "dark";
    document.documentElement.classList.add("dark");
    try {
      observer.callback([], observer as never);
    } finally {
      document.documentElement.classList.remove("dark");
    }
    expect(terminal.refresh).toHaveBeenCalledWith(0, 23);

    await act(async () => mounted.root.render(<TerminalViewport {...props} resizeEpoch={1} />));
    await act(async () => {
      for (const callback of animationFrames.values()) callback(0);
      animationFrames.clear();
    });
    expect(xtermState.fitAddons[0]?.fit).toHaveBeenCalled();
    expect(testState.resizeCommand).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: { threadId: THREAD_ID, terminalId: "term-1", cols: 80, rows: 24 },
    });
  });

  it("repaints an existing terminal from the current resolved theme", async () => {
    const props = viewportProps();
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;
    const observer = observerInstances[0]!;

    testState.resolvedTheme = "dark";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    expect(xtermState.terminals).toHaveLength(1);
    expect(terminal.options.theme).toEqual(
      expect.objectContaining({
        cursor: "rgb(180, 203, 255)",
        brightWhite: "rgb(244, 247, 252)",
      }),
    );

    document.documentElement.classList.add("dark");
    try {
      observer.callback([], observer as never);
    } finally {
      document.documentElement.classList.remove("dark");
    }

    expect(terminal.options.theme).toEqual(
      expect.objectContaining({
        cursor: "rgb(180, 203, 255)",
        brightWhite: "rgb(244, 247, 252)",
      }),
    );
  });

  it("uses the authoritative console theme when reattaching a Windows Codex terminal", async () => {
    testState.resolvedTheme = "dark";
    testState.session.consoleTheme = "light";
    document.documentElement.classList.add("dark");
    document.body.style.backgroundColor = "rgb(14, 18, 24)";
    document.body.style.color = "rgb(237, 241, 247)";
    const command = decodeTerminalLaunchCommand({ executable: "codex.exe", args: [] })!;
    const props = viewportProps({ terminalLabel: "Codex Terminal", command });

    try {
      await mount(<TerminalViewport {...props} />);

      expect(xtermState.terminals[0]?.options.theme).toEqual(
        expect.objectContaining({ background: "rgb(255, 255, 255)" }),
      );
      expect(buttonByLabel("Restart Codex Terminal to apply Dark theme").disabled).toBe(false);
    } finally {
      document.documentElement.classList.remove("dark");
      document.body.style.removeProperty("background-color");
      document.body.style.removeProperty("color");
    }
  });

  it("ignores the Codex-only console theme when reattaching a Windows Cursor terminal", async () => {
    testState.resolvedTheme = "dark";
    testState.session.consoleTheme = "light";
    document.documentElement.classList.add("dark");
    document.body.style.backgroundColor = "rgb(14, 18, 24)";
    document.body.style.color = "rgb(237, 241, 247)";
    const command = decodeTerminalLaunchCommand({ executable: "cursor-agent.exe", args: [] })!;
    const props = viewportProps({ terminalLabel: "Cursor Terminal", command });

    try {
      await mount(<TerminalViewport {...props} />);

      expect(xtermState.terminals[0]?.options.theme).toEqual(
        expect.objectContaining({ background: "rgb(14, 18, 24)" }),
      );
      expect(document.querySelector('button[aria-label^="Restart Cursor Terminal"]')).toBeNull();
      const attachment = testState.attachedSessionInputs.at(-1) as {
        terminal: { env?: Record<string, string> };
      };
      expect(attachment.terminal.env).not.toHaveProperty("BIBCODE_WINDOWS_CONSOLE_THEME");
    } finally {
      document.documentElement.classList.remove("dark");
      document.body.style.removeProperty("background-color");
      document.body.style.removeProperty("color");
    }
  });

  it("keeps a running Codex launch palette until an explicit theme restart", async () => {
    testState.resolvedTheme = "dark";
    const command = decodeTerminalLaunchCommand({
      executable: "codex.exe",
      args: ["--dangerously-bypass-approvals-and-sandbox"],
      env: { COMMAND_ONLY: "yes" },
    })!;
    const props = viewportProps({
      terminalLabel: "Codex Terminal",
      command,
      worktreePath: null,
      runtimeEnv: { RUNTIME_ONLY: "yes" },
    });
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;
    const initialAttachment = testState.attachedSessionInputs.at(-1) as {
      terminal: { env?: Record<string, string> };
    };

    testState.resolvedTheme = "light";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    const attachmentAfterThemeChange = testState.attachedSessionInputs.at(-1) as {
      terminal: { env?: Record<string, string> };
    };
    expect(attachmentAfterThemeChange.terminal).toEqual(initialAttachment.terminal);
    expect(attachmentAfterThemeChange.terminal.env).toMatchObject({
      BIBCODE_OSC_BACKGROUND: "14,18,24",
      BIBCODE_WINDOWS_CONSOLE_THEME: "dark",
    });
    expect(xtermState.terminals).toHaveLength(1);
    expect(terminal.options.theme).toEqual(
      expect.objectContaining({ background: "rgb(14, 18, 24)" }),
    );
    const observer = observerInstances[0]!;
    observer.callback([], observer as never);
    expect(terminal.options.theme).toEqual(
      expect.objectContaining({ background: "rgb(14, 18, 24)" }),
    );

    await click(buttonByLabel("Restart Codex Terminal to apply Light theme"));

    expect(testState.restartCommand).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      input: {
        threadId: THREAD_ID,
        terminalId: "term-1",
        cwd: "/repo",
        worktreePath: null,
        cols: 80,
        rows: 24,
        env: {
          COMMAND_ONLY: "yes",
          RUNTIME_ONLY: "yes",
          BIBCODE_OSC_BACKGROUND: "255,255,255",
          BIBCODE_OSC_FOREGROUND: "28,33,41",
          BIBCODE_OSC_CURSOR: "38,56,78",
          BIBCODE_WINDOWS_CONSOLE_THEME: "light",
        },
        command,
      },
    });

    expect(terminal.options.theme).toEqual(
      expect.objectContaining({ background: "rgb(14, 18, 24)" }),
    );
    expect(buttonByLabel("Restart Codex Terminal to apply Light theme").disabled).toBe(true);
  });

  it("keeps the Windows Codex launch palette when a generation arrives after a theme change", async () => {
    testState.resolvedTheme = "dark";
    const command = decodeTerminalLaunchCommand({ executable: "codex.exe", args: [] })!;
    const props = viewportProps({ terminalLabel: "Codex Terminal", command });
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;

    testState.resolvedTheme = "light";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    const runtime = testState.session.transcriptRuntime!;
    runtime.ingest(restartedEvent("first authoritative generation"));
    testState.session = { ...testState.session, generation: runtime.metadata().generation };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    expect(terminal.options.theme).toEqual(
      expect.objectContaining({ background: "rgb(14, 18, 24)" }),
    );
    expect(buttonByLabel("Restart Codex Terminal to apply Light theme").disabled).toBe(false);
    const attachmentThemes = testState.attachedSessionInputs.map((value) => {
      const input = value as { terminal: { env?: Record<string, string> } };
      return input.terminal.env?.BIBCODE_WINDOWS_CONSOLE_THEME;
    });
    expect(attachmentThemes.filter((theme) => theme !== undefined)).toEqual(
      expect.arrayContaining(["dark"]),
    );
    expect(attachmentThemes).not.toContain("light");
  });

  it("advances the Windows Codex attachment theme once after a confirmed restart generation", async () => {
    testState.resolvedTheme = "dark";
    const command = decodeTerminalLaunchCommand({ executable: "codex.exe", args: [] })!;
    const props = viewportProps({ terminalLabel: "Codex Terminal", command });
    const mounted = await mount(<TerminalViewport {...props} />);

    testState.resolvedTheme = "light";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    await click(buttonByLabel("Restart Codex Terminal to apply Light theme"));

    const runtime = testState.session.transcriptRuntime!;
    runtime.ingest(restartedEvent("confirmed light restart"));
    testState.session = { ...testState.session, generation: runtime.metadata().generation };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    const confirmedAttachment = testState.attachedSessionInputs.at(-1) as {
      terminal: { env?: Record<string, string> };
    };
    expect(confirmedAttachment.terminal.env).toMatchObject({
      BIBCODE_OSC_BACKGROUND: "255,255,255",
      BIBCODE_WINDOWS_CONSOLE_THEME: "light",
    });

    runtime.ingest(restartedEvent("follow-up producer snapshot"));
    testState.session = { ...testState.session, generation: runtime.metadata().generation };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    const stableAttachment = testState.attachedSessionInputs.at(-1) as {
      terminal: { env?: Record<string, string> };
    };
    expect(stableAttachment.terminal.env).toMatchObject({
      BIBCODE_OSC_BACKGROUND: "255,255,255",
      BIBCODE_WINDOWS_CONSOLE_THEME: "light",
    });
    const attachmentThemeTransitions = testState.attachedSessionInputs
      .map((value) => {
        const input = value as { terminal: { env?: Record<string, string> } };
        return input.terminal.env?.BIBCODE_WINDOWS_CONSOLE_THEME;
      })
      .filter((theme): theme is string => theme !== undefined)
      .filter((theme, index, themes) => index === 0 || theme !== themes[index - 1]);
    expect(attachmentThemeTransitions).toEqual(["dark", "light"]);
    expect(xtermState.terminals).toHaveLength(1);
    expect(document.querySelector('button[aria-label^="Restart Codex Terminal"]')).toBeNull();
  });

  it("adopts the new Codex palette only after the restarted generation arrives", async () => {
    testState.resolvedTheme = "dark";
    const command = decodeTerminalLaunchCommand({ executable: "codex", args: [] })!;
    const props = viewportProps({ terminalLabel: "Codex Terminal", command });
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;

    testState.resolvedTheme = "light";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    await click(buttonByLabel("Restart Codex Terminal to apply Light theme"));

    expect(document.querySelector('button[aria-label^="Restart Codex Terminal"]')).not.toBeNull();
    expect(terminal.options.theme).toEqual(
      expect.objectContaining({ background: "rgb(14, 18, 24)" }),
    );

    const runtime = testState.session.transcriptRuntime!;
    runtime.ingest(restartedEvent("restarted with light theme"));
    testState.session = {
      ...testState.session,
      generation: runtime.metadata().generation,
    };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    expect(xtermState.terminals).toHaveLength(1);
    expect(terminal.options.theme).toEqual(
      expect.objectContaining({ background: "rgb(255, 255, 255)" }),
    );
    expect(document.querySelector('button[aria-label^="Restart Codex Terminal"]')).toBeNull();
  });

  it("adopts the requested Codex process theme when the app theme changes before generation", async () => {
    testState.resolvedTheme = "dark";
    const command = decodeTerminalLaunchCommand({ executable: "codex", args: [] })!;
    const props = viewportProps({ terminalLabel: "Codex Terminal", command });
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;

    testState.resolvedTheme = "light";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    await click(buttonByLabel("Restart Codex Terminal to apply Light theme"));

    testState.resolvedTheme = "dark";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    expect(document.querySelector('button[aria-label^="Restart Codex Terminal"]')).toBeNull();

    const runtime = testState.session.transcriptRuntime!;
    runtime.ingest(restartedEvent("process launched with requested light theme"));
    testState.session = { ...testState.session, generation: runtime.metadata().generation };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    expect(terminal.options.theme).toEqual(
      expect.objectContaining({ background: "rgb(255, 255, 255)" }),
    );
    expect(buttonByLabel("Restart Codex Terminal to apply Dark theme").disabled).toBe(false);
  });

  it("keeps a successful restart pending and deduplicated until generation advances", async () => {
    testState.resolvedTheme = "dark";
    const command = decodeTerminalLaunchCommand({ executable: "codex", args: [] })!;
    const props = viewportProps({ terminalLabel: "Codex Terminal", command });
    const mounted = await mount(<TerminalViewport {...props} />);
    testState.resolvedTheme = "light";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    const restartButton = buttonByLabel("Restart Codex Terminal to apply Light theme");
    await act(async () => {
      restartButton.click();
      restartButton.click();
      await Promise.resolve();
    });

    expect(testState.restartCommand).toHaveBeenCalledOnce();
    expect(buttonByLabel("Restart Codex Terminal to apply Light theme").disabled).toBe(true);

    await click(buttonByLabel("Restart Codex Terminal to apply Light theme"));
    expect(testState.restartCommand).toHaveBeenCalledOnce();
  });

  it("does not close the terminal for a transient closed event during a theme restart", async () => {
    testState.resolvedTheme = "dark";
    const onSessionExited = vi.fn();
    const command = decodeTerminalLaunchCommand({ executable: "codex", args: [] })!;
    const props = viewportProps({ terminalLabel: "Codex Terminal", command, onSessionExited });
    const mounted = await mount(<TerminalViewport {...props} />);

    testState.resolvedTheme = "light";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    await click(buttonByLabel("Restart Codex Terminal to apply Light theme"));

    const runtime = testState.session.transcriptRuntime!;
    runtime.ingest({
      type: "closed",
      threadId: THREAD_ID,
      terminalId: "term-1",
      sequence: 2,
    });
    testState.session = {
      ...testState.session,
      status: runtime.metadata().status,
    };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    expect(onSessionExited).not.toHaveBeenCalled();
  });

  it.each([
    ["success", AsyncResult.success(undefined)],
    ["failure", AsyncResult.failure(Cause.fail(new Error("late restart failure")))],
  ])(
    "ignores a late %s result after the requested generation was consumed",
    async (_label, lateResult) => {
      let resolveRestart!: (result: typeof lateResult) => void;
      testState.restartCommand.mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveRestart = resolve;
          }),
      );
      testState.resolvedTheme = "dark";
      const command = decodeTerminalLaunchCommand({ executable: "codex", args: [] })!;
      const props = viewportProps({ terminalLabel: "Codex Terminal", command });
      const mounted = await mount(<TerminalViewport {...props} />);
      testState.resolvedTheme = "light";
      await act(async () => mounted.root.render(<TerminalViewport {...props} />));
      await click(buttonByLabel("Restart Codex Terminal to apply Light theme"));

      testState.resolvedTheme = "dark";
      const runtime = testState.session.transcriptRuntime!;
      runtime.ingest(restartedEvent("new requested generation"));
      testState.session = { ...testState.session, generation: runtime.metadata().generation };
      await act(async () => mounted.root.render(<TerminalViewport {...props} />));
      expect(buttonByLabel("Restart Codex Terminal to apply Dark theme").disabled).toBe(false);

      await act(async () => resolveRestart(lateResult));

      expect(buttonByLabel("Restart Codex Terminal to apply Dark theme").disabled).toBe(false);
      expect(mounted.container.querySelector('[role="alert"]')).toBeNull();
    },
  );

  it("repaints a non-Windows provider terminal live without offering a restart", async () => {
    testState.serverConfig = {
      availableEditors: ["vscode"],
      environment: { platform: { os: "darwin" } },
    };
    testState.resolvedTheme = "dark";
    const props = viewportProps({
      terminalLabel: "Claude Terminal",
      command: decodeTerminalLaunchCommand({ executable: "claude", args: [] })!,
    });
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;

    testState.resolvedTheme = "light";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    expect(terminal.options.theme).toEqual(
      expect.objectContaining({ background: "rgb(255, 255, 255)" }),
    );
    expect(document.querySelector('button[aria-label^="Restart Codex Terminal"]')).toBeNull();
    expect(testState.restartCommand).not.toHaveBeenCalled();
  });

  it("keeps a running Codex launch palette on Darwin until an explicit theme restart", async () => {
    testState.serverConfig = {
      availableEditors: ["vscode"],
      environment: { platform: { os: "darwin" } },
    };
    testState.resolvedTheme = "dark";
    const command = decodeTerminalLaunchCommand({ executable: "codex", args: [] })!;
    const props = viewportProps({ terminalLabel: "Codex Terminal", command });
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;
    const initialAttachment = testState.attachedSessionInputs.at(-1) as {
      terminal: { env?: Record<string, string> };
    };

    testState.resolvedTheme = "light";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    const attachmentAfterThemeChange = testState.attachedSessionInputs.at(-1) as {
      terminal: { env?: Record<string, string> };
    };
    expect(attachmentAfterThemeChange.terminal).toEqual(initialAttachment.terminal);
    expect(attachmentAfterThemeChange.terminal.env).toMatchObject({
      BIBCODE_OSC_BACKGROUND: "14,18,24",
      BIBCODE_WINDOWS_CONSOLE_THEME: "dark",
    });
    expect(xtermState.terminals).toHaveLength(1);
    expect(terminal.options.theme).toEqual(
      expect.objectContaining({ background: "rgb(14, 18, 24)" }),
    );
    expect(buttonByLabel("Restart Codex Terminal to apply Light theme").disabled).toBe(false);
    expect(testState.restartCommand).not.toHaveBeenCalled();
  });

  it("dismisses the Codex theme notice without restarting the process", async () => {
    testState.resolvedTheme = "dark";
    const props = viewportProps({
      terminalLabel: "Codex Terminal",
      command: decodeTerminalLaunchCommand({ executable: "codex", args: [] })!,
    });
    const mounted = await mount(<TerminalViewport {...props} />);

    testState.resolvedTheme = "light";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    await click(buttonByLabel("Dismiss terminal theme notice"));

    expect(testState.restartCommand).not.toHaveBeenCalled();
    expect(document.querySelector('button[aria-label^="Restart Codex Terminal"]')).toBeNull();
  });

  it("disables the Codex theme notice controls while restart is pending", async () => {
    let resolveRestart!: (result: ReturnType<typeof AsyncResult.success>) => void;
    testState.restartCommand.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveRestart = resolve;
        }),
    );
    testState.resolvedTheme = "dark";
    const props = viewportProps({
      terminalLabel: "Codex Terminal",
      command: decodeTerminalLaunchCommand({ executable: "codex", args: [] })!,
    });
    const mounted = await mount(<TerminalViewport {...props} />);
    testState.resolvedTheme = "light";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    await click(buttonByLabel("Restart Codex Terminal to apply Light theme"));

    expect(buttonByLabel("Restart Codex Terminal to apply Light theme").disabled).toBe(true);
    expect(buttonByLabel("Dismiss terminal theme notice").disabled).toBe(true);
    expect(mounted.container.textContent).toContain("Restarting");

    await act(async () => resolveRestart(AsyncResult.success(undefined)));
    expect(buttonByLabel("Restart Codex Terminal to apply Light theme").disabled).toBe(true);
  });

  it("keeps a failed Codex theme restart visible and retryable", async () => {
    testState.restartCommand
      .mockResolvedValueOnce(AsyncResult.failure(Cause.fail(new Error("restart unavailable"))))
      .mockResolvedValueOnce(AsyncResult.success(undefined));
    testState.resolvedTheme = "dark";
    const props = viewportProps({
      terminalLabel: "Codex Terminal",
      command: decodeTerminalLaunchCommand({ executable: "codex", args: [] })!,
    });
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;
    testState.resolvedTheme = "light";
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));

    await click(buttonByLabel("Restart Codex Terminal to apply Light theme"));

    expect(mounted.container.querySelector('[role="alert"]')?.textContent).toContain(
      "restart unavailable",
    );
    expect(buttonByLabel("Restart Codex Terminal to apply Light theme").disabled).toBe(false);
    expect(terminal.options.theme).toEqual(
      expect.objectContaining({ background: "rgb(14, 18, 24)" }),
    );

    await click(buttonByLabel("Restart Codex Terminal to apply Light theme"));
    expect(testState.restartCommand).toHaveBeenCalledTimes(2);
  });

  it("skips the theme refresh when a document mutation leaves the resolved theme unchanged", async () => {
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const observer = observerInstances[0]!;
    terminal.refresh.mockClear();

    // Class/style churn on <html> (tooltips, scroll locks) must not force a
    // full repaint when the terminal theme it resolves to is identical.
    observer.callback([], observer as never);

    expect(terminal.refresh).not.toHaveBeenCalled();
  });

  it("tolerates an unmeasurable terminal and applies the dark application theme", async () => {
    xtermState.fitShouldThrow = true;
    testState.resolvedTheme = "dark";
    document.documentElement.classList.add("dark");
    const mounted = await mount(
      <TerminalViewport
        {...viewportProps({ runtimeEnv: { Z_VAR: "last", "": "ignored", A_VAR: "first" } })}
      />,
    );
    const terminal = xtermState.terminals[0]!;

    expect(xtermState.fitAddons[0]?.fit).toHaveBeenCalled();
    expect(terminal.options.theme).toEqual(
      expect.objectContaining({
        cursor: "rgb(180, 203, 255)",
        brightWhite: "rgb(244, 247, 252)",
      }),
    );

    await act(async () => {
      await mounted.root.render(
        <TerminalViewport {...viewportProps({ runtimeEnv: { A_VAR: "first", Z_VAR: "last" } })} />,
      );
    });
    expect(xtermState.terminals).toHaveLength(1);
    document.documentElement.classList.remove("dark");
  });

  it("renders the exited session message and resets exit handling after a restart", async () => {
    const onSessionExited = vi.fn();
    const props = viewportProps({ onSessionExited });
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;
    const runtime = testState.session.transcriptRuntime!;

    testState.session = { ...testState.session, status: "exited" };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    expect(terminal.writes).toContain("\r\n[terminal] Process exited\r\n");
    expect(onSessionExited).toHaveBeenCalledOnce();

    runtime.ingest(restartedEvent("done"));
    testState.session = {
      ...testState.session,
      status: "running",
      generation: runtime.metadata().generation,
    };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    testState.session = { ...testState.session, status: "exited" };
    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    expect(onSessionExited).toHaveBeenCalledTimes(2);
  });

  it("provides only terminal links that intersect the requested wrapped buffer row", async () => {
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(
      terminalBufferLine("see https://example.test/docs and "),
      terminalBufferLine("src/main.ts:12", true),
      terminalBufferLine("plain output"),
    );

    expect(provideTerminalLinks(terminal, 1)?.map((link) => link.text)).toEqual([
      "https://example.test/docs",
    ]);
    expect(provideTerminalLinks(terminal, 2)?.map((link) => link.text)).toEqual(["src/main.ts:12"]);
    expect(provideTerminalLinks(terminal, 3)).toBeUndefined();
    expect(provideTerminalLinks(terminal, 99)).toBeUndefined();
  });

  it("requires the platform link modifier and reports unavailable browser links", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel" });
    vi.mocked(openLink).mockReturnValue("unavailable");
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("https://example.test/docs"));
    const [link] = provideTerminalLinks(terminal) ?? [];
    expect(link).toBeDefined();
    terminal.writes.length = 0;

    link!.activate(new MouseEvent("click"));
    expect(terminal.writes).toEqual([]);
    expect(openLink).not.toHaveBeenCalled();

    link!.activate(new MouseEvent("click", { metaKey: true }));
    expect(terminal.writes).toContain(
      "\r\n[terminal] Opening links is unavailable in this browser.\r\n",
    );
  });

  it("routes Ctrl-clicked URLs through the link router without a context menu", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    testState.localApiAvailable = true;
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("http://localhost:5173"));
    const [link] = provideTerminalLinks(terminal) ?? [];

    link!.activate(new MouseEvent("click", { ctrlKey: true }));
    expect(openLink).toHaveBeenLastCalledWith(
      expect.objectContaining({
        url: "http://localhost:5173",
        threadRef: THREAD_REF,
        invert: false,
      }),
    );

    link!.activate(new MouseEvent("click", { ctrlKey: true, shiftKey: true }));
    expect(openLink).toHaveBeenLastCalledWith(
      expect.objectContaining({ url: "http://localhost:5173", invert: true }),
    );
    expect(testState.contextMenuShow).not.toHaveBeenCalled();
  });

  it("reports link router failures in the terminal", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel" });
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("https://example.test/docs"));
    const [link] = provideTerminalLinks(terminal) ?? [];

    link!.activate(new MouseEvent("click", { metaKey: true }));
    const { onError } = vi.mocked(openLink).mock.calls[0]![0];
    onError?.(new Error("browser blocked"));
    expect(terminal.writes).toContain("\r\n[terminal] browser blocked\r\n");
    onError?.("unknown");
    expect(terminal.writes).toContain("\r\n[terminal] Unable to open link\r\n");
  });

  it("routes OSC 8 hyperlinks through the link router behind the activation modifier", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    expect((terminal.options.linkHandler as Osc8LinkHandler).allowNonHttpProtocols).toBe(false);
    const click = osc8Link(terminal, "example.test/osc", "https://example.test/osc");

    click(new MouseEvent("click"));
    expect(openLink).not.toHaveBeenCalled();

    click(new MouseEvent("click", { ctrlKey: true }));
    expect(openLink).toHaveBeenLastCalledWith(
      expect.objectContaining({ url: "https://example.test/osc", invert: false }),
    );

    click(new MouseEvent("click", { ctrlKey: true, shiftKey: true }));
    expect(openLink).toHaveBeenLastCalledWith(
      expect.objectContaining({ url: "https://example.test/osc", invert: true }),
    );
    expect(testState.contextMenuShow).not.toHaveBeenCalled();
  });

  it("uses Cmd as the OSC 8 activation modifier on macOS", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel" });
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const click = osc8Link(terminal, "https://example.test/osc", "https://example.test/osc");

    click(new MouseEvent("click", { ctrlKey: true }));
    expect(openLink).not.toHaveBeenCalled();
    click(new MouseEvent("click", { metaKey: true }));
    expect(openLink).toHaveBeenLastCalledWith(
      expect.objectContaining({ url: "https://example.test/osc", invert: false }),
    );
  });

  it("confirms OSC 8 links whose visible text names a different target", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    testState.localApiAvailable = true;
    testState.contextMenuShow.mockResolvedValue(null);
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const click = osc8Link(terminal, "docs.example.com", "https://evil.test/login?next=1");

    click(new MouseEvent("click", { ctrlKey: true, clientX: 24, clientY: 36 }));
    await act(async () => Promise.resolve());
    expect(testState.contextMenuShow).toHaveBeenCalledWith(
      [
        { id: "open", label: "Open https://evil.test/login" },
        { id: "copy", label: "Copy link" },
      ],
      { x: 24, y: 36 },
    );
    // Dismissing the menu opens and copies nothing.
    expect(openLink).not.toHaveBeenCalled();
  });

  it("opens a confirmed OSC 8 link with the click's invert value", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    testState.localApiAvailable = true;
    testState.contextMenuShow.mockResolvedValue("open");
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const click = osc8Link(terminal, "docs.example.com", "https://evil.test/");

    click(new MouseEvent("click", { ctrlKey: true, shiftKey: true }));
    await act(async () => Promise.resolve());
    expect(openLink).toHaveBeenLastCalledWith(
      expect.objectContaining({ url: "https://evil.test/", invert: true }),
    );
  });

  it("copies a confirmed OSC 8 link instead of opening it", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { platform: "Linux", clipboard: { writeText } });
    testState.localApiAvailable = true;
    testState.contextMenuShow.mockResolvedValue("copy");
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const click = osc8Link(terminal, "docs.example.com", "https://evil.test/");

    click(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    expect(writeText).toHaveBeenCalledWith("https://evil.test/");
    expect(openLink).not.toHaveBeenCalled();
  });

  it("falls back to the textarea copy and explains a failed OSC 8 link copy", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    // xterm's own copy listener would replace the payload with its selection, so the fallback
    // must copy from a textarea outside the terminal.
    const copiedFrom: Array<{ value: string; insideTerminal: boolean }> = [];
    const copy = vi.fn(() => {
      const active = document.activeElement as HTMLTextAreaElement;
      copiedFrom.push({
        value: active.value,
        insideTerminal: active === xtermState.terminals[0]!.textarea,
      });
      return true;
    });
    Object.defineProperty(document, "execCommand", { configurable: true, value: copy });
    testState.localApiAvailable = true;
    testState.contextMenuShow.mockResolvedValue("copy");
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const click = osc8Link(terminal, "docs.example.com", "https://evil.test/");
    terminal.writes.length = 0;
    // Focus moved to another pane while the menu was open; the copy must not take it away.
    const composer = document.createElement("input");
    document.body.append(composer);
    composer.focus();

    click(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    expect(document.activeElement).toBe(composer);
    composer.remove();
    expect(copy).toHaveBeenCalledWith("copy");
    expect(copiedFrom).toEqual([{ value: "https://evil.test/", insideTerminal: false }]);
    expect(terminal.writes).toEqual([]);

    copy.mockReturnValue(false);
    click(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    expect(terminal.writes).toContain(
      "\r\n[terminal] Couldn't copy the link: https://evil.test/\r\n",
    );
  });

  it("skips the link copy fallback once its terminal is gone", async () => {
    let reject: (cause: unknown) => void = () => undefined;
    const writeText = vi.fn(() => new Promise<void>((_, fail) => (reject = fail)));
    vi.stubGlobal("navigator", { platform: "Linux", clipboard: { writeText } });
    const copy = vi.fn(() => true);
    Object.defineProperty(document, "execCommand", { configurable: true, value: copy });
    testState.localApiAvailable = true;
    testState.contextMenuShow.mockResolvedValue("copy");
    const mounted = await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const click = osc8Link(terminal, "docs.example.com", "https://evil.test/");

    click(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    expect(writeText).toHaveBeenCalledOnce();
    await unmount(mounted);
    await act(async () => reject(new Error("not allowed")));
    expect(copy).not.toHaveBeenCalled();
  });

  it("ignores an OSC 8 menu choice that resolves after its terminal is gone", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    testState.localApiAvailable = true;
    let choose: (choice: string | null) => void = () => undefined;
    testState.contextMenuShow.mockReturnValue(
      new Promise<string | null>((resolve) => (choose = resolve)),
    );
    const mounted = await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const click = osc8Link(terminal, "docs.example.com", "https://evil.test/");

    click(new MouseEvent("click", { ctrlKey: true }));
    await unmount(mounted);
    await act(async () => choose("open"));
    expect(openLink).not.toHaveBeenCalled();
  });

  it("confirms a label linked inside a longer displayed address", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    testState.localApiAvailable = true;
    testState.contextMenuShow.mockResolvedValue(null);
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("https://evil.test.trusted.example/"));
    const handler = terminal.options.linkHandler as Osc8LinkHandler;

    handler.activate(new MouseEvent("click", { ctrlKey: true }), "https://evil.test/", {
      start: { x: 9, y: 1 },
      end: { x: 17, y: 1 },
    });
    await act(async () => Promise.resolve());
    expect(testState.contextMenuShow).toHaveBeenCalledOnce();
    expect(openLink).not.toHaveBeenCalled();
  });

  it("truncates long OSC 8 targets in the confirmation menu", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    testState.localApiAvailable = true;
    testState.contextMenuShow.mockResolvedValue(null);
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const click = osc8Link(terminal, "docs", `https://evil.test/${"a".repeat(120)}`);

    click(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    const [items] = testState.contextMenuShow.mock.calls[0]!;
    expect(items[0].label).toBe(`Open https://evil.test/${"a".repeat(61)}…`);
    expect(items[0].label.length).toBe(85);
  });

  it("keeps the end of a long destination hostname in the confirmation menu", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    testState.localApiAvailable = true;
    testState.contextMenuShow.mockResolvedValue(null);
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const host = `trusted.example.${"a".repeat(60)}.attacker.example`;
    const click = osc8Link(terminal, "docs", `https://${host}/login`);

    click(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    const [items] = testState.contextMenuShow.mock.calls[0]!;
    const label: string = items[0].label;
    expect(label).toMatch(/^Open https:\/\/….*\.attacker\.example\/login$/);
    // The fallback menu right-truncates near 24rem, so the owning domain must come early.
    expect(label.indexOf(".attacker.example") + ".attacker.example".length).toBeLessThanOrEqual(45);
  });

  it("reports unavailable OSC 8 confirmation without a local API", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const click = osc8Link(terminal, "docs.example.com", "https://evil.test/");

    click(new MouseEvent("click", { ctrlKey: true }));
    expect(openLink).not.toHaveBeenCalled();
    expect(terminal.writes).toContain(
      "\r\n[terminal] Opening links is unavailable in this browser.\r\n",
    );
  });

  it("refuses network paths without previewing or opening an editor", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(
      terminalBufferLine(
        "\\\\server\\share\\a.html //server/share/b.html \\\\.\\C:\\..\\UNC\\a\\s\\x.html //./C:/../UNC/a/s/x.html \\\\?\\C:\\x.html",
      ),
    );
    const links = provideTerminalLinks(terminal) ?? [];
    expect(links.map((link) => link.text)).toEqual([
      "\\\\server\\share\\a.html",
      "//server/share/b.html",
      "\\\\.\\C:\\..\\UNC\\a\\s\\x.html",
      "//./C:/../UNC/a/s/x.html",
      "\\\\?\\C:\\x.html",
    ]);

    // Plain clicks would preview; Ctrl+Shift clicks would open the editor. Both are refused.
    for (const link of links) {
      link.activate(new MouseEvent("click", { ctrlKey: true }));
      link.activate(new MouseEvent("click", { ctrlKey: true, shiftKey: true }));
    }
    await act(async () => Promise.resolve());
    expect(openFileInPreview).not.toHaveBeenCalled();
    expect(testState.openPath).not.toHaveBeenCalled();
    expect(
      terminal.writes.filter(
        (write) => write === "\r\n[terminal] Network paths can't be opened from the terminal.\r\n",
      ),
    ).toHaveLength(10);
  });

  it("opens relative links under a UNC working directory such as WSL", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    const cwd = "\\\\wsl.localhost\\Ubuntu\\home\\u\\proj";
    await mount(<TerminalViewport {...viewportProps({ cwd })} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("index.html src/main.ts:4"));
    const [htmlLink, sourceLink] = provideTerminalLinks(terminal) ?? [];
    terminal.writes.length = 0;

    htmlLink!.activate(new MouseEvent("click", { ctrlKey: true }));
    sourceLink!.activate(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    expect(openFileInPreview).toHaveBeenCalledWith(
      expect.objectContaining({ filePath: `${cwd}\\index.html` }),
    );
    expect(testState.openPath).toHaveBeenCalledWith(`${cwd}\\src\\main.ts:4`);
    expect(terminal.writes).toEqual([]);
  });

  it("keeps one OSC 8 confirmation menu open at a time", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    testState.localApiAvailable = true;
    let choose: (choice: string | null) => void = () => undefined;
    testState.contextMenuShow.mockImplementation(
      () => new Promise<string | null>((resolve) => (choose = resolve)),
    );
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    const click = osc8Link(terminal, "docs.example.com", "https://evil.test/");

    click(new MouseEvent("click", { ctrlKey: true }));
    click(new MouseEvent("click", { ctrlKey: true }));
    expect(testState.contextMenuShow).toHaveBeenCalledOnce();

    await act(async () => choose(null));
    click(new MouseEvent("click", { ctrlKey: true }));
    expect(testState.contextMenuShow).toHaveBeenCalledTimes(2);
  });

  it("refuses file URLs it cannot convert instead of treating them as relative paths", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("file://server/share/r.pdf file:///tmp/%E0.html"));
    const links = provideTerminalLinks(terminal) ?? [];
    expect(links).toHaveLength(2);

    for (const link of links) link.activate(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    expect(openFileInPreview).not.toHaveBeenCalled();
    expect(testState.openPath).not.toHaveBeenCalled();
    expect(
      terminal.writes.filter(
        (write) => write === "\r\n[terminal] Unable to open this file link.\r\n",
      ),
    ).toHaveLength(2);
  });

  it("opens html paths in the editor when the terminal has no thread", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    await mount(
      <TerminalViewport
        {...viewportProps({ threadRef: { ...THREAD_REF, threadId: "" as typeof THREAD_ID } })}
      />,
    );
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("dist/index.html"));
    const [link] = provideTerminalLinks(terminal) ?? [];

    link!.activate(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    expect(openFileInPreview).not.toHaveBeenCalled();
    expect(testState.openPath).toHaveBeenCalledWith("/repo/dist/index.html");
  });

  it("opens html paths in the integrated browser and Ctrl+Shift opens them in the editor", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("built dist/index.html"));
    const [link] = provideTerminalLinks(terminal) ?? [];

    link!.activate(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    expect(openFileInPreview).toHaveBeenCalledWith(
      expect.objectContaining({
        threadRef: THREAD_REF,
        filePath: "/repo/dist/index.html",
        httpBaseUrl: "http://127.0.0.1:3773",
        createAssetUrl: testState.createAssetUrl,
      }),
    );
    expect(testState.openPath).not.toHaveBeenCalled();

    link!.activate(new MouseEvent("click", { ctrlKey: true, shiftKey: true }));
    await act(async () => Promise.resolve());
    expect(openFileInPreview).toHaveBeenCalledTimes(1);
    expect(testState.openPath).toHaveBeenCalledWith("/repo/dist/index.html");
  });

  it("keeps the physical Shift for link activation while mouse tracking inverts it", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    const mounted = await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.modes.mouseTrackingMode = "drag";
    terminal.bufferLines.push(terminalBufferLine("http://localhost:5173 dist/index.html"));
    const [urlLink, fileLink] = provideTerminalLinks(terminal) ?? [];
    const surface = mounted.container.querySelector("[data-terminal-xterm-mount]")!;
    const target = document.createElement("div");
    surface.append(target);
    // xterm activates links from its own mouseup listener, after the capture-phase inversion.
    const clickWithCtrlShift = (link: FakeTerminalLink) => {
      const activate = (event: MouseEvent) => link.activate(event);
      document.addEventListener("mouseup", activate);
      const modifiers = { bubbles: true, button: 0, ctrlKey: true, shiftKey: true };
      target.dispatchEvent(new MouseEvent("mousedown", modifiers));
      target.dispatchEvent(new MouseEvent("mouseup", modifiers));
      document.removeEventListener("mouseup", activate);
    };

    clickWithCtrlShift(urlLink!);
    expect(openLink).toHaveBeenLastCalledWith(
      expect.objectContaining({ url: "http://localhost:5173", invert: true }),
    );

    clickWithCtrlShift(fileLink!);
    await act(async () => Promise.resolve());
    expect(openFileInPreview).not.toHaveBeenCalled();
    expect(testState.openPath).toHaveBeenCalledWith("/repo/dist/index.html");
  });

  it("keeps opening non-preview paths in the editor", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("src/main.ts:4"));
    const [link] = provideTerminalLinks(terminal) ?? [];

    link!.activate(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    expect(openFileInPreview).not.toHaveBeenCalled();
    expect(testState.openPath).toHaveBeenCalledWith("/repo/src/main.ts:4");
  });

  it("offers the editor when an html path is outside the workspace", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    vi.mocked(openFileInPreview).mockResolvedValue(
      AsyncResult.failure(Cause.fail({ _tag: "AssetWorkspacePathValidationError" })),
    );
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("/tmp/report.html"));
    const [link] = provideTerminalLinks(terminal) ?? [];
    terminal.writes.length = 0;

    link!.activate(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    expect(showFileOutsideWorkspaceNotice).toHaveBeenCalledOnce();
    expect(terminal.writes).toEqual([]);

    vi.mocked(showFileOutsideWorkspaceNotice).mock.calls[0]![0].onOpenInEditor();
    await act(async () => Promise.resolve());
    expect(testState.openPath).toHaveBeenCalledWith("/tmp/report.html");
  });

  it("offers the editor when an html preview fails for another reason", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    vi.mocked(openFileInPreview).mockResolvedValue(
      AsyncResult.failure(Cause.fail(new Error("asset service down"))),
    );
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("index.html"));
    const [link] = provideTerminalLinks(terminal) ?? [];
    terminal.writes.length = 0;

    link!.activate(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    expect(showFileOutsideWorkspaceNotice).not.toHaveBeenCalled();
    expect(showPreviewFailedNotice).toHaveBeenCalledOnce();
    expect(terminal.writes).toEqual([]);

    vi.mocked(showPreviewFailedNotice).mock.calls[0]![0].onOpenInEditor();
    await act(async () => Promise.resolve());
    expect(testState.openPath).toHaveBeenCalledWith("/repo/index.html");
  });

  it("offers the editor when the environment has no connection to preview through", async () => {
    vi.stubGlobal("navigator", { platform: "Linux" });
    testState.preparedConnection = Option.none();
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("index.html"));
    const [link] = provideTerminalLinks(terminal) ?? [];

    link!.activate(new MouseEvent("click", { ctrlKey: true }));
    await act(async () => Promise.resolve());
    expect(openFileInPreview).not.toHaveBeenCalled();
    expect(showPreviewFailedNotice).toHaveBeenCalledOnce();
  });

  it("opens path links and reports editor failures", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel" });
    await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("src/main.ts:12"));
    const [link] = provideTerminalLinks(terminal) ?? [];
    terminal.writes.length = 0;

    link!.activate(new MouseEvent("click", { metaKey: true }));
    await act(async () => Promise.resolve());
    expect(testState.openPath).toHaveBeenCalledWith("/repo/src/main.ts:12");
    expect(terminal.writes).toEqual([]);

    testState.openPath.mockResolvedValueOnce(
      AsyncResult.failure(Cause.fail(new Error("editor unavailable"))),
    );
    link!.activate(new MouseEvent("click", { metaKey: true }));
    await act(async () => Promise.resolve());
    expect(terminal.writes).toContain("\r\n[terminal] editor unavailable\r\n");

    testState.openPath.mockResolvedValueOnce(AsyncResult.failure(Cause.fail("unknown")));
    link!.activate(new MouseEvent("click", { metaKey: true }));
    await act(async () => Promise.resolve());
    expect(terminal.writes).toContain("\r\n[terminal] Unable to open path\r\n");
  });

  it("keeps retained path links mounted but guards activation while the workspace is unavailable", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel" });
    const props = viewportProps();
    const mounted = await mount(<TerminalViewport {...props} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("retained src/main.ts:12"));
    const [link] = provideTerminalLinks(terminal) ?? [];
    expect(link).toBeDefined();
    terminal.writes.length = 0;

    const reason = "Workspace unavailable. Retry detection or remove it from BiBCode.";
    await act(async () =>
      mounted.root.render(<TerminalViewport {...props} workspaceUnavailable={reason} />),
    );

    expect(xtermState.terminals).toHaveLength(1);
    expect(terminal.dispose).not.toHaveBeenCalled();
    link!.activate(new MouseEvent("click", { metaKey: true }));
    await act(async () => Promise.resolve());
    expect(testState.openPath).not.toHaveBeenCalled();
    expect(terminal.writes).toContain(`\r\n[terminal] ${reason}\r\n`);

    await act(async () => mounted.root.render(<TerminalViewport {...props} />));
    expect(xtermState.terminals).toHaveLength(1);
    link!.activate(new MouseEvent("click", { metaKey: true }));
    await act(async () => Promise.resolve());
    expect(testState.openPath).toHaveBeenCalledWith("/repo/src/main.ts:12");
  });

  it("ignores link providers and activations after the terminal is disposed", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel" });
    const mounted = await mount(<TerminalViewport {...viewportProps()} />);
    const terminal = xtermState.terminals[0]!;
    terminal.bufferLines.push(terminalBufferLine("https://example.test/docs"));
    const [link] = provideTerminalLinks(terminal) ?? [];
    terminal.writes.length = 0;

    await unmount(mounted);

    expect(provideTerminalLinks(terminal)).toBeUndefined();
    link!.activate(new MouseEvent("click", { metaKey: true }));
    expect(terminal.writes).toEqual([]);
  });
});

describe("ThreadTerminalPanel mounted controls", () => {
  it("forwards each split terminal's provider launch command to its own attachment", async () => {
    const codexCommand = {
      executable: "/opt/codex",
      args: ["--dangerously-bypass-approvals-and-sandbox"],
      label: "Codex Terminal",
    };
    const cursorCommand = {
      executable: "/opt/cursor-agent",
      args: ["--yolo"],
      label: "Cursor Terminal",
    };
    await mount(
      <ThreadTerminalPanel
        {...panelProps({
          threadRef: scopeThreadRef(EnvironmentId.make("environment-1"), THREAD_ID),
          terminalIds: ["term-1", "term-2"],
          terminalGroups: [{ id: "group-1", terminalIds: ["term-1", "term-2"] }],
          terminalCommandsById: new Map([
            ["term-1", codexCommand],
            ["term-2", cursorCommand],
          ]),
        })}
      />,
    );

    expect(testState.attachedSessionInputs).toEqual(
      expect.arrayContaining([
        {
          environmentId: "environment-1",
          terminal: expect.objectContaining({
            terminalId: "term-1",
            command: codexCommand,
          }),
          attach: true,
        },
        {
          environmentId: "environment-1",
          terminal: expect.objectContaining({
            terminalId: "term-2",
            command: cursorCommand,
          }),
          attach: true,
        },
      ]),
    );
  });

  it("creates the first terminal from the rendered empty state", async () => {
    const onNewTerminal = vi.fn();
    await mount(
      <ThreadTerminalPanel
        {...panelProps({ terminalIds: [], terminalGroups: [], onNewTerminal })}
      />,
    );

    const button = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
      (candidate) => candidate.textContent === "New Terminal",
    );
    expect(button).toBeDefined();
    await click(button!);
    expect(onNewTerminal).toHaveBeenCalledOnce();
  });

  it("renders an empty panel without resize or create controls", async () => {
    const mounted = await mount(
      <ThreadTerminalPanel
        {...panelProps({
          terminalIds: [],
          terminalGroups: [],
          onNewTerminal: undefined,
        })}
      />,
    );

    const aside = mounted.container.querySelector("aside");
    expect(aside?.getAttribute("data-terminal-owner")).toBe("right-panel");
    expect(aside?.getAttribute("style")).toBeNull();
    expect(mounted.container.querySelector(".cursor-row-resize")).toBeNull();
    expect(mounted.container.querySelector("button")).toBeNull();
  });

  it("runs split, new, per-session close, and terminal activation from rendered controls", async () => {
    const onSplitTerminal = vi.fn();
    const onSplitTerminalVertical = vi.fn();
    const onNewTerminal = vi.fn();
    const onCloseTerminal = vi.fn();
    const onActiveTerminalChange = vi.fn();
    await mount(
      <ThreadTerminalPanel
        {...panelProps({
          terminalIds: ["term-1", "term-2"],
          terminalGroups: [
            { id: "group-1", terminalIds: ["term-1"] },
            { id: "group-2", terminalIds: ["term-2"] },
          ],
          onSplitTerminal,
          onSplitTerminalVertical,
          onNewTerminal,
          onCloseTerminal,
          onActiveTerminalChange,
        })}
      />,
    );

    await click(buttonByLabel("Split Terminal Horizontally"));
    await click(buttonByLabel("Split Terminal Vertically"));
    await click(buttonByLabel("New Terminal"));
    await click(buttonByLabel("Close Terminal 1"));
    expect(onSplitTerminal).toHaveBeenCalledOnce();
    expect(onSplitTerminalVertical).toHaveBeenCalledOnce();
    expect(onNewTerminal).toHaveBeenCalledOnce();
    expect(onCloseTerminal).toHaveBeenCalledWith("term-1");

    const terminalTwo = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
      (candidate) => candidate.textContent?.includes("Terminal 2"),
    );
    expect(terminalTwo).toBeDefined();
    await click(terminalTwo!);
    expect(onActiveTerminalChange).toHaveBeenCalledWith("term-2");
  });

  it("normalizes malformed groups and enforces the split limit for a vertical group", async () => {
    const onSplitTerminal = vi.fn();
    const onSplitTerminalVertical = vi.fn();
    const onNewTerminal = vi.fn();
    const onCloseTerminal = vi.fn();
    const onActiveTerminalChange = vi.fn();
    const mounted = await mount(
      <ThreadTerminalPanel
        {...panelProps({
          worktreePath: "/worktree",
          runtimeEnv: { ROOT_TERMINAL: "1" },
          terminalIds: [" term-1 ", "term-1", "", "term-2", "term-3", "term-4", "orphan"],
          activeTerminalId: "missing",
          activeTerminalGroupId: "missing",
          terminalGroups: [
            {
              id: "",
              terminalIds: ["", "missing", "term-1", "term-1", "term-2", "term-3", "term-4"],
              splitDirection: "vertical",
            },
            { id: "group-term-1", terminalIds: ["term-1"] },
            { id: "group-term-1", terminalIds: ["orphan"] },
          ],
          onSplitTerminal,
          onSplitTerminalVertical,
          onNewTerminal,
          onCloseTerminal,
          onActiveTerminalChange,
          splitShortcutLabel: "Ctrl+H",
          splitVerticalShortcutLabel: "Ctrl+V",
          newShortcutLabel: "Ctrl+N",
          closeShortcutLabel: "Ctrl+W",
          terminalLabelsById: new Map([["term-1", "Primary"]]),
          terminalLaunchLocationsById: new Map([
            [
              "term-2",
              {
                cwd: "/alternate",
                worktreePath: null,
                runtimeEnv: { TERMINAL_VARIANT: "secondary" },
              },
            ],
          ]),
        })}
      />,
    );

    expect(xtermState.terminals).toHaveLength(4);
    expect(mounted.container.textContent).toContain("Primary");
    expect(mounted.container.querySelector<HTMLElement>(".grid")?.style.gridTemplateRows).toContain(
      "repeat(4",
    );
    expect(testState.attachedSessionInputs).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          terminal: expect.objectContaining({
            terminalId: "term-2",
            cwd: "/alternate",
            worktreePath: null,
            // env now always carries the reserved OSC theme colors too.
            env: expect.objectContaining({ TERMINAL_VARIANT: "secondary" }),
          }),
        }),
      ]),
    );

    await click(buttonByLabel("Split Terminal Horizontally (max 4 per group)"));
    await click(buttonByLabel("Split Terminal Vertically (max 4 per group)"));
    expect(onSplitTerminal).not.toHaveBeenCalled();
    expect(onSplitTerminalVertical).not.toHaveBeenCalled();

    await click(buttonByLabel("New Terminal (Ctrl+N)"));
    await click(buttonByLabel("Close Primary (Ctrl+W)"));
    expect(onNewTerminal).toHaveBeenCalledOnce();
    expect(onCloseTerminal).toHaveBeenCalledWith("term-1");

    const splitPanes = Array.from(
      mounted.container.querySelector<HTMLElement>(".grid")?.children ?? [],
    ) as HTMLElement[];
    expect(splitPanes).toHaveLength(4);
    splitPanes[0]!.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(onActiveTerminalChange).not.toHaveBeenCalled();
    splitPanes[1]!.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(onActiveTerminalChange).toHaveBeenCalledWith("term-2");
  });

  it("shows shortcut labels for an under-limit horizontal split", async () => {
    const onSplitTerminal = vi.fn();
    const onSplitTerminalVertical = vi.fn();
    await mount(
      <ThreadTerminalPanel
        {...panelProps({
          terminalIds: ["term-1", "term-2"],
          activeTerminalId: "term-2",
          terminalGroups: [{ id: "group-1", terminalIds: ["term-1", "term-2"] }],
          onSplitTerminal,
          onSplitTerminalVertical,
          splitShortcutLabel: "Ctrl+H",
          splitVerticalShortcutLabel: "Ctrl+V",
        })}
      />,
    );

    await click(buttonByLabel("Split Terminal Horizontally (Ctrl+H)"));
    await click(buttonByLabel("Split Terminal Vertically (Ctrl+V)"));
    expect(onSplitTerminal).toHaveBeenCalledOnce();
    expect(onSplitTerminalVertical).toHaveBeenCalledOnce();
  });

  it("derives a default group and omits unavailable terminal actions", async () => {
    const onCloseTerminal = vi.fn();
    await mount(
      <ThreadTerminalPanel
        {...panelProps({
          terminalIds: ["term-1"],
          activeTerminalId: "missing",
          activeTerminalGroupId: "missing",
          terminalGroups: [],
          onSplitTerminal: undefined,
          onSplitTerminalVertical: undefined,
          onNewTerminal: undefined,
          onCloseTerminal,
        })}
      />,
    );

    expect(document.querySelectorAll('button[aria-label^="Split Terminal"]')).toHaveLength(0);
    expect(document.querySelector('button[aria-label^="New Terminal"]')).toBeNull();
    expect(document.querySelector('button[aria-label="Close Terminal"]')).toBeNull();
    expect(onCloseTerminal).not.toHaveBeenCalled();
  });

  it("disables create, split, restart, and terminal writes with one workspace reason", async () => {
    const reason = "Workspace unavailable. Retry detection or remove it from BiBCode.";
    const onSplitTerminal = vi.fn();
    const onNewTerminal = vi.fn();
    await mount(
      <ThreadTerminalPanel
        {...panelProps({ onSplitTerminal, onNewTerminal })}
        workspaceUnavailable={reason}
      />,
    );

    const split = buttonByLabel("Split Terminal Horizontally");
    const create = buttonByLabel("New Terminal");
    expect(split.disabled).toBe(true);
    expect(create.disabled).toBe(true);
    expect(split.title).toBe(reason);
    expect(create.title).toBe(reason);
    await click(split);
    await click(create);
    expect(onSplitTerminal).not.toHaveBeenCalled();
    expect(onNewTerminal).not.toHaveBeenCalled();

    const terminal = xtermState.terminals[0]!;
    terminal.dataHandler?.("echo blocked\r");
    await Promise.resolve();
    await Promise.resolve();
    expect(testState.writeCommand).not.toHaveBeenCalled();
  });
});
