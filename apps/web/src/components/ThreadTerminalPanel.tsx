import { useAtomValue } from "@effect/atom-react";
import { FitAddon } from "@xterm/addon-fit";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
  type AtomCommandResult,
} from "@bibcode/client-runtime/state/runtime";
import {
  createTerminalInputScheduler,
  createTerminalInputSchedulerRegistry,
  type TerminalInputScheduler,
  type TerminalInputSendResult,
  type TerminalRenderSignal,
  terminalInputKey,
} from "@bibcode/client-runtime/state/terminal";
import {
  Plus,
  SquareSplitHorizontal,
  SquareSplitVertical,
  TerminalSquare,
  XIcon,
} from "lucide-react";
import {
  type ResolvedKeybindingsConfig,
  type ScopedThreadRef,
  TerminalImagePasteMimeType,
  type TerminalLaunchCommand,
  type TerminalSize,
  type TerminalResizeInput,
  type ThreadId,
  type ProjectId,
} from "@bibcode/contracts";
import { getTerminalLabel } from "@bibcode/shared/terminalLabels";
import * as Schema from "effect/Schema";
import { Terminal, type ITheme } from "@xterm/xterm";
import {
  type ReactNode,
  useCallback,
  useEffect,
  useEffectEvent,
  useMemo,
  useRef,
  useState,
} from "react";
import { Popover, PopoverPopup, PopoverTrigger } from "~/components/ui/popover";
import { Button } from "~/components/ui/button";
import { type TerminalContextSelection } from "~/lib/terminalContext";
import { isMacPlatform, randomUUID } from "~/lib/utils";
import { useOpenInPreferredEditor } from "../editorPreferences";
import {
  collectWrappedTerminalLinkLine,
  extractTerminalLinks,
  fileUrlToPath,
  isNetworkPath,
  isTerminalLinkActivation,
  resolvePathLinkTarget,
  resolveWrappedTerminalLinkRange,
  terminalLinkLabelShowsUri,
  terminalPreviewFilePath,
  wrappedTerminalLinkRangeIntersectsBufferLine,
} from "../terminal-links";
import {
  isDiffToggleShortcut,
  isTerminalClearShortcut,
  isTerminalCloseShortcut,
  isTerminalNewShortcut,
  isTerminalSplitShortcut,
  isTerminalSplitVerticalShortcut,
  isTerminalNewCenterShortcut,
  terminalClipboardShortcut,
  terminalDeleteShortcutData,
  terminalNavigationShortcutData,
  terminalNewlineShortcutData,
} from "../keybindings";
import { MAX_TERMINALS_PER_GROUP, type ThreadTerminalGroup } from "../types";
import { readLocalApi } from "~/localApi";
import { useAttachedTerminalSession } from "../state/terminalSessions";
import { serverEnvironment } from "../state/server";
import { previewEnvironment } from "../state/preview";
import { terminalEnvironment } from "../state/terminal";
import { assetEnvironment } from "../state/assets";
import { readPreparedConnection } from "../state/session";
import { useAtomQueryRunner } from "../state/use-atom-query-runner";
import { openLink } from "~/browser/openLink";
import { showFileOutsideWorkspaceNotice, showPreviewFailedNotice } from "~/browser/linkNotices";
import { openFileInPreview } from "~/browser/openFileInPreview";
import { canPreviewAnyUrlInRuntime } from "~/previewStateStore";
import { createTerminalOutputSink } from "./terminalOutputSink";
import { installTerminalReplyGuard } from "./terminalReplyGuard";
import { proposeTerminalDimensions, registerTerminalSizeReader } from "./terminalSizing";
import {
  shouldClaimTerminalSize,
  terminalDimensionsEqual,
  terminalSizesEqual,
  hasForeignTerminalSizeOwner,
  shouldShowTerminalSizeNotice,
  type TerminalDimensions,
  type TerminalSizeTrigger,
} from "./terminalSizePolicy";
import { observeCanvasDevicePixelSize } from "./terminalDevicePixelCorrection";
import {
  loadTerminalWebglAddon,
  resizeWebglCanvasBackingStore,
  webglCanvasFrom,
  webglContextFrom,
  type WebglAddonInstance,
} from "./terminalWebgl";
import { useAtomCommand } from "../state/use-atom-command";
import { useEnvironmentSettings, usePrimarySettings } from "../hooks/useSettings";
import { useTheme } from "../hooks/useTheme";
import { ensureBundledTerminalFontLoaded, resolveTerminalFontFamily } from "../lib/terminalFont";
import {
  mergeTerminalSpawnEnv,
  resolveTerminalThemeMode,
  retainTerminalLaunchTheme,
  terminalExtendedAnsiPalette,
  usesPersistentWindowsConsoleTheme,
  type TerminalLaunchThemeState,
  type TerminalThemeMode,
} from "./terminalTheme";
import { ProviderTerminalActivityDock } from "./activity/ProviderTerminalActivityDock";

const terminalInputRegistry = createTerminalInputSchedulerRegistry();
const terminalInputBindings = new Map<string, TerminalInputBinding>();
// Retain dismissal across renderer/component remounts; terminal retirement
// releases it alongside the retained input resources below.
const dismissedTerminalSizes = new Map<string, TerminalSize>();
const TERMINAL_WRITE_INTERRUPTED = Symbol("terminal-write-interrupted");

interface TerminalInputBinding {
  renderer: {
    readonly owner: object;
    readonly terminal: Terminal;
    readonly onInputError: (message: string | null) => void;
  } | null;
  write: ((data: string, fallbackError: string) => Promise<TerminalInputSendResult>) | null;
  ordered: boolean;
  generation: number;
  attachment: { readonly runtime: object; readonly generation: number } | null;
  error: string | null;
  resetInput: (() => void) | null;
  readonly pendingFallbacks: Array<{
    remaining: number;
    message: string;
    notified?: boolean;
    onWriteError?: ((error: unknown) => void) | undefined;
  }>;
}

interface WebglContextLossDisposable {
  dispose(): void;
}

interface WebglLifecycle {
  readonly terminal: Terminal;
  readonly disposed: boolean;
  setContextLossDisposable(disposable: WebglContextLossDisposable): void;
  setDevicePixelDisposable(disposable: WebglContextLossDisposable): void;
  dispose(): void;
}

interface WebglLoseContextExtension {
  loseContext(): void;
}

function createWebglLifecycle(addon: WebglAddonInstance, terminal: Terminal): WebglLifecycle {
  let disposed = false;
  let contextLossDisposable: WebglContextLossDisposable | null = null;
  let devicePixelDisposable: WebglContextLossDisposable | null = null;

  return {
    terminal,
    get disposed() {
      return disposed;
    },
    setContextLossDisposable(disposable) {
      if (disposed) {
        try {
          disposable.dispose();
        } catch {
          // Best-effort listener cleanup must not make the terminal unusable.
        }
        return;
      }
      contextLossDisposable = disposable;
    },
    setDevicePixelDisposable(disposable) {
      if (disposed) {
        try {
          disposable.dispose();
        } catch {
          // Best-effort listener cleanup must not make the terminal unusable.
        }
        return;
      }
      devicePixelDisposable = disposable;
    },
    dispose() {
      if (disposed) return;
      disposed = true;

      const context = webglContextFrom(addon);

      try {
        devicePixelDisposable?.dispose();
      } catch {
        // Our own observer; never block the addon's teardown on it.
      }
      devicePixelDisposable = null;

      try {
        contextLossDisposable?.dispose();
      } catch {
        // The addon owns this registration too; continue with renderer cleanup.
      }
      contextLossDisposable = null;

      try {
        addon.dispose();
      } catch {
        // Falling back must remain safe even if a partially activated addon throws.
      }

      try {
        (
          context?.getExtension("WEBGL_lose_context") as WebglLoseContextExtension | null
        )?.loseContext();
      } catch {
        // Context release is best effort because this is an implementation detail.
      }
    },
  };
}

/** Retire the retained input binding and size-notice dismissal together. */
export function releaseTerminalUiResources(
  environmentId: string,
  threadId: string,
  terminalId: string,
): void {
  const key = terminalInputKey(environmentId, threadId, terminalId);
  const binding = terminalInputBindings.get(key);
  terminalInputRegistry.release(key);
  if (binding) {
    binding.renderer = null;
    binding.write = null;
    binding.pendingFallbacks.length = 0;
  }
  terminalInputBindings.delete(key);
  dismissedTerminalSizes.delete(key);
}

function atomCommandTerminalInputResult(
  result: AtomCommandResult<unknown, unknown>,
  fallbackError: string,
): TerminalInputSendResult {
  if (result._tag === "Success") {
    return { ok: true };
  }
  if (isAtomCommandInterrupted(result)) {
    return { ok: false, error: TERMINAL_WRITE_INTERRUPTED };
  }
  const error = squashAtomCommandFailure(result);
  return {
    ok: false,
    error: error instanceof Error ? error : new Error(fallbackError),
  };
}

function createTerminalInputWriter<A, E>(input: {
  readonly environmentId: ScopedThreadRef["environmentId"];
  readonly threadId: ThreadId;
  readonly terminalId: string;
  readonly run: (request: {
    readonly environmentId: ScopedThreadRef["environmentId"];
    readonly input: {
      readonly threadId: ThreadId;
      readonly terminalId: string;
      readonly data: string;
    };
  }) => Promise<AtomCommandResult<A, E>>;
}): NonNullable<TerminalInputBinding["write"]> {
  const { environmentId, threadId, terminalId, run } = input;
  return async (data, fallbackError) => {
    const result = await run({
      environmentId,
      input: { threadId, terminalId, data },
    });
    return atomCommandTerminalInputResult(result, fallbackError);
  };
}

export function enqueueTerminalInput<A, E>(input: {
  readonly environmentId: string;
  readonly threadId: string;
  readonly terminalId: string;
  readonly data: string;
  readonly fallbackError: string;
  readonly write: (data: string) => Promise<AtomCommandResult<A, E>>;
  readonly onWriteError?: ((error: unknown) => void) | undefined;
}): void {
  if (input.data.length === 0) return;
  const inputKey = terminalInputKey(input.environmentId, input.threadId, input.terminalId);
  const { binding, scheduler } = acquireTerminalInputBinding(inputKey);
  if (binding.ordered && binding.error !== null) {
    try {
      input.onWriteError?.(new Error(binding.error));
    } catch {
      // Failure observers are isolated from input admission.
    }
    return;
  }
  binding.write = async (data, fallbackError) =>
    atomCommandTerminalInputResult(await input.write(data), fallbackError);
  binding.pendingFallbacks.push({
    remaining: input.data.length,
    message: input.fallbackError,
    onWriteError: input.onWriteError,
  });
  scheduler.enqueue(input.data);
}

const isTerminalImagePasteMimeType = Schema.is(TerminalImagePasteMimeType);

/**
 * Escapes a server path so a CLI that tokenizes pasted text (Codex, Claude Code)
 * sees one path: backslash escapes on POSIX hosts, quotes on Windows.
 */
export function quoteTerminalPastePath(path: string, os: string | undefined): string {
  if ((os === "windows" ? /^[\w./:\\-]+$/ : /^[\w./:-]+$/).test(path)) return path;
  return os === "windows" ? `"${path}"` : path.replace(/[^\w./-]/g, "\\$&");
}

const LINK_TARGET_LABEL_LENGTH = 80;
// The fallback context menu right-truncates near 24rem, so the host's owning domain (its end)
// must land early in the label.
const LINK_TARGET_HOST_LENGTH = 32;

/**
 * Origin and path of a link target for a confirmation label, about 80 characters. A long host
 * keeps its end, which names the domain's owner; the path is shortened to fit.
 */
function describeLinkTarget(uri: string): string {
  let url: URL;
  try {
    url = new URL(uri);
  } catch {
    return uri.length > LINK_TARGET_LABEL_LENGTH
      ? `${uri.slice(0, LINK_TARGET_LABEL_LENGTH - 1)}…`
      : uri;
  }
  const host =
    url.host.length > LINK_TARGET_HOST_LENGTH
      ? `…${url.host.slice(-(LINK_TARGET_HOST_LENGTH - 1))}`
      : url.host;
  const origin = `${url.protocol}//${host}`;
  const pathRoom = LINK_TARGET_LABEL_LENGTH - origin.length;
  const path = url.pathname;
  return path.length <= pathRoom
    ? origin + path
    : `${origin}${path.slice(0, Math.max(0, pathRoom - 1))}…`;
}

function writeSystemMessage(terminal: Terminal, message: string): void {
  terminal.write(`\r\n[terminal] ${message}\r\n`);
}

/**
 * Copies with execCommand, which WebKit runs only on a DOM selection; xterm
 * uses this same textarea for its native right-click copy handling. Pending
 * textarea input is restored afterwards.
 */
function copyThroughTextarea(terminal: Terminal, text: string): boolean {
  const textarea = terminal.textarea;
  if (!textarea) return false;
  const { value, selectionStart, selectionEnd, selectionDirection } = textarea;
  try {
    textarea.value = text;
    textarea.select();
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    textarea.value = value;
    textarea.setSelectionRange(selectionStart, selectionEnd, selectionDirection);
  }
}

/**
 * The native menu resolves after the click's user activation may have lapsed, so a missing or
 * rejected clipboard write falls back to execCommand; a final failure prints the link to copy.
 */
async function copyLinkText(
  terminal: Terminal,
  uri: string,
  isCurrent: () => boolean,
): Promise<void> {
  try {
    await navigator.clipboard.writeText(uri);
    return;
  } catch {
    // A closed or retargeted viewport must not steal focus for a stale copy.
    if (!isCurrent() || copyThroughDetachedTextarea(uri)) return;
  }
  writeSystemMessage(terminal, `Couldn't copy the link: ${uri}`);
}

/**
 * execCommand copy from a textarea outside the terminal: xterm's own `copy` listener would
 * replace the payload with its selection if the terminal's textarea were used.
 */
function copyThroughDetachedTextarea(text: string): boolean {
  // Focus may have moved to another pane while the menu or clipboard was pending; give it back.
  const previousFocus = document.activeElement;
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.readOnly = true;
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.append(textarea);
  try {
    textarea.focus();
    textarea.select();
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    textarea.remove();
    if (previousFocus instanceof HTMLElement) previousFocus.focus();
  }
}

function acquireTerminalInputBinding(inputKey: string): {
  readonly binding: TerminalInputBinding;
  readonly scheduler: TerminalInputScheduler;
} {
  let binding = terminalInputBindings.get(inputKey);
  if (!binding) {
    binding = {
      renderer: null,
      write: null,
      ordered: false,
      generation: 0,
      attachment: null,
      error: null,
      resetInput: null,
      pendingFallbacks: [],
    };
    terminalInputBindings.set(inputKey, binding);
  }

  const keyedBinding = binding;
  const scheduler = terminalInputRegistry.acquire(inputKey, () => {
    const rawScheduler = createTerminalInputScheduler({
      maxInFlight: () => (keyedBinding.ordered ? 16 : 1),
      maxFrameBytes: 16 * 1024,
      maxPendingBytes: 1024 * 1024,
      stopOnError: () => keyedBinding.ordered,
      onReset: () => keyedBinding.resetInput?.(),
      send: async (data) => {
        const generation = keyedBinding.generation;
        let remaining = data.length;
        let fallbackError = "Terminal write failed";
        let hasFallback = false;
        const consumedFallbacks = new Set<(typeof keyedBinding.pendingFallbacks)[number]>();
        const notifyFallbackFailures = (error: unknown) => {
          if (error === TERMINAL_WRITE_INTERRUPTED || generation !== keyedBinding.generation)
            return;
          const affectedFallbacks = new Set([
            ...consumedFallbacks,
            ...keyedBinding.pendingFallbacks,
          ]);
          for (const fallback of affectedFallbacks) {
            if (fallback.notified) continue;
            fallback.notified = true;
            try {
              fallback.onWriteError?.(error);
            } catch {
              // Failure observers must not break scheduler cleanup.
            }
          }
        };
        while (remaining > 0 && keyedBinding.pendingFallbacks.length > 0) {
          const pendingFallback = keyedBinding.pendingFallbacks[0]!;
          consumedFallbacks.add(pendingFallback);
          if (!hasFallback) {
            fallbackError = pendingFallback.message;
            hasFallback = true;
          }
          const consumed = Math.min(remaining, pendingFallback.remaining);
          pendingFallback.remaining -= consumed;
          remaining -= consumed;
          if (pendingFallback.remaining === 0) {
            keyedBinding.pendingFallbacks.shift();
          }
        }

        const write = keyedBinding.write;
        if (!write) {
          const error = new Error(fallbackError);
          notifyFallbackFailures(error);
          return { ok: false, error };
        }
        let result: TerminalInputSendResult;
        try {
          result = await write(data, fallbackError);
        } catch (error) {
          notifyFallbackFailures(error);
          throw error;
        }
        if (!result.ok) {
          notifyFallbackFailures(result.error);
        }
        return result;
      },
      onWriteError: (error) => {
        for (const fallback of keyedBinding.pendingFallbacks) {
          if (fallback.notified || error === TERMINAL_WRITE_INTERRUPTED) continue;
          fallback.notified = true;
          try {
            fallback.onWriteError?.(error);
          } catch {
            // Failure observers must not break scheduler cleanup.
          }
        }
        keyedBinding.pendingFallbacks.length = 0;
        if (keyedBinding.ordered) {
          keyedBinding.error =
            error instanceof Error ? error.message : "Terminal input was interrupted.";
          keyedBinding.renderer?.onInputError(keyedBinding.error);
        }
        if (error === TERMINAL_WRITE_INTERRUPTED) return;
        const active = keyedBinding.renderer?.terminal;
        if (!active) return;
        writeSystemMessage(
          active,
          error instanceof Error ? error.message : "Terminal write failed",
        );
      },
    });

    return {
      enqueue: (data) => rawScheduler.enqueue(data),
      reserve: () => rawScheduler.reserve(),
      reset: () => {
        keyedBinding.generation += 1;
        keyedBinding.error = null;
        keyedBinding.renderer?.onInputError(null);
        keyedBinding.pendingFallbacks.length = 0;
        rawScheduler.reset();
      },
      pendingLength: () => rawScheduler.pendingLength(),
      isDraining: () => rawScheduler.isDraining(),
    };
  });

  return { binding: keyedBinding, scheduler };
}

export function fitTerminalSafely(fitAddon: FitAddon): boolean {
  try {
    fitAddon.fit();
    return true;
  } catch {
    return false;
  }
}

export function runtimeEnvSignature(runtimeEnv: Record<string, string> | undefined): string {
  if (!runtimeEnv) return "";
  return JSON.stringify(
    Object.entries(runtimeEnv)
      .filter(([key, value]) => key.length > 0 && typeof value === "string")
      .sort(([leftKey], [rightKey]) => leftKey.localeCompare(rightKey)),
  );
}

export function resolveTerminalDocumentVisibility(
  visibilityState: DocumentVisibilityState,
  desktopUiAutomation: boolean,
): boolean {
  return desktopUiAutomation || visibilityState === "visible";
}

function isDocumentVisible(): boolean {
  return (
    typeof document === "undefined" ||
    resolveTerminalDocumentVisibility(
      document.visibilityState,
      import.meta.env.VITE_BIBCODE_DESKTOP_E2E === "1",
    )
  );
}

export function normalizeComputedColor(value: string | null | undefined, fallback: string): string {
  const normalizedValue = value?.trim().toLowerCase();
  if (
    !normalizedValue ||
    normalizedValue === "transparent" ||
    normalizedValue === "rgba(0, 0, 0, 0)" ||
    normalizedValue === "rgba(0 0 0 / 0)"
  ) {
    return fallback;
  }
  return value ?? fallback;
}

function terminalThemesEqual(left: ITheme, right: ITheme): boolean {
  const keys = new Set([...Object.keys(left), ...Object.keys(right)] as Array<keyof ITheme>);
  for (const key of keys) {
    // `extendedAnsi` is a freshly built array on every theme compose, so an
    // identity check would report a change on each call and rebuild the glyph
    // atlas for nothing. Compare it by value.
    if (key === "extendedAnsi") {
      const leftPalette = left.extendedAnsi;
      const rightPalette = right.extendedAnsi;
      if (leftPalette === rightPalette) continue;
      if (
        leftPalette === undefined ||
        rightPalette === undefined ||
        leftPalette.length !== rightPalette.length ||
        leftPalette.some((color, index) => color !== rightPalette[index])
      ) {
        return false;
      }
      continue;
    }
    if (left[key] !== right[key]) return false;
  }
  return true;
}

function repaintTerminalTheme(
  terminal: Terminal,
  resolvedTheme: TerminalThemeMode,
  mountElement?: HTMLElement | null,
): void {
  const nextTheme = terminalThemeFromApp(resolvedTheme, mountElement);
  const currentTheme = terminal.options.theme;
  if (currentTheme && terminalThemesEqual(currentTheme, nextTheme)) return;
  terminal.options.theme = nextTheme;
  terminal.refresh(0, terminal.rows - 1);
}

function terminalThemeFromApp(
  resolvedTheme: TerminalThemeMode,
  mountElement?: HTMLElement | null,
): ITheme {
  const isDark = resolvedTheme === "dark";
  const fallbackBackground = isDark ? "rgb(14, 18, 24)" : "rgb(255, 255, 255)";
  const fallbackForeground = isDark ? "rgb(237, 241, 247)" : "rgb(28, 33, 41)";
  const panelSurface =
    mountElement?.closest(".thread-terminal-panel") ??
    document.querySelector(".thread-terminal-panel") ??
    document.body;
  const panelStyles = getComputedStyle(panelSurface);
  const bodyStyles = getComputedStyle(document.body);
  const requestedThemeMatchesDocument =
    document.documentElement.classList.contains("dark") === isDark;
  const background = requestedThemeMatchesDocument
    ? normalizeComputedColor(
        panelStyles.backgroundColor,
        normalizeComputedColor(bodyStyles.backgroundColor, fallbackBackground),
      )
    : fallbackBackground;
  const foreground = requestedThemeMatchesDocument
    ? normalizeComputedColor(
        panelStyles.color,
        normalizeComputedColor(bodyStyles.color, fallbackForeground),
      )
    : fallbackForeground;

  if (isDark) {
    return {
      background,
      foreground,
      extendedAnsi: terminalExtendedAnsiPalette("dark"),
      cursor: "rgb(180, 203, 255)",
      selectionBackground: "rgba(180, 203, 255, 0.25)",
      scrollbarSliderBackground: "rgba(255, 255, 255, 0.1)",
      scrollbarSliderHoverBackground: "rgba(255, 255, 255, 0.18)",
      scrollbarSliderActiveBackground: "rgba(255, 255, 255, 0.22)",
      black: "rgb(24, 30, 38)",
      red: "rgb(255, 122, 142)",
      green: "rgb(134, 231, 149)",
      yellow: "rgb(244, 205, 114)",
      blue: "rgb(137, 190, 255)",
      magenta: "rgb(208, 176, 255)",
      cyan: "rgb(124, 232, 237)",
      white: "rgb(210, 218, 230)",
      brightBlack: "rgb(110, 120, 136)",
      brightRed: "rgb(255, 168, 180)",
      brightGreen: "rgb(176, 245, 186)",
      brightYellow: "rgb(255, 224, 149)",
      brightBlue: "rgb(174, 210, 255)",
      brightMagenta: "rgb(229, 203, 255)",
      brightCyan: "rgb(167, 244, 247)",
      brightWhite: "rgb(244, 247, 252)",
    };
  }

  return {
    background,
    foreground,
    extendedAnsi: terminalExtendedAnsiPalette("light"),
    cursor: "rgb(38, 56, 78)",
    selectionBackground: "rgba(37, 63, 99, 0.2)",
    scrollbarSliderBackground: "rgba(0, 0, 0, 0.15)",
    scrollbarSliderHoverBackground: "rgba(0, 0, 0, 0.25)",
    scrollbarSliderActiveBackground: "rgba(0, 0, 0, 0.3)",
    black: "rgb(44, 53, 66)",
    red: "rgb(191, 70, 87)",
    green: "rgb(60, 126, 86)",
    yellow: "rgb(146, 112, 35)",
    blue: "rgb(72, 102, 163)",
    magenta: "rgb(132, 86, 149)",
    cyan: "rgb(53, 127, 141)",
    white: "rgb(210, 215, 223)",
    brightBlack: "rgb(112, 123, 140)",
    brightRed: "rgb(212, 95, 112)",
    brightGreen: "rgb(85, 148, 111)",
    brightYellow: "rgb(173, 133, 45)",
    brightBlue: "rgb(91, 124, 194)",
    brightMagenta: "rgb(153, 107, 172)",
    brightCyan: "rgb(70, 149, 164)",
    brightWhite: "rgb(236, 240, 246)",
  };
}

export function getTerminalSelectionRect(mountElement: HTMLElement): DOMRect | null {
  const selection = window.getSelection();
  if (!selection || selection.rangeCount === 0 || selection.isCollapsed) {
    return null;
  }

  const range = selection.getRangeAt(0);
  const commonAncestor = range.commonAncestorContainer;
  const selectionRoot =
    commonAncestor instanceof Element ? commonAncestor : commonAncestor.parentElement;
  if (!(selectionRoot instanceof Element) || !mountElement.contains(selectionRoot)) {
    return null;
  }

  const rects = Array.from(range.getClientRects()).filter(
    (rect) => rect.width > 0 || rect.height > 0,
  );
  if (rects.length > 0) {
    return rects[rects.length - 1] ?? null;
  }

  const boundingRect = range.getBoundingClientRect();
  return boundingRect.width > 0 || boundingRect.height > 0 ? boundingRect : null;
}

export function resolveTerminalSelectionActionPosition(options: {
  bounds: { left: number; top: number; width: number; height: number };
  selectionRect: { right: number; bottom: number } | null;
  pointer: { x: number; y: number } | null;
  viewport?: { width: number; height: number } | null;
}): { x: number; y: number } {
  const { bounds, selectionRect, pointer, viewport } = options;
  const viewportWidth =
    viewport?.width ??
    (typeof window === "undefined" ? bounds.left + bounds.width + 8 : window.innerWidth);
  const viewportHeight =
    viewport?.height ??
    (typeof window === "undefined" ? bounds.top + bounds.height + 8 : window.innerHeight);
  const panelLeft = Math.round(bounds.left);
  const panelTop = Math.round(bounds.top);
  const panelRight = Math.round(bounds.left + bounds.width);
  const panelBottom = Math.round(bounds.top + bounds.height);
  const preferredX =
    selectionRect !== null
      ? Math.round(selectionRect.right)
      : pointer === null
        ? Math.round(bounds.left + bounds.width - 140)
        : Math.max(panelLeft, Math.min(Math.round(pointer.x), panelRight));
  const preferredY =
    selectionRect !== null
      ? Math.round(selectionRect.bottom + 4)
      : pointer === null
        ? Math.round(bounds.top + 12)
        : Math.max(panelTop, Math.min(Math.round(pointer.y), panelBottom));
  return {
    x: Math.max(8, Math.min(preferredX, Math.max(viewportWidth - 8, 8))),
    y: Math.max(8, Math.min(preferredY, Math.max(viewportHeight - 8, 8))),
  };
}

interface TerminalViewportProps {
  threadRef: ScopedThreadRef;
  threadId: ThreadId;
  projectId: ProjectId;
  terminalId: string;
  terminalLabel: string;
  cwd: string;
  worktreePath?: string | null;
  runtimeEnv?: Record<string, string>;
  command?: TerminalLaunchCommand;
  /** Marks the session as a host thread's center panel so other clients adopt it. */
  centerPanel?: boolean;
  visible: boolean;
  onSessionExited: () => void;
  onAddTerminalContext: (selection: TerminalContextSelection) => void;
  focusRequestId: number;
  autoFocus: boolean;
  resizeEpoch: number;
  keybindings: ResolvedKeybindingsConfig;
  workspaceUnavailable?: string | null;
}

interface TerminalLaunchLocation {
  readonly cwd: string;
  readonly worktreePath?: string | null;
  readonly runtimeEnv?: Record<string, string>;
}

interface CodexThemeRestartRequest {
  readonly id: number;
  readonly sourceGeneration: number;
  readonly targetTheme: TerminalThemeMode;
  readonly phase: "submitting" | "awaiting-generation";
}

interface CodexThemeRestartError {
  readonly generation: number;
  readonly targetTheme: TerminalThemeMode;
  readonly message: string;
}

export function TerminalViewport({
  threadRef,
  threadId,
  projectId,
  terminalId,
  terminalLabel,
  cwd,
  worktreePath,
  runtimeEnv,
  command,
  centerPanel = false,
  visible,
  onSessionExited,
  onAddTerminalContext,
  focusRequestId,
  autoFocus,
  resizeEpoch,
  keybindings,
  workspaceUnavailable = null,
}: TerminalViewportProps) {
  const { resolvedTheme: appTheme } = useTheme();
  const terminalThemePreference = usePrimarySettings(
    (settings) => settings.terminalThemePreference,
  );
  // The terminal surface owns its theme. Provider TUIs paint their own dark
  // panels with hardcoded 256-colour indices and never ask the terminal what
  // colours it uses, so a light terminal renders those panels as dark blocks.
  // Everything downstream — the xterm theme and the OSC colours the PTY reports
  // to providers — is derived from this, never from the app theme directly.
  const resolvedTheme = resolveTerminalThemeMode(terminalThemePreference, appTheme);
  const containerRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const environmentId = threadRef.environmentId;
  const serverConfig = useAtomValue(serverEnvironment.configValueAtom(environmentId));
  const orderedInput = serverConfig?.environment.capabilities?.terminalOrderedInput === true;
  const sizeOwnership = serverConfig?.environment.capabilities?.terminalSizeOwnership === true;
  const terminalTargetKey = terminalInputKey(environmentId, threadId, terminalId);
  const [rendererAttachmentInput, setRendererAttachmentInput] = useState<{
    readonly target: string;
    readonly sizeClaim: string;
    readonly sizeOwnership: boolean;
    readonly dimensions: TerminalDimensions | null;
  } | null>(null);
  const readyRenderer =
    rendererAttachmentInput?.target === terminalTargetKey &&
    rendererAttachmentInput.sizeOwnership === sizeOwnership
      ? rendererAttachmentInput
      : null;
  const [mirroringSize, setMirroringSize] = useState(false);
  const rendererRef = useRef<{
    readonly sizeClaim: string;
    dismissSizeNotice(): void;
    render(signal: TerminalRenderSignal, claimOnAttach: boolean): void;
    size(trigger: TerminalSizeTrigger): void;
  } | null>(null);
  const [inputError, setInputError] = useState<string | null>(null);
  const [inputReattaching, setInputReattaching] = useState(false);
  const [imagePastesPending, setImagePastesPending] = useState(0);
  const cancelImagePastesRef = useRef<() => void>(() => undefined);
  const openInPreferredEditor = useOpenInPreferredEditor(
    environmentId,
    serverConfig?.availableEditors ?? [],
  );
  const openTerminalPath = useEffectEvent((target: string) => openInPreferredEditor(target));
  const readWorkspaceUnavailable = useEffectEvent(() => workspaceUnavailable);
  const readHasThread = useEffectEvent(() => threadRef.threadId.length > 0);
  const openPreview = useAtomCommand(previewEnvironment.open, {
    reportFailure: false,
  });
  const createAssetUrl = useAtomQueryRunner(assetEnvironment.createUrl, {
    reportFailure: false,
  });
  const openPreviewFile = useEffectEvent(async (absolutePath: string) => {
    const connection = readPreparedConnection(environmentId);
    if (!connection) return { ok: false as const, outside: false };
    const result = await openFileInPreview({
      threadRef,
      filePath: absolutePath,
      httpBaseUrl: connection.httpBaseUrl,
      createAssetUrl,
      openPreview,
    });
    if (result._tag === "Success" || isAtomCommandInterrupted(result)) return { ok: true as const };
    const error = squashAtomCommandFailure(result) as { readonly _tag?: string } | undefined;
    return { ok: false as const, outside: error?._tag === "AssetWorkspacePathValidationError" };
  });
  const routeTerminalUrl = useEffectEvent((url: string, invert: boolean) => {
    const outcome = openLink({
      url,
      threadRef: threadRef.threadId.length > 0 ? threadRef : null,
      environmentId,
      invert,
      openPreview,
      onError: (cause) => {
        const t = terminalRef.current;
        if (t)
          writeSystemMessage(t, cause instanceof Error ? cause.message : "Unable to open link");
      },
    });
    if (outcome !== "unavailable") return;
    const t = terminalRef.current;
    if (t) writeSystemMessage(t, "Opening links is unavailable in this browser.");
  });
  const openEditorPath = useEffectEvent((target: string) => {
    void (async () => {
      const result = await openTerminalPath(target);
      if (result._tag === "Success" || isAtomCommandInterrupted(result)) return;
      const error = squashAtomCommandFailure(result);
      const t = terminalRef.current;
      if (t) writeSystemMessage(t, error instanceof Error ? error.message : "Unable to open path");
    })();
  });
  const runTerminalWrite = useAtomCommand(terminalEnvironment.write, {
    reportFailure: false,
  });
  const runPrepareInput = useAtomCommand(terminalEnvironment.prepareInput, {
    reportFailure: false,
  });
  const runResetInput = useAtomCommand(terminalEnvironment.resetInput, { reportFailure: false });
  const runTerminalResize = useAtomCommand(terminalEnvironment.resize, {
    reportFailure: false,
  });
  const runTerminalRestart = useAtomCommand(terminalEnvironment.restart, {
    reportFailure: false,
  });
  const runStageImagePaste = useAtomCommand(terminalEnvironment.stageImagePaste, {
    reportFailure: false,
  });
  const stageImagePaste = useEffectEvent(
    (image: File, mimeType: TerminalImagePasteMimeType, signal: AbortSignal) =>
      serverConfig?.environment.capabilities?.terminalImagePaste === true
        ? runStageImagePaste(
            {
              environmentId,
              input: { file: image, name: image.name || "pasted-image", mimeType },
            },
            { signal },
          )
        : null,
  );
  const hasHandledExitRef = useRef(false);
  const selectionPointerRef = useRef<{ x: number; y: number } | null>(null);
  const selectionActionRequestIdRef = useRef(0);
  const selectionActionOpenRef = useRef(false);
  const keybindingsRef = useRef(keybindings);
  const inputSchedulerRef = useRef<TerminalInputScheduler | null>(null);
  const focusGenerationRef = useRef(0);
  const fulfilledFocusRequestIdRef = useRef<number | null>(null);
  const resizeRendererGenerationRef = useRef(0);
  const lastRequestedSizeRef = useRef<{
    readonly generation: number;
    readonly dimensions: TerminalDimensions;
  } | null>(null);
  const webglGenerationRef = useRef(0);
  const webglLifecycleRef = useRef<WebglLifecycle | null>(null);
  const webglFailedRef = useRef(false);
  const webglDiagnosticRecordedRef = useRef(false);
  // Terminal creation precedes attach. The committed renderer epoch lets the
  // transcript and WebGL effects attach once to the same current instance.
  const rendererEpochRef = useRef(0);
  const [rendererEpoch, setRendererEpoch] = useState(0);
  const [documentVisible, setDocumentVisible] = useState(isDocumentVisible);
  const shouldRender = visible && isDocumentVisible() && documentVisible;
  const enableTerminalAgentActivity = useEnvironmentSettings(
    environmentId,
    (settings) => settings.enableTerminalAgentActivity,
  );
  const webglEnabled = usePrimarySettings((settings) => settings.terminal.webglEnabled);
  const terminalFontPreference = usePrimarySettings((settings) => settings.terminalFontPreference);
  const terminalFontFamily = useMemo(
    () => resolveTerminalFontFamily(terminalFontPreference),
    [terminalFontPreference],
  );
  const readTerminalFontFamily = useEffectEvent(() => terminalFontFamily);
  const runtimeEnvKey = useMemo(() => runtimeEnvSignature(runtimeEnv), [runtimeEnv]);
  const recordWebglDiagnosticOnce = useCallback(() => {
    if (webglDiagnosticRecordedRef.current) return;
    webglDiagnosticRecordedRef.current = true;
    console.warn("[terminal] WebGL renderer unavailable; using the standard renderer.");
  }, []);
  const handleSessionExited = useEffectEvent(() => {
    onSessionExited();
  });
  const handleAddTerminalContext = useEffectEvent((selection: TerminalContextSelection) => {
    onAddTerminalContext(selection);
  });
  const readTerminalLabel = useEffectEvent(() => terminalLabel);
  const hasAuthoritativeHostConfig = serverConfig !== null;
  const hasPersistentWindowsConsoleTheme = usesPersistentWindowsConsoleTheme(command);
  const attachmentLaunchThemeRef = useRef<{
    readonly targetKey: string;
    readonly theme: TerminalThemeMode;
    readonly pinned: boolean;
  } | null>(null);
  const canAttachTerminal = command === undefined || hasAuthoritativeHostConfig;
  const shouldPinAttachmentTheme = shouldRender && canAttachTerminal;
  if (attachmentLaunchThemeRef.current?.targetKey !== terminalTargetKey) {
    attachmentLaunchThemeRef.current = {
      targetKey: terminalTargetKey,
      theme: resolvedTheme,
      pinned: shouldPinAttachmentTheme,
    };
  } else if (!attachmentLaunchThemeRef.current.pinned) {
    attachmentLaunchThemeRef.current = {
      targetKey: terminalTargetKey,
      theme: resolvedTheme,
      pinned: shouldPinAttachmentTheme,
    };
  }
  const attachmentLaunchTheme = attachmentLaunchThemeRef.current.theme;
  // Launch-command env supplies provider defaults and the thread's runtime env
  // wins ordinary conflicts. Theme colors and the Codex-only Windows console
  // theme are authoritative because the PTY consumes them during spawn.
  const spawnEnv = useMemo(() => {
    const merged = mergeTerminalSpawnEnv({
      commandEnv: command?.env,
      runtimeEnv,
      resolvedTheme: attachmentLaunchTheme,
      windowsConsoleTheme: hasPersistentWindowsConsoleTheme,
    });
    return Object.keys(merged).length > 0 ? merged : undefined;
  }, [attachmentLaunchTheme, command, hasPersistentWindowsConsoleTheme, runtimeEnv]);
  const terminalSession = useAttachedTerminalSession({
    environmentId,
    terminal: {
      threadId,
      terminalId,
      cwd,
      ...readyRenderer?.dimensions,
      ...(sizeOwnership && readyRenderer ? { sizeClaim: readyRenderer.sizeClaim } : {}),
      ...(worktreePath !== undefined ? { worktreePath } : {}),
      ...(spawnEnv ? { env: spawnEnv } : {}),
      ...(command ? { command } : {}),
      ...(centerPanel ? { centerPanel } : {}),
    },
    attach: shouldRender && canAttachTerminal && readyRenderer !== null,
  });
  const terminalError = terminalSession.error;
  const terminalStatus = terminalSession.status;
  const terminalGeneration = terminalSession.generation;
  const readTerminalGeneration = useEffectEvent(() => terminalGeneration);
  const readServerOs = useEffectEvent(() => serverConfig?.environment.platform.os);
  const transcriptRuntime = terminalSession.transcriptRuntime;
  const themeRestartRequestIdRef = useRef(0);
  const themeRestartRequestRef = useRef<CodexThemeRestartRequest | null>(null);
  const [themeRestartRequest, setThemeRestartRequest] = useState<CodexThemeRestartRequest | null>(
    null,
  );
  const [themeRestartError, setThemeRestartError] = useState<CodexThemeRestartError | null>(null);
  const launchThemeRef = useRef<TerminalLaunchThemeState | null>(null);
  const activeThemeRestartRequest = themeRestartRequestRef.current;
  launchThemeRef.current = retainTerminalLaunchTheme(launchThemeRef.current, {
    persistentConsoleTheme: hasPersistentWindowsConsoleTheme,
    generation: terminalGeneration,
    resolvedTheme: hasPersistentWindowsConsoleTheme ? attachmentLaunchTheme : resolvedTheme,
    authoritativeTheme: terminalSession.consoleTheme,
    restartRequest: activeThemeRestartRequest,
  });
  if (
    hasPersistentWindowsConsoleTheme &&
    activeThemeRestartRequest !== null &&
    terminalGeneration > activeThemeRestartRequest.sourceGeneration &&
    attachmentLaunchThemeRef.current.targetKey === terminalTargetKey &&
    attachmentLaunchThemeRef.current.theme !== activeThemeRestartRequest.targetTheme
  ) {
    attachmentLaunchThemeRef.current = {
      ...attachmentLaunchThemeRef.current,
      theme: activeThemeRestartRequest.targetTheme,
      pinned: true,
    };
  }
  const effectiveTerminalTheme = hasPersistentWindowsConsoleTheme
    ? launchThemeRef.current.theme
    : resolvedTheme;
  const effectiveTerminalThemeRef = useRef(effectiveTerminalTheme);
  effectiveTerminalThemeRef.current = effectiveTerminalTheme;
  const nativeThemeMismatch =
    hasPersistentWindowsConsoleTheme &&
    terminalStatus === "running" &&
    effectiveTerminalTheme !== resolvedTheme;
  const [dismissedThemeNotice, setDismissedThemeNotice] = useState<{
    readonly generation: number;
    readonly targetTheme: TerminalThemeMode;
  } | null>(null);
  const showCodexThemeNotice =
    nativeThemeMismatch &&
    (dismissedThemeNotice === null ||
      dismissedThemeNotice.generation !== terminalGeneration ||
      dismissedThemeNotice.targetTheme !== resolvedTheme);
  const themeRestartPending = themeRestartRequest !== null;
  const visibleThemeRestartError =
    themeRestartError !== null &&
    themeRestartError.generation === terminalGeneration &&
    themeRestartError.targetTheme === resolvedTheme
      ? themeRestartError.message
      : null;
  const hasAuthoritativeTerminalState = transcriptRuntime !== null;
  const previousMetadataRef = useRef({
    error: null as string | null,
    status: "closed" as typeof terminalStatus,
  });

  const restartForTheme = useCallback(() => {
    const terminal = terminalRef.current;
    if (
      workspaceUnavailable ||
      !terminal ||
      !hasPersistentWindowsConsoleTheme ||
      themeRestartRequestRef.current !== null
    )
      return;

    const targetSpawnEnv = mergeTerminalSpawnEnv({
      commandEnv: command?.env,
      runtimeEnv,
      resolvedTheme,
      windowsConsoleTheme: true,
    });

    const request: CodexThemeRestartRequest = {
      id: themeRestartRequestIdRef.current + 1,
      sourceGeneration: terminalGeneration,
      targetTheme: resolvedTheme,
      phase: "submitting",
    };
    themeRestartRequestIdRef.current = request.id;
    themeRestartRequestRef.current = request;
    setThemeRestartRequest(request);
    setThemeRestartError(null);
    void (async () => {
      try {
        const result = await runTerminalRestart({
          environmentId,
          input: {
            threadId,
            terminalId,
            cwd,
            ...(worktreePath !== undefined ? { worktreePath } : {}),
            cols: terminal.cols,
            rows: terminal.rows,
            ...(Object.keys(targetSpawnEnv).length > 0 ? { env: targetSpawnEnv } : {}),
            ...(command ? { command } : {}),
          },
        });
        if (themeRestartRequestRef.current?.id !== request.id) return;
        if (result._tag === "Success") {
          const awaitingGeneration: CodexThemeRestartRequest = {
            ...request,
            phase: "awaiting-generation",
          };
          themeRestartRequestRef.current = awaitingGeneration;
          setThemeRestartRequest((current) =>
            current?.id === request.id ? awaitingGeneration : current,
          );
          return;
        }

        themeRestartRequestRef.current = null;
        setThemeRestartRequest((current) => (current?.id === request.id ? null : current));
        if (!isAtomCommandInterrupted(result)) {
          const error = squashAtomCommandFailure(result);
          setThemeRestartError({
            generation: request.sourceGeneration,
            targetTheme: request.targetTheme,
            message: error instanceof Error ? error.message : "Unable to restart terminal",
          });
        }
      } catch (cause: unknown) {
        if (themeRestartRequestRef.current?.id !== request.id) return;
        themeRestartRequestRef.current = null;
        setThemeRestartRequest((current) => (current?.id === request.id ? null : current));
        setThemeRestartError({
          generation: request.sourceGeneration,
          targetTheme: request.targetTheme,
          message: cause instanceof Error ? cause.message : "Unable to restart terminal",
        });
      }
    })();
  }, [
    command,
    cwd,
    environmentId,
    hasPersistentWindowsConsoleTheme,
    runTerminalRestart,
    resolvedTheme,
    runtimeEnv,
    terminalGeneration,
    terminalId,
    threadId,
    worktreePath,
    workspaceUnavailable,
  ]);

  useEffect(() => {
    const request = themeRestartRequestRef.current;
    if (request !== null && terminalGeneration > request.sourceGeneration) {
      themeRestartRequestRef.current = null;
      setThemeRestartRequest((current) => (current?.id === request.id ? null : current));
    }
    setThemeRestartError((current) =>
      current !== null && current.generation !== terminalGeneration ? null : current,
    );
  }, [terminalGeneration]);

  useEffect(() => {
    if (!visible) return;

    const updateDocumentVisibility = () => {
      setDocumentVisible(isDocumentVisible());
    };
    updateDocumentVisibility();
    document.addEventListener("visibilitychange", updateDocumentVisibility);
    return () => {
      document.removeEventListener("visibilitychange", updateDocumentVisibility);
    };
  }, [visible]);

  useEffect(() => {
    keybindingsRef.current = keybindings;
  }, [keybindings]);

  useEffect(() => {
    const mount = containerRef.current;
    if (!mount || !shouldRender || !canAttachTerminal) return;

    const localApi = readLocalApi();
    // Mouse-tracking gestures flip the force-selection modifier (Shift off macOS) on the
    // events xterm later activates links with; Shift-click routing needs the physical key.
    const physicalShiftKeys = new WeakMap<MouseEvent, boolean>();
    const physicalShiftKey = (event: MouseEvent) => physicalShiftKeys.get(event) ?? event.shiftKey;
    let linkConfirmOpen = false;

    const fitAddon = new FitAddon();
    const terminal = new Terminal({
      cursorBlink: true,
      macOptionClickForcesSelection: true,
      lineHeight: 1,
      fontSize: 12,
      scrollback: 5_000,
      fontFamily: readTerminalFontFamily(),
      theme: terminalThemeFromApp(effectiveTerminalThemeRef.current, mount),
      // OSC 8 activations bypass the link provider, so apply the same activation guard here.
      linkHandler: {
        activate: (event, uri, range) => {
          if (!isTerminalLinkActivation(event)) return;
          const activeTerminal = terminalRef.current;
          if (!activeTerminal) return;
          const invert = physicalShiftKey(event);
          const getLine = (index: number) => activeTerminal.buffer.active.getLine(index);
          if (terminalLinkLabelShowsUri(range, uri, getLine)) {
            routeTerminalUrl(uri, invert);
            return;
          }
          // OSC 8 text can disguise its target, so confirm it, as xterm's default handler did.
          if (!localApi) {
            writeSystemMessage(activeTerminal, "Opening links is unavailable in this browser.");
            return;
          }
          // Rapid Ctrl-clicks must not stack confirmation menus.
          if (linkConfirmOpen) return;
          linkConfirmOpen = true;
          void localApi.contextMenu
            .show(
              [
                { id: "open", label: `Open ${describeLinkTarget(uri)}` },
                { id: "copy", label: "Copy link" },
              ],
              { x: event.clientX, y: event.clientY },
            )
            .then((choice) => {
              // A retargeted or closed viewport must not open this link against its new thread.
              if (terminalRef.current !== activeTerminal) return;
              if (choice === "open") routeTerminalUrl(uri, invert);
              if (choice === "copy") {
                void copyLinkText(
                  activeTerminal,
                  uri,
                  () => terminalRef.current === activeTerminal,
                );
              }
            })
            .catch(() => {
              if (terminalRef.current === activeTerminal) {
                writeSystemMessage(activeTerminal, "Unable to open link");
              }
            })
            .finally(() => {
              linkConfirmOpen = false;
            });
        },
        allowNonHttpProtocols: false,
      },
    });
    terminal.loadAddon(fitAddon);
    terminal.open(mount);
    fitTerminalSafely(fitAddon);
    const sizeClaim = randomUUID();
    let fittedDimensions = proposeTerminalDimensions(fitAddon);
    const updateFittedDimensions = () => (fittedDimensions = proposeTerminalDimensions(fitAddon));
    setRendererAttachmentInput({
      target: terminalTargetKey,
      sizeClaim,
      sizeOwnership,
      dimensions: fittedDimensions,
    });
    let mirroringSizeVisible = false;
    const updateMirroringSize = (next: boolean) => {
      if (mirroringSizeVisible === next) return;
      mirroringSizeVisible = next;
      setMirroringSize(next);
    };
    let appliedSize: TerminalSize | null = null;
    let claimInFlight: TerminalSize | null = null;
    let layoutChangedWhileClaimPending = false;
    let claimNoticeSuppressed = false;
    let oscColorResponderActive = false;
    let disposed = false;
    const resizeTerminal = (size: Pick<TerminalResizeInput, "cols" | "rows" | "sizeClaim">) =>
      runTerminalResize({ environmentId, input: { threadId, terminalId, ...size } });
    const requestTerminalResize = (rendererGeneration: number, dimensions: TerminalDimensions) => {
      if (resizeRendererGenerationRef.current !== rendererGeneration) return;
      const current = lastRequestedSizeRef.current;
      if (
        current?.generation === rendererGeneration &&
        terminalDimensionsEqual(current.dimensions, dimensions)
      )
        return;
      const request = { generation: rendererGeneration, dimensions };
      lastRequestedSizeRef.current = request;
      void resizeTerminal(dimensions).then((result) => {
        if (
          result._tag === "Failure" &&
          resizeRendererGenerationRef.current === rendererGeneration &&
          lastRequestedSizeRef.current === request
        ) {
          lastRequestedSizeRef.current = null;
        }
      });
    };

    const resizeRendererGeneration = resizeRendererGenerationRef.current + 1;
    resizeRendererGenerationRef.current = resizeRendererGeneration;
    lastRequestedSizeRef.current = null;
    terminalRef.current = terminal;
    fitAddonRef.current = fitAddon;
    rendererEpochRef.current += 1;
    setRendererEpoch(rendererEpochRef.current);
    const rendererOwner = {};
    const { binding: inputBinding, scheduler: inputScheduler } =
      acquireTerminalInputBinding(terminalTargetKey);
    inputSchedulerRef.current = inputScheduler;
    inputBinding.renderer = { owner: rendererOwner, terminal, onInputError: setInputError };
    const unregisterSizeReader = registerTerminalSizeReader(
      terminalTargetKey,
      updateFittedDimensions,
    );
    inputBinding.ordered = orderedInput;
    inputBinding.resetInput = () => {
      void runResetInput({ environmentId, input: { threadId, terminalId } });
    };
    setInputError(inputBinding.error);
    inputBinding.write = createTerminalInputWriter({
      environmentId,
      threadId,
      terminalId,
      run: runTerminalWrite,
    });
    const createOutputSink = () =>
      createTerminalOutputSink({
        write: (data, onParsed) => terminal.write(data, onParsed),
      });
    let outputSink = createOutputSink();
    const replyGuard = installTerminalReplyGuard(
      terminal,
      () => !sizeOwnership || !hasForeignTerminalSizeOwner(appliedSize, sizeClaim),
      () => oscColorResponderActive,
    );
    const updateSizeNotice = (fitted = updateFittedDimensions()) => {
      updateMirroringSize(
        !dismissedTerminalSizes.has(terminalTargetKey) &&
          !claimNoticeSuppressed &&
          sizeOwnership &&
          shouldShowTerminalSizeNotice({
            sizeClaim,
            applied: appliedSize,
            fitted,
            claimInFlight: claimInFlight !== null,
          }),
      );
    };
    const handleSizeTrigger = (trigger: TerminalSizeTrigger) => {
      if (disposed || resizeRendererGenerationRef.current !== resizeRendererGeneration) return;
      if (!sizeOwnership) {
        if (trigger !== "layout") return;
        const wasAtBottom = terminal.buffer.active.viewportY >= terminal.buffer.active.baseY;
        fitTerminalSafely(fitAddon);
        if (wasAtBottom) terminal.scrollToBottom();
        requestTerminalResize(resizeRendererGeneration, {
          cols: terminal.cols,
          rows: terminal.rows,
        });
        return;
      }
      const shouldClaim = (fitted: TerminalDimensions | null) =>
        shouldClaimTerminalSize({
          sizeClaim,
          applied: appliedSize,
          fitted,
          trigger,
          documentFocused: document.hasFocus(),
          claimInFlight: claimInFlight !== null,
        });
      // Use the same policy with cached geometry before measuring on keydown.
      if (trigger === "keypress" && fittedDimensions !== null && !shouldClaim(fittedDimensions))
        return;
      const fitted = updateFittedDimensions();
      if (
        trigger === "layout" &&
        claimInFlight !== null &&
        fitted !== null &&
        !terminalDimensionsEqual(claimInFlight, fitted)
      ) {
        layoutChangedWhileClaimPending = true;
      }
      if (fitted && shouldClaim(fitted)) {
        const request = { ...fitted, sizeClaim };
        claimInFlight = request;
        claimNoticeSuppressed = true;
        const finishClaim = (succeeded: boolean) => {
          if (
            disposed ||
            resizeRendererGenerationRef.current !== resizeRendererGeneration ||
            claimInFlight !== request
          )
            return;
          claimInFlight = null;
          if (!succeeded) claimNoticeSuppressed = false;
          const refit = layoutChangedWhileClaimPending;
          layoutChangedWhileClaimPending = false;
          if (refit) handleSizeTrigger("layout");
          else updateSizeNotice();
        };
        void resizeTerminal(request).then(
          (result) => finishClaim(result._tag === "Success"),
          () => finishClaim(false),
        );
      }
      updateSizeNotice(fitted);
    };
    const applySize = (size: TerminalSize | undefined, claimOnAttach = false) => {
      if (disposed) return;
      if (!terminalSizesEqual(appliedSize, size)) claimNoticeSuppressed = false;
      const dismissed = dismissedTerminalSizes.get(terminalTargetKey);
      if (size && dismissed && !terminalSizesEqual(dismissed, size))
        dismissedTerminalSizes.delete(terminalTargetKey);
      appliedSize = size ?? null;
      if (size && !terminalDimensionsEqual(terminal, size)) {
        const wasAtBottom = terminal.buffer.active.viewportY >= terminal.buffer.active.baseY;
        terminal.resize(size.cols, size.rows);
        if (wasAtBottom) terminal.scrollToBottom();
      }
      if (size?.sizeClaim === null) handleSizeTrigger("attach");
      else if (claimOnAttach)
        handleSizeTrigger(size?.sizeClaim === sizeClaim ? "layout" : "attach");
      else if (size?.sizeClaim === sizeClaim) handleSizeTrigger("layout");
      else updateSizeNotice();
    };
    const render = (signal: TerminalRenderSignal, claimOnAttach: boolean) => {
      if (disposed) return;
      if (signal.type === "delta") {
        outputSink.push(signal.data);
        return;
      }
      if (signal.type === "resized") {
        if (sizeOwnership) outputSink.barrier(() => applySize(signal.size));
        return;
      }

      // Reset is authoritative. Drop any old-generation delta still queued for
      // a frame before replacing xterm's display with the bounded snapshot.
      outputSink.dispose();
      outputSink = createOutputSink();
      outputSink.barrier(() => {
        // A new attachment/process generation supersedes its old claim request.
        // Its late RPC completion is ignored by the request-identity guard.
        if (claimOnAttach) {
          claimInFlight = null;
          layoutChangedWhileClaimPending = false;
          claimNoticeSuppressed = false;
        }
        oscColorResponderActive = signal.oscColorResponderActive ?? false;
        if (sizeOwnership) applySize(signal.size, claimOnAttach);
        else {
          lastRequestedSizeRef.current = null;
          handleSizeTrigger("layout");
        }
      });
      replyGuard.replay(
        signal.snapshot,
        sizeOwnership &&
          signal.firstAttachmentGrant === true &&
          signal.size?.sizeClaim === sizeClaim,
      );
      terminal.clearSelection();
    };
    const renderer = {
      sizeClaim,
      render,
      size: handleSizeTrigger,
      dismissSizeNotice() {
        if (appliedSize) dismissedTerminalSizes.set(terminalTargetKey, appliedSize);
        updateSizeNotice();
      },
    };
    rendererRef.current = renderer;

    const clearSelectionAction = () => {
      selectionActionRequestIdRef.current += 1;
    };

    const readSelectionAction = (): {
      position: { x: number; y: number };
      selection: TerminalContextSelection;
    } | null => {
      const activeTerminal = terminalRef.current;
      const mountElement = containerRef.current;
      if (!activeTerminal || !mountElement || !activeTerminal.hasSelection()) {
        return null;
      }
      const selectionText = activeTerminal.getSelection();
      const selectionPosition = activeTerminal.getSelectionPosition();
      const normalizedText = selectionText.replace(/\r\n/g, "\n").replace(/^\n+|\n+$/g, "");
      if (!selectionPosition || normalizedText.length === 0) {
        return null;
      }
      const lineStart = selectionPosition.start.y + 1;
      const lineCount = normalizedText.split("\n").length;
      const lineEnd = Math.max(lineStart, lineStart + lineCount - 1);
      const bounds = mountElement.getBoundingClientRect();
      const selectionRect = getTerminalSelectionRect(mountElement);
      const position = resolveTerminalSelectionActionPosition({
        bounds,
        selectionRect:
          selectionRect === null
            ? null
            : { right: selectionRect.right, bottom: selectionRect.bottom },
        pointer: selectionPointerRef.current,
      });
      return {
        position,
        selection: {
          terminalId,
          terminalLabel: readTerminalLabel(),
          lineStart,
          lineEnd,
          text: normalizedText,
        },
      };
    };

    // The native menu resolves after the click's user activation may have
    // lapsed, so a rejected clipboard write falls back to execCommand.
    const copySelectionText = async (text: string, isCurrent: () => boolean) => {
      try {
        await navigator.clipboard.writeText(text);
        return;
      } catch {
        if (!isCurrent() || copyThroughTextarea(terminal, text)) return;
      }
      writeSystemMessage(
        terminal,
        `Copy failed. Try ${isMacPlatform(navigator.platform) ? "Cmd" : "Ctrl"}+C.`,
      );
    };

    const showSelectionAction = async () => {
      if (!localApi) {
        clearSelectionAction();
        return;
      }
      if (selectionActionOpenRef.current) {
        return;
      }
      const nextAction = readSelectionAction();
      if (!nextAction) {
        clearSelectionAction();
        return;
      }
      // Copy matches keyboard copy; only Add to chat trims the selection.
      const copyText = terminalRef.current?.getSelection() ?? "";
      const requestId = ++selectionActionRequestIdRef.current;
      selectionActionOpenRef.current = true;
      try {
        const clicked = await localApi.contextMenu.show(
          [
            { id: "copy", label: "Copy" },
            { id: "add-to-chat", label: "Add to chat" },
          ],
          nextAction.position,
        );
        if (requestId !== selectionActionRequestIdRef.current) return;
        if (clicked === "copy") {
          // A rebuilt or retargeted viewport supersedes the request mid-write.
          const isCurrent = () => requestId === selectionActionRequestIdRef.current;
          await copySelectionText(copyText, isCurrent);
          if (isCurrent()) terminal.focus();
          return;
        }
        if (clicked !== "add-to-chat") return;
        handleAddTerminalContext(nextAction.selection);
        terminalRef.current?.clearSelection();
        terminalRef.current?.focus();
      } finally {
        selectionActionOpenRef.current = false;
      }
    };

    const inputBlocked = () =>
      Boolean(readWorkspaceUnavailable()) ||
      (inputBinding.ordered && inputBinding.error !== null) ||
      inputBinding.renderer?.owner !== rendererOwner;
    const sendTerminalInput = (data: string, fallbackError: string) => {
      if (!replyGuard.acceptInput()) return;
      if (inputBlocked() || data.length === 0) return;
      inputBinding.pendingFallbacks.push({ remaining: data.length, message: fallbackError });
      inputScheduler.enqueue(data);
    };

    terminal.attachCustomKeyEventHandler((event) => {
      if (event.type === "keydown") handleSizeTrigger("keypress");
      const currentKeybindings = keybindingsRef.current;
      const options = { context: { terminalFocus: true, terminalOpen: true } };
      if (
        isTerminalNewCenterShortcut(event, currentKeybindings, options) ||
        isTerminalSplitShortcut(event, currentKeybindings, options) ||
        isTerminalSplitVerticalShortcut(event, currentKeybindings, options) ||
        isTerminalNewShortcut(event, currentKeybindings, options) ||
        isTerminalCloseShortcut(event, currentKeybindings, options) ||
        isDiffToggleShortcut(event, currentKeybindings, options)
      ) {
        return false;
      }

      const clipboard = terminalClipboardShortcut(event);
      if (clipboard !== null) {
        if (clipboard.onlyWithSelection && !terminal.hasSelection()) return true;
        if (clipboard.action === "copy" && event.shiftKey) {
          event.preventDefault();
          event.stopPropagation();
          if (terminal.hasSelection() && !copyThroughTextarea(terminal, terminal.getSelection())) {
            writeSystemMessage(terminal, `Copy failed. Try ${event.metaKey ? "Cmd" : "Ctrl"}+C.`);
          }
        }
        // Let the browser dispatch trusted clipboard events to xterm's own
        // handlers, including bracketed paste. Returning false only stops
        // xterm turning the shortcut into terminal input.
        return false;
      }

      const navigationData = terminalNavigationShortcutData(event);
      if (navigationData !== null) {
        event.preventDefault();
        event.stopPropagation();
        sendTerminalInput(navigationData, "Failed to move cursor");
        return false;
      }

      const deleteData = terminalDeleteShortcutData(event);
      if (deleteData !== null) {
        event.preventDefault();
        event.stopPropagation();
        sendTerminalInput(deleteData, "Failed to delete terminal input");
        return false;
      }

      const newlineData = terminalNewlineShortcutData(event);
      if (newlineData !== null) {
        event.preventDefault();
        event.stopPropagation();
        sendTerminalInput(newlineData, "Failed to insert newline");
        return false;
      }

      if (!isTerminalClearShortcut(event)) return true;
      event.preventDefault();
      event.stopPropagation();
      sendTerminalInput("\u000c", "Failed to clear terminal");
      return false;
    });

    const terminalLinksDisposable = terminal.registerLinkProvider({
      provideLinks: (bufferLineNumber, callback) => {
        const activeTerminal = terminalRef.current;
        if (!activeTerminal) {
          callback(undefined);
          return;
        }

        const wrappedLine = collectWrappedTerminalLinkLine(bufferLineNumber, (bufferLineIndex) =>
          activeTerminal.buffer.active.getLine(bufferLineIndex),
        );
        if (!wrappedLine) {
          callback(undefined);
          return;
        }

        const links = extractTerminalLinks(wrappedLine.text)
          .map((match) => ({
            match,
            range: resolveWrappedTerminalLinkRange(wrappedLine, match),
          }))
          .filter(({ range }) =>
            wrappedTerminalLinkRangeIntersectsBufferLine(range, bufferLineNumber),
          );
        if (links.length === 0) {
          callback(undefined);
          return;
        }

        callback(
          links.map(({ match, range }) => ({
            text: match.text,
            range,
            activate: (event: MouseEvent) => {
              if (!isTerminalLinkActivation(event)) return;

              const latestTerminal = terminalRef.current;
              if (!latestTerminal) return;

              if (match.kind === "url") {
                routeTerminalUrl(match.text, physicalShiftKey(event));
                return;
              }
              const unavailableReason = readWorkspaceUnavailable();
              if (unavailableReason !== null) {
                writeSystemMessage(latestTerminal, unavailableReason);
                return;
              }
              const localPath = match.text.startsWith("file://")
                ? fileUrlToPath(match.text)
                : match.text;
              if (localPath === null) {
                writeSystemMessage(latestTerminal, "Unable to open this file link.");
                return;
              }
              if (isNetworkPath(localPath)) {
                writeSystemMessage(
                  latestTerminal,
                  "Network paths can't be opened from the terminal.",
                );
                return;
              }
              const editorTarget = resolvePathLinkTarget(localPath, cwd);
              const previewFile = terminalPreviewFilePath(localPath, cwd);
              if (
                previewFile !== null &&
                readHasThread() &&
                canPreviewAnyUrlInRuntime() &&
                !physicalShiftKey(event)
              ) {
                void (async () => {
                  const outcome = await openPreviewFile(previewFile);
                  if (outcome.ok) return;
                  const onOpenInEditor = () => openEditorPath(editorTarget);
                  if (outcome.outside) showFileOutsideWorkspaceNotice({ onOpenInEditor });
                  else showPreviewFailedNotice({ onOpenInEditor });
                })();
                return;
              }
              openEditorPath(editorTarget);
            },
          })),
        );
      },
    });

    const inputDisposable = terminal.onData((data) => {
      sendTerminalInput(data, "Terminal write failed");
    });

    const selectionDisposable = terminal.onSelectionChange(() => {
      if (terminalRef.current?.hasSelection()) {
        return;
      }
      clearSelectionAction();
    });

    const handleContextMenu = (event: MouseEvent) => {
      if (!localApi || !terminal.hasSelection()) return;
      event.preventDefault();
      event.stopPropagation();
      selectionPointerRef.current = { x: event.clientX, y: event.clientY };
      void showSelectionAction();
    };
    const handlePointerDown = () => {
      handleSizeTrigger("pointer");
      clearSelectionAction();
      terminal.focus();
    };
    // Mouse-capturing CLIs (Codex, opencode) would receive a plain drag and
    // leave nothing for Ctrl+C to copy. Invert xterm's force-selection
    // modifier: a plain drag selects, Shift (Option on macOS) + drag reaches
    // the app. A gesture sent to the app keeps the inversion through its drag
    // and release, which xterm reports from document listeners. A selection
    // gesture inverts only its mousedown: a synthesized Option on release would
    // trigger xterm's Option-click cursor movement on macOS.
    // xterm also clears its selection on every report it sends the app, so
    // while a selection exists a right-click is kept from the app (the copy
    // menu opens instead) and, under any-motion tracking, so is hover. Hover
    // stops at `.xterm-screen` (xterm's stable screen element), after xterm's
    // link detection there and before the report listener on `.xterm`.
    const forceSelectionKey = isMacPlatform(navigator.platform) ? "altKey" : "shiftKey";
    const invertForceSelectionKey = (event: MouseEvent) => {
      if (!physicalShiftKeys.has(event)) physicalShiftKeys.set(event, event.shiftKey);
      Object.defineProperty(event, forceSelectionKey, { value: !event[forceSelectionKey] });
    };
    let invertingGesture = false;
    const handleMouseDown = (event: MouseEvent) => {
      const tracking = terminal.modes.mouseTrackingMode !== "none";
      if (tracking && event.button === 2 && terminal.hasSelection()) {
        Object.defineProperty(event, forceSelectionKey, { value: true });
        return;
      }
      if (event.button !== 0 || !tracking) {
        invertingGesture = false;
        return;
      }
      invertingGesture = event[forceSelectionKey];
      invertForceSelectionKey(event);
    };
    const handleHover = (event: MouseEvent) => {
      if (
        event.buttons === 0 &&
        terminal.modes.mouseTrackingMode === "any" &&
        terminal.hasSelection()
      ) {
        event.stopPropagation();
      }
    };
    const handleGestureMouse = (event: MouseEvent) => {
      if (!invertingGesture) return;
      invertForceSelectionKey(event);
      if (event.type === "mouseup" && event.button === 0) invertingGesture = false;
    };
    // The program reads its own host's clipboard, never this client's, so a
    // pasted image is uploaded and the path of its server copy is pasted.
    const pasteAborts = new Set<AbortController>();
    cancelImagePastesRef.current = () => {
      for (const abort of pasteAborts) abort.abort();
    };
    const handlePaste = (event: ClipboardEvent) => {
      const data = event.clipboardData;
      if (!data || data.getData("text/plain") !== "") return;
      const image = Array.from(data.files).find((file) => file.type.startsWith("image/"));
      if (!image) return;
      event.preventDefault();
      event.stopPropagation();
      if (inputBlocked()) return;
      if (!isTerminalImagePasteMimeType(image.type)) {
        writeSystemMessage(terminal, "Image paste supports PNG, JPEG, GIF and WebP images.");
        return;
      }
      const abort = new AbortController();
      const staging = stageImagePaste(image, image.type, abort.signal);
      if (staging === null) {
        writeSystemMessage(
          terminal,
          "Image paste needs a newer BiBCode server on this environment.",
        );
        return;
      }
      // Input typed during the upload waits behind the path in the terminal's
      // persistent input queue, so Enter cannot submit the prompt early.
      const generation = readTerminalGeneration();
      const bracketedAtPaste = terminal.modes.bracketedPasteMode;
      const fallback = { remaining: 0, message: "Terminal write failed" };
      inputBinding.pendingFallbacks.push(fallback);
      const fill = inputScheduler.reserve();
      pasteAborts.add(abort);
      setImagePastesPending((count) => count + 1);
      void staging.then((result) => {
        pasteAborts.delete(abort);
        setImagePastesPending((count) => count - 1);
        let path: string | null = null;
        if (result._tag === "Success") {
          if (readTerminalGeneration() === generation) path = result.value;
          else if (!disposed) {
            writeSystemMessage(terminal, "Image paste dropped: the terminal restarted.");
          }
        } else if (!disposed && !isAtomCommandInterrupted(result)) {
          const error = squashAtomCommandFailure(result);
          writeSystemMessage(
            terminal,
            `Image paste failed: ${error instanceof Error ? error.message : "upload error"}`,
          );
        }
        if (inputBlocked()) path = null;
        const quoted = path === null ? null : quoteTerminalPastePath(path, readServerOs());
        // The program may toggle bracketed paste while the image uploads.
        const bracketed = disposed ? bracketedAtPaste : terminal.modes.bracketedPasteMode;
        const text = quoted === null ? null : bracketed ? `\x1b[200~${quoted}\x1b[201~` : quoted;
        if (text !== null) {
          fallback.remaining = text.length;
          fallback.message = "Image paste failed";
        }
        fill(text);
      });
    };
    const ownerDocument = mount.ownerDocument;
    mount.addEventListener("paste", handlePaste, true);
    mount.addEventListener("mousedown", handleMouseDown, true);
    const screenElement = mount.querySelector<HTMLElement>(".xterm-screen");
    screenElement?.addEventListener("mousemove", handleHover);
    ownerDocument.addEventListener("mousemove", handleGestureMouse, true);
    ownerDocument.addEventListener("mouseup", handleGestureMouse, true);
    mount.addEventListener("contextmenu", handleContextMenu);
    mount.addEventListener("pointerdown", handlePointerDown);
    const handleFocus = () => handleSizeTrigger("focus");
    const handleWindowFocus = () => {
      if (mount.contains(document.activeElement)) handleFocus();
    };
    mount.addEventListener("focusin", handleFocus);
    window.addEventListener("focus", handleWindowFocus);

    const themeObserver = new MutationObserver(() => {
      const activeTerminal = terminalRef.current;
      if (!activeTerminal) return;
      // <html> class/style churn (tooltips, scroll locks) fires this observer
      // far more often than the theme changes; a repaint rebuilds the glyph
      // atlas, so skip it unless the resolved theme actually differs.
      repaintTerminalTheme(activeTerminal, effectiveTerminalThemeRef.current, containerRef.current);
    });
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class", "style"],
    });

    const fitTimer = window.setTimeout(() => {
      const activeTerminal = terminalRef.current;
      const activeFitAddon = fitAddonRef.current;
      if (!activeTerminal || !activeFitAddon) return;
      handleSizeTrigger("layout");
    }, 30);

    return () => {
      if (disposed) return;
      disposed = true;
      updateMirroringSize(false);

      resizeRendererGenerationRef.current += 1;
      lastRequestedSizeRef.current = null;
      window.clearTimeout(fitTimer);
      outputSink.dispose();
      replyGuard.dispose();
      if (rendererRef.current === renderer) rendererRef.current = null;
      setRendererAttachmentInput(null);
      unregisterSizeReader();
      inputDisposable.dispose();
      selectionDisposable.dispose();
      terminalLinksDisposable.dispose();
      clearSelectionAction();
      cancelImagePastesRef.current();
      cancelImagePastesRef.current = () => undefined;
      mount.removeEventListener("paste", handlePaste, true);
      mount.removeEventListener("mousedown", handleMouseDown, true);
      screenElement?.removeEventListener("mousemove", handleHover);
      ownerDocument.removeEventListener("mousemove", handleGestureMouse, true);
      ownerDocument.removeEventListener("mouseup", handleGestureMouse, true);
      mount.removeEventListener("contextmenu", handleContextMenu);
      mount.removeEventListener("pointerdown", handlePointerDown);
      mount.removeEventListener("focusin", handleFocus);
      window.removeEventListener("focus", handleWindowFocus);
      themeObserver.disconnect();
      if (inputBinding.renderer?.owner === rendererOwner) {
        inputBinding.renderer = null;
      }
      if (inputSchedulerRef.current === inputScheduler) {
        inputSchedulerRef.current = null;
      }
      if (terminalRef.current === terminal) {
        terminalRef.current = null;
      }
      if (fitAddonRef.current === fitAddon) {
        fitAddonRef.current = null;
      }
      webglGenerationRef.current += 1;
      const activeWebglLifecycle = webglLifecycleRef.current;
      if (activeWebglLifecycle?.terminal === terminal) {
        activeWebglLifecycle.dispose();
        if (webglLifecycleRef.current === activeWebglLifecycle) {
          webglLifecycleRef.current = null;
        }
      }
      terminal.dispose();
    };
    // Focus is handled by a separate activation effect so it never tears down this renderer.
  }, [
    terminalTargetKey,
    runTerminalResize,
    canAttachTerminal,
    command,
    cwd,
    environmentId,
    runtimeEnvKey,
    shouldRender,
    terminalId,
    threadId,
    sizeOwnership,
    orderedInput,
    worktreePath,
  ]);

  useEffect(() => {
    const renderer = rendererRef.current;
    if (
      !shouldRender ||
      !renderer ||
      transcriptRuntime === null ||
      rendererEpoch !== rendererEpochRef.current
    )
      return;
    let generation: number | null = null;
    const attachment = transcriptRuntime.attachRenderer((signal) => {
      const nextGeneration = transcriptRuntime.metadata().generation;
      const claimOnAttach = signal.type === "reset" && generation !== nextGeneration;
      generation = nextGeneration;
      renderer.render(signal, claimOnAttach);
    }, renderer.sizeClaim);
    return () => attachment.detach();
  }, [shouldRender, transcriptRuntime, rendererEpoch]);

  useEffect(() => {
    const activeTerminal = terminalRef.current;
    if (!activeTerminal) return;
    repaintTerminalTheme(activeTerminal, effectiveTerminalTheme, containerRef.current);
  }, [effectiveTerminalTheme]);

  useEffect(() => {
    const terminal = terminalRef.current;
    const fitAddon = fitAddonRef.current;
    if (!shouldRender || terminal === null || fitAddon === null) return;

    terminal.options.fontFamily = terminalFontFamily;
    const rendererGeneration = resizeRendererGenerationRef.current;
    let cancelled = false;

    void ensureBundledTerminalFontLoaded().then(() => {
      if (
        cancelled ||
        resizeRendererGenerationRef.current !== rendererGeneration ||
        terminalRef.current !== terminal ||
        fitAddonRef.current !== fitAddon
      ) {
        return;
      }

      terminal.clearTextureAtlas();
      rendererRef.current?.size("layout");
      terminal.refresh(0, terminal.rows - 1);
    });

    return () => {
      cancelled = true;
    };
  }, [shouldRender, terminalFontFamily]);

  useEffect(() => {
    const terminal = terminalRef.current;
    if (
      !shouldRender ||
      !webglEnabled ||
      !hasAuthoritativeTerminalState ||
      terminal === null ||
      webglFailedRef.current
    ) {
      return;
    }
    // A stale render's epoch means a fresh terminal was just created and this
    // effect will re-run once the epoch state commits — attaching now would
    // create a second context for the same terminal.
    if (rendererEpoch !== rendererEpochRef.current) {
      return;
    }

    const generation = webglGenerationRef.current + 1;
    webglGenerationRef.current = generation;
    let cleanedUp = false;
    let lifecycle: WebglLifecycle | null = null;
    const isCurrent = () =>
      !cleanedUp && webglGenerationRef.current === generation && terminalRef.current === terminal;

    void (async () => {
      try {
        const { WebglAddon } = await loadTerminalWebglAddon();
        if (!isCurrent()) return;

        const addon = new WebglAddon();
        lifecycle = createWebglLifecycle(addon, terminal);
        terminal.loadAddon(addon);
        if (!isCurrent()) {
          lifecycle.dispose();
          return;
        }

        // WebKit never reports a canvas's exact device-pixel box, and the addon
        // responds by disconnecting its own observer for good, so the backing
        // store keeps a rounded size and the compositor rescales every glyph.
        const webglCanvas = webglCanvasFrom(addon);
        if (webglCanvas) {
          lifecycle.setDevicePixelDisposable(
            observeCanvasDevicePixelSize(webglCanvas, (width, height) => {
              if (terminalRef.current !== terminal) return;
              if (!resizeWebglCanvasBackingStore(addon, width, height)) return;
              terminal.refresh(0, terminal.rows - 1);
            }),
          );
        }

        const contextLossDisposable = addon.onContextLoss(() => {
          const activeLifecycle = lifecycle;
          if (activeLifecycle === null || activeLifecycle.disposed) return;
          webglFailedRef.current = true;
          activeLifecycle.dispose();
          if (webglLifecycleRef.current === activeLifecycle) {
            webglLifecycleRef.current = null;
          }
          recordWebglDiagnosticOnce();
        });
        lifecycle.setContextLossDisposable(contextLossDisposable);
        if (!isCurrent() || lifecycle.disposed) {
          lifecycle.dispose();
          return;
        }

        webglLifecycleRef.current = lifecycle;
      } catch {
        lifecycle?.dispose();
        if (!isCurrent()) return;

        webglFailedRef.current = true;
        if (webglLifecycleRef.current === lifecycle) {
          webglLifecycleRef.current = null;
        }
        recordWebglDiagnosticOnce();
      }
    })();

    return () => {
      cleanedUp = true;
      if (webglGenerationRef.current === generation) {
        webglGenerationRef.current += 1;
      }
      lifecycle?.dispose();
      if (webglLifecycleRef.current === lifecycle) {
        webglLifecycleRef.current = null;
      }
    };
  }, [
    recordWebglDiagnosticOnce,
    shouldRender,
    hasAuthoritativeTerminalState,
    webglEnabled,
    rendererEpoch,
  ]);

  useEffect(() => {
    if (!shouldRender || !hasAuthoritativeTerminalState) return;

    const previous = previousMetadataRef.current;
    const suppressThemeRestartExit =
      themeRestartPending && (terminalStatus === "closed" || terminalStatus === "exited");
    previousMetadataRef.current = {
      error: terminalError,
      status: suppressThemeRestartExit ? previous.status : terminalStatus,
    };
    const terminal = terminalRef.current;

    if (terminal !== null && terminalError !== null && terminalError !== previous.error) {
      writeSystemMessage(terminal, terminalError);
    }

    if (suppressThemeRestartExit) {
      return;
    }
    if (terminalStatus === "running") {
      hasHandledExitRef.current = false;
    } else if (
      (terminalStatus === "closed" || terminalStatus === "exited") &&
      terminalStatus !== previous.status &&
      !hasHandledExitRef.current
    ) {
      hasHandledExitRef.current = true;
      if (terminal !== null) {
        writeSystemMessage(
          terminal,
          terminalStatus === "closed" ? "Terminal closed" : "Process exited",
        );
      }
      handleSessionExited();
    }
  }, [
    hasAuthoritativeTerminalState,
    shouldRender,
    terminalError,
    terminalStatus,
    themeRestartPending,
  ]);

  useEffect(() => {
    if (!shouldRender || transcriptRuntime === null) return;
    const binding = terminalInputBindings.get(
      terminalInputKey(environmentId, threadId, terminalId),
    );
    if (!binding || binding.renderer?.terminal !== terminalRef.current) return;
    const previous = binding.attachment;
    if (
      previous !== null &&
      (previous.runtime !== transcriptRuntime || previous.generation !== terminalGeneration)
    ) {
      inputSchedulerRef.current?.reset();
    }
    binding.attachment = { runtime: transcriptRuntime, generation: terminalGeneration };
    setInputReattaching(false);
    const generation = binding.generation;
    void runPrepareInput({ environmentId, input: { threadId, terminalId } }).then((result) => {
      if (
        generation !== binding.generation ||
        result._tag === "Success" ||
        isAtomCommandInterrupted(result)
      )
        return;
      const error = squashAtomCommandFailure(result);
      binding.error = error instanceof Error ? error.message : "Terminal input could not connect.";
      binding.renderer?.onInputError(binding.error);
    });
  }, [
    environmentId,
    orderedInput,
    runPrepareInput,
    shouldRender,
    terminalGeneration,
    terminalId,
    threadId,
    transcriptRuntime,
  ]);

  useEffect(() => {
    if (terminalError !== null) setInputReattaching(false);
  }, [terminalError]);

  useEffect(() => {
    const generation = focusGenerationRef.current + 1;
    focusGenerationRef.current = generation;
    if (
      !autoFocus ||
      !shouldRender ||
      transcriptRuntime === null ||
      fulfilledFocusRequestIdRef.current === focusRequestId
    ) {
      return;
    }
    const terminal = terminalRef.current;
    const fitAddon = fitAddonRef.current;
    if (!terminal || !fitAddon) return;
    const frame = window.requestAnimationFrame(() => {
      if (
        focusGenerationRef.current !== generation ||
        terminalRef.current !== terminal ||
        fitAddonRef.current !== fitAddon
      ) {
        return;
      }
      terminal.focus();
      fulfilledFocusRequestIdRef.current = focusRequestId;
    });
    return () => {
      focusGenerationRef.current += 1;
      window.cancelAnimationFrame(frame);
    };
  }, [autoFocus, focusRequestId, shouldRender, transcriptRuntime]);

  useEffect(() => {
    const container = containerRef.current;
    if (!shouldRender || !container) return;

    let frame: number | null = null;
    let refreshTimer: number | null = null;
    const observer = new ResizeObserver(() => {
      if (frame !== null) {
        window.cancelAnimationFrame(frame);
      }
      if (refreshTimer !== null) {
        window.clearTimeout(refreshTimer);
        refreshTimer = null;
      }
      const terminal = terminalRef.current;
      const fitAddon = fitAddonRef.current;
      if (!terminal || !fitAddon) return;
      const resizeRendererGeneration = resizeRendererGenerationRef.current;
      frame = window.requestAnimationFrame(() => {
        frame = null;
        if (terminalRef.current !== terminal || fitAddonRef.current !== fitAddon) return;
        rendererRef.current?.size("layout");
        refreshTimer = window.setTimeout(() => {
          refreshTimer = null;
          if (
            resizeRendererGenerationRef.current !== resizeRendererGeneration ||
            terminalRef.current !== terminal ||
            fitAddonRef.current !== fitAddon
          ) {
            return;
          }
          terminal.clearTextureAtlas();
          terminal.refresh(0, terminal.rows - 1);
        }, 50);
      });
    });
    observer.observe(container);

    return () => {
      observer.disconnect();
      if (frame !== null) {
        window.cancelAnimationFrame(frame);
      }
      if (refreshTimer !== null) {
        window.clearTimeout(refreshTimer);
      }
    };
  }, [shouldRender]);

  useEffect(() => {
    const terminal = terminalRef.current;
    const fitAddon = fitAddonRef.current;
    if (!shouldRender || !terminal || !fitAddon) return;
    const resizeRendererGeneration = resizeRendererGenerationRef.current;
    const frame = window.requestAnimationFrame(() => {
      if (
        resizeRendererGenerationRef.current !== resizeRendererGeneration ||
        terminalRef.current !== terminal
      )
        return;
      rendererRef.current?.size("layout");
    });
    return () => {
      window.cancelAnimationFrame(frame);
    };
  }, [environmentId, resizeEpoch, shouldRender, terminalId, threadId]);
  return (
    <div className="relative h-full w-full overflow-hidden rounded-[4px] bg-background">
      <div
        ref={containerRef}
        className="h-full w-full overflow-hidden"
        data-terminal-xterm-mount={terminalId}
      />
      {shouldRender && sizeOwnership && mirroringSize ? (
        <div className="absolute inset-x-2 top-10 z-20 flex max-w-sm flex-wrap items-center gap-2 rounded-md border bg-popover px-3 py-2 text-xs text-popover-foreground shadow-md">
          <span>Sized for another window.</span>
          <Button
            type="button"
            size="xs"
            variant="outline"
            onClick={() => rendererRef.current?.size("fit")}
          >
            Fit to this window
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label="Dismiss terminal size notice"
            onClick={() => rendererRef.current?.dismissSizeNotice()}
          >
            <XIcon className="size-3.5" />
          </Button>
        </div>
      ) : null}
      {enableTerminalAgentActivity && shouldRender && command?.activity !== undefined ? (
        <div
          className="pointer-events-none absolute inset-x-0 top-7 bottom-0 z-10"
          data-provider-terminal-activity-host={terminalId}
        >
          <ProviderTerminalActivityDock
            threadRef={threadRef}
            projectId={projectId}
            terminalId={terminalId}
            activity={command.activity}
            visible={shouldRender}
            compact={false}
          />
        </div>
      ) : null}
      {shouldRender && transcriptRuntime === null && terminalError !== null ? (
        <div
          role="alert"
          className="absolute inset-0 flex items-center justify-center px-4 text-center text-xs text-destructive"
        >
          {terminalError}
        </div>
      ) : null}
      {shouldRender && imagePastesPending > 0 ? (
        <div
          role="status"
          className="absolute top-2 left-2 z-20 flex items-center gap-3 rounded-md border bg-popover px-3 py-2 text-xs text-popover-foreground shadow-md"
        >
          <span>Uploading pasted image… Typing continues after it.</span>
          <Button
            type="button"
            size="xs"
            variant="outline"
            onClick={() => cancelImagePastesRef.current()}
          >
            Cancel
          </Button>
        </div>
      ) : null}
      {shouldRender && orderedInput && inputError !== null ? (
        <div
          role="alert"
          className="absolute inset-x-2 bottom-2 z-20 flex items-center gap-3 rounded-md border bg-popover px-3 py-2 text-xs text-popover-foreground shadow-md"
        >
          <span className="min-w-0 flex-1">{inputError}</span>
          <Button
            type="button"
            size="xs"
            variant="outline"
            disabled={inputReattaching}
            onClick={() => {
              setInputReattaching(true);
              terminalSession.reattach();
            }}
          >
            {inputReattaching ? "Reconnecting…" : "Reconnect input"}
          </Button>
        </div>
      ) : null}
      {showCodexThemeNotice ? (
        <div className="absolute top-2 right-2 z-10 flex max-w-sm items-center gap-2 rounded-md border bg-popover px-2.5 py-2 text-xs text-popover-foreground shadow-md">
          <span>This terminal cached the previous theme.</span>
          <Button
            type="button"
            size="xs"
            disabled={themeRestartPending || workspaceUnavailable !== null}
            title={workspaceUnavailable ?? undefined}
            aria-label={`Restart ${terminalLabel} to apply ${resolvedTheme === "light" ? "Light" : "Dark"} theme`}
            onClick={restartForTheme}
          >
            {themeRestartPending ? "Restarting…" : "Restart to apply"}
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label="Dismiss terminal theme notice"
            disabled={themeRestartPending}
            onClick={() =>
              setDismissedThemeNotice({
                generation: terminalGeneration,
                targetTheme: resolvedTheme,
              })
            }
          >
            <XIcon className="size-3.5" />
          </Button>
          {visibleThemeRestartError ? (
            <span role="alert" className="text-destructive">
              {visibleThemeRestartError}
            </span>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

export type TerminalPanelOwner = "center-panel" | "right-panel";

interface ThreadTerminalPanelProps {
  readonly owner: TerminalPanelOwner;
  threadRef: ScopedThreadRef;
  threadId: ThreadId;
  projectId: ProjectId;
  cwd: string;
  worktreePath?: string | null;
  runtimeEnv?: Record<string, string>;
  terminalIds: string[];
  activeTerminalId: string;
  terminalGroups: ThreadTerminalGroup[];
  activeTerminalGroupId: string;
  focusRequestId: number;
  focusEligible?: boolean;
  onSplitTerminal?: (() => void) | undefined;
  onSplitTerminalVertical?: (() => void) | undefined;
  onNewTerminal?: (() => void) | undefined;
  splitShortcutLabel?: string | undefined;
  splitVerticalShortcutLabel?: string | undefined;
  newShortcutLabel?: string | undefined;
  closeShortcutLabel?: string | undefined;
  onActiveTerminalChange: (terminalId: string) => void;
  onCloseTerminal: (terminalId: string) => void;
  onAddTerminalContext: (selection: TerminalContextSelection) => void;
  keybindings: ResolvedKeybindingsConfig;
  /** Prefer server-provided tab titles when present (e.g. active subprocess name). */
  terminalLabelsById?: ReadonlyMap<string, string>;
  /** Structured launch commands for provider terminals that have not started yet. */
  terminalCommandsById?: ReadonlyMap<string, TerminalLaunchCommand>;
  /** Prefer per-session launch locations when the server already knows a terminal. */
  terminalLaunchLocationsById?: ReadonlyMap<string, TerminalLaunchLocation>;
  workspaceUnavailable?: string | null;
}

interface TerminalActionButtonProps {
  label: string;
  className: string;
  onClick: () => void;
  children: ReactNode;
  disabledReason?: string | null;
}

function TerminalActionButton({
  label,
  className,
  onClick,
  children,
  disabledReason = null,
}: TerminalActionButtonProps) {
  return (
    <Popover>
      <PopoverTrigger
        openOnHover
        render={
          <button
            type="button"
            className={className}
            onClick={onClick}
            aria-label={label}
            disabled={disabledReason !== null}
            title={disabledReason ?? undefined}
          />
        }
      >
        {children}
      </PopoverTrigger>
      <PopoverPopup
        tooltipStyle
        side="bottom"
        sideOffset={6}
        align="center"
        className="pointer-events-none select-none"
      >
        {disabledReason ?? label}
      </PopoverPopup>
    </Popover>
  );
}

export default function ThreadTerminalPanel({
  owner,
  threadRef,
  threadId,
  projectId,
  cwd,
  worktreePath,
  runtimeEnv,
  terminalIds,
  activeTerminalId,
  terminalGroups,
  activeTerminalGroupId,
  focusRequestId,
  focusEligible = true,
  onSplitTerminal,
  onSplitTerminalVertical,
  onNewTerminal,
  splitShortcutLabel,
  splitVerticalShortcutLabel,
  newShortcutLabel,
  closeShortcutLabel,
  onActiveTerminalChange,
  onCloseTerminal,
  onAddTerminalContext,
  keybindings,
  terminalLabelsById,
  terminalCommandsById,
  terminalLaunchLocationsById,
  workspaceUnavailable = null,
}: ThreadTerminalPanelProps) {
  const normalizedTerminalIds = useMemo(() => {
    const normalizedIds: string[] = [];
    const seen = new Set<string>();
    for (const id of terminalIds) {
      const trimmedId = id.trim();
      if (trimmedId.length === 0 || seen.has(trimmedId)) continue;
      seen.add(trimmedId);
      normalizedIds.push(trimmedId);
    }
    return normalizedIds;
  }, [terminalIds]);

  const resolvedActiveTerminalId =
    normalizedTerminalIds.length === 0
      ? ""
      : normalizedTerminalIds.includes(activeTerminalId)
        ? activeTerminalId
        : (normalizedTerminalIds[0] ?? "");

  const resolvedTerminalGroups = useMemo(() => {
    if (normalizedTerminalIds.length === 0) {
      return [];
    }
    const validTerminalIdSet = new Set(normalizedTerminalIds);
    const assignedTerminalIds = new Set<string>();
    const usedGroupIds = new Set<string>();
    const nextGroups: ThreadTerminalGroup[] = [];

    const assignUniqueGroupId = (groupId: string): string => {
      if (!usedGroupIds.has(groupId)) {
        usedGroupIds.add(groupId);
        return groupId;
      }
      let suffix = 2;
      while (usedGroupIds.has(`${groupId}-${suffix}`)) {
        suffix += 1;
      }
      const uniqueGroupId = `${groupId}-${suffix}`;
      usedGroupIds.add(uniqueGroupId);
      return uniqueGroupId;
    };

    for (const terminalGroup of terminalGroups) {
      const nextTerminalIds: string[] = [];
      const seenGroupTerminalIds = new Set<string>();
      for (const id of terminalGroup.terminalIds) {
        const terminalId = id.trim();
        if (terminalId.length === 0) continue;
        if (seenGroupTerminalIds.has(terminalId)) continue;
        seenGroupTerminalIds.add(terminalId);
        if (!validTerminalIdSet.has(terminalId)) continue;
        if (assignedTerminalIds.has(terminalId)) continue;
        nextTerminalIds.push(terminalId);
      }
      if (nextTerminalIds.length === 0) continue;

      for (const terminalId of nextTerminalIds) {
        assignedTerminalIds.add(terminalId);
      }

      const baseGroupId =
        terminalGroup.id.trim().length > 0
          ? terminalGroup.id.trim()
          : `group-${nextTerminalIds[0] ?? normalizedTerminalIds[0] ?? ""}`;
      nextGroups.push({
        id: assignUniqueGroupId(baseGroupId),
        terminalIds: nextTerminalIds,
        ...(terminalGroup.splitDirection === "vertical"
          ? { splitDirection: "vertical" as const }
          : {}),
      });
    }

    for (const terminalId of normalizedTerminalIds) {
      if (assignedTerminalIds.has(terminalId)) continue;
      nextGroups.push({
        id: assignUniqueGroupId(`group-${terminalId}`),
        terminalIds: [terminalId],
      });
    }

    const terminalOrderIndex = new Map(
      normalizedTerminalIds.map((id, index) => [id, index] as const),
    );
    nextGroups.sort((left, right) => {
      const rank = (ids: readonly string[]) =>
        Math.min(...ids.map((id) => terminalOrderIndex.get(id) ?? Number.POSITIVE_INFINITY));
      return rank(left.terminalIds) - rank(right.terminalIds);
    });

    return nextGroups;
  }, [normalizedTerminalIds, terminalGroups]);

  const resolvedActiveGroupIndex = useMemo(() => {
    const indexById = resolvedTerminalGroups.findIndex(
      (terminalGroup) => terminalGroup.id === activeTerminalGroupId,
    );
    if (indexById >= 0) return indexById;
    const indexByTerminal = resolvedTerminalGroups.findIndex((terminalGroup) =>
      terminalGroup.terminalIds.includes(resolvedActiveTerminalId),
    );
    return indexByTerminal >= 0 ? indexByTerminal : 0;
  }, [activeTerminalGroupId, resolvedActiveTerminalId, resolvedTerminalGroups]);

  const visibleTerminalIds =
    resolvedTerminalGroups[resolvedActiveGroupIndex]?.terminalIds ??
    (normalizedTerminalIds.length > 0 ? [resolvedActiveTerminalId] : []);
  const splitDirection =
    resolvedTerminalGroups[resolvedActiveGroupIndex]?.splitDirection ?? "horizontal";
  const hasTerminalSidebar = normalizedTerminalIds.length > 1;
  const isSplitView = visibleTerminalIds.length > 1;
  const showGroupHeaders =
    resolvedTerminalGroups.length > 1 ||
    resolvedTerminalGroups.some((terminalGroup) => terminalGroup.terminalIds.length > 1);
  const hasReachedSplitLimit = visibleTerminalIds.length >= MAX_TERMINALS_PER_GROUP;
  const terminalLabelById = useMemo(() => {
    const next = new Map<string, string>();
    for (const terminalId of normalizedTerminalIds) {
      next.set(terminalId, terminalLabelsById?.get(terminalId) ?? getTerminalLabel(terminalId));
    }
    return next;
  }, [normalizedTerminalIds, terminalLabelsById]);
  const resolveTerminalLaunchLocation = useCallback(
    (terminalId: string): TerminalLaunchLocation => {
      return (
        terminalLaunchLocationsById?.get(terminalId) ?? {
          cwd,
          ...(worktreePath !== undefined ? { worktreePath } : {}),
          ...(runtimeEnv ? { runtimeEnv } : {}),
        }
      );
    },
    [cwd, runtimeEnv, terminalLaunchLocationsById, worktreePath],
  );
  const splitTerminalActionLabel = hasReachedSplitLimit
    ? `Split Terminal Horizontally (max ${MAX_TERMINALS_PER_GROUP} per group)`
    : splitShortcutLabel
      ? `Split Terminal Horizontally (${splitShortcutLabel})`
      : "Split Terminal Horizontally";
  const splitTerminalVerticalActionLabel = hasReachedSplitLimit
    ? `Split Terminal Vertically (max ${MAX_TERMINALS_PER_GROUP} per group)`
    : splitVerticalShortcutLabel
      ? `Split Terminal Vertically (${splitVerticalShortcutLabel})`
      : "Split Terminal Vertically";
  const newTerminalActionLabel = newShortcutLabel
    ? `New Terminal (${newShortcutLabel})`
    : "New Terminal";
  const hasTerminalToolbarActions = Boolean(
    onSplitTerminal || onSplitTerminalVertical || onNewTerminal,
  );
  const onSplitTerminalAction = useCallback(() => {
    if (workspaceUnavailable || hasReachedSplitLimit || !onSplitTerminal) return;
    onSplitTerminal();
  }, [hasReachedSplitLimit, onSplitTerminal, workspaceUnavailable]);
  const onSplitTerminalVerticalAction = useCallback(() => {
    if (workspaceUnavailable || hasReachedSplitLimit || !onSplitTerminalVertical) return;
    onSplitTerminalVertical();
  }, [hasReachedSplitLimit, onSplitTerminalVertical, workspaceUnavailable]);
  const onNewTerminalAction = useCallback(() => {
    if (!workspaceUnavailable) onNewTerminal?.();
  }, [onNewTerminal, workspaceUnavailable]);

  if (normalizedTerminalIds.length === 0) {
    return (
      <aside
        data-terminal-owner={owner}
        className="thread-terminal-panel relative flex h-full min-w-0 flex-1 flex-col overflow-hidden bg-background"
      >
        <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-3 px-4 py-6 text-center text-sm text-muted-foreground">
          <p>No terminal sessions for this thread yet.</p>
          {onNewTerminal ? (
            <button
              type="button"
              className="rounded-md border border-border/80 bg-background px-3 py-1.5 text-xs font-medium text-foreground transition-colors hover:bg-accent"
              onClick={onNewTerminalAction}
              disabled={workspaceUnavailable !== null}
              title={workspaceUnavailable ?? undefined}
            >
              {newTerminalActionLabel}
            </button>
          ) : null}
        </div>
      </aside>
    );
  }

  const activeTerminalLaunchLocation = resolveTerminalLaunchLocation(resolvedActiveTerminalId);

  return (
    <aside
      data-terminal-owner={owner}
      className="thread-terminal-panel relative flex h-full min-w-0 flex-1 flex-col overflow-hidden bg-background"
    >
      {!hasTerminalSidebar && hasTerminalToolbarActions ? (
        <div className="pointer-events-none absolute right-2 top-2 z-20">
          <div
            data-terminal-toolbar="floating"
            className="pointer-events-auto inline-flex items-center overflow-hidden rounded-md border border-border/80 bg-background/70"
          >
            {onSplitTerminal ? (
              <>
                <TerminalActionButton
                  className={`p-1 text-foreground/90 transition-colors ${
                    hasReachedSplitLimit
                      ? "cursor-not-allowed opacity-45 hover:bg-transparent"
                      : "hover:bg-accent"
                  }`}
                  onClick={onSplitTerminalAction}
                  label={splitTerminalActionLabel}
                  disabledReason={workspaceUnavailable}
                >
                  <SquareSplitHorizontal className="size-3.25" />
                </TerminalActionButton>
                {onSplitTerminalVertical || onNewTerminal ? (
                  <div data-terminal-toolbar-separator className="h-4 w-px bg-border/80" />
                ) : null}
              </>
            ) : null}
            {onSplitTerminalVertical ? (
              <>
                <TerminalActionButton
                  className={`p-1 text-foreground/90 transition-colors ${
                    hasReachedSplitLimit
                      ? "cursor-not-allowed opacity-45 hover:bg-transparent"
                      : "hover:bg-accent"
                  }`}
                  onClick={onSplitTerminalVerticalAction}
                  label={splitTerminalVerticalActionLabel}
                  disabledReason={workspaceUnavailable}
                >
                  <SquareSplitVertical className="size-3.25" />
                </TerminalActionButton>
                {onNewTerminal ? (
                  <div data-terminal-toolbar-separator className="h-4 w-px bg-border/80" />
                ) : null}
              </>
            ) : null}
            {onNewTerminal ? (
              <TerminalActionButton
                className="p-1 text-foreground/90 transition-colors hover:bg-accent"
                onClick={onNewTerminalAction}
                label={newTerminalActionLabel}
                disabledReason={workspaceUnavailable}
              >
                <Plus className="size-3.25" />
              </TerminalActionButton>
            ) : null}
          </div>
        </div>
      ) : null}

      <div className="min-h-0 w-full flex-1">
        <div className={`flex h-full min-h-0 ${hasTerminalSidebar ? "gap-1.5" : ""}`}>
          <div className="min-w-0 flex-1">
            {isSplitView ? (
              <div
                className="grid h-full w-full min-w-0 gap-0 overflow-hidden"
                style={
                  splitDirection === "vertical"
                    ? {
                        gridTemplateRows: `repeat(${visibleTerminalIds.length}, minmax(0, 1fr))`,
                      }
                    : {
                        gridTemplateColumns: `repeat(${visibleTerminalIds.length}, minmax(0, 1fr))`,
                      }
                }
              >
                {visibleTerminalIds.map((terminalId) => {
                  const terminalLaunchLocation = resolveTerminalLaunchLocation(terminalId);
                  return (
                    <div
                      key={terminalId}
                      className={`min-h-0 min-w-0 ${
                        splitDirection === "vertical"
                          ? "border-t first:border-t-0"
                          : "border-l first:border-l-0"
                      } ${
                        terminalId === resolvedActiveTerminalId
                          ? "border-border"
                          : "border-border/70"
                      }`}
                      onMouseDown={() => {
                        if (terminalId !== resolvedActiveTerminalId) {
                          onActiveTerminalChange(terminalId);
                        }
                      }}
                    >
                      <div className="h-full p-1">
                        <TerminalViewport
                          threadRef={threadRef}
                          threadId={threadId}
                          projectId={projectId}
                          terminalId={terminalId}
                          terminalLabel={terminalLabelById.get(terminalId) ?? "Terminal"}
                          cwd={terminalLaunchLocation.cwd}
                          {...(terminalLaunchLocation.worktreePath !== undefined
                            ? { worktreePath: terminalLaunchLocation.worktreePath }
                            : {})}
                          {...(terminalLaunchLocation.runtimeEnv
                            ? { runtimeEnv: terminalLaunchLocation.runtimeEnv }
                            : {})}
                          {...(terminalCommandsById?.get(terminalId)
                            ? { command: terminalCommandsById.get(terminalId)! }
                            : {})}
                          centerPanel={owner === "center-panel"}
                          visible
                          onSessionExited={() => onCloseTerminal(terminalId)}
                          onAddTerminalContext={onAddTerminalContext}
                          focusRequestId={focusRequestId}
                          autoFocus={focusEligible && terminalId === resolvedActiveTerminalId}
                          resizeEpoch={0}
                          keybindings={keybindings}
                          workspaceUnavailable={workspaceUnavailable}
                        />
                      </div>
                    </div>
                  );
                })}
              </div>
            ) : (
              <div className="h-full p-1">
                <TerminalViewport
                  key={resolvedActiveTerminalId}
                  threadRef={threadRef}
                  threadId={threadId}
                  projectId={projectId}
                  terminalId={resolvedActiveTerminalId}
                  terminalLabel={terminalLabelById.get(resolvedActiveTerminalId) ?? "Terminal"}
                  cwd={activeTerminalLaunchLocation.cwd}
                  {...(activeTerminalLaunchLocation.worktreePath !== undefined
                    ? { worktreePath: activeTerminalLaunchLocation.worktreePath }
                    : {})}
                  {...(activeTerminalLaunchLocation.runtimeEnv
                    ? { runtimeEnv: activeTerminalLaunchLocation.runtimeEnv }
                    : {})}
                  {...(terminalCommandsById?.get(resolvedActiveTerminalId)
                    ? { command: terminalCommandsById.get(resolvedActiveTerminalId)! }
                    : {})}
                  centerPanel={owner === "center-panel"}
                  visible
                  onSessionExited={() => onCloseTerminal(resolvedActiveTerminalId)}
                  onAddTerminalContext={onAddTerminalContext}
                  focusRequestId={focusRequestId}
                  autoFocus={focusEligible}
                  resizeEpoch={0}
                  keybindings={keybindings}
                  workspaceUnavailable={workspaceUnavailable}
                />
              </div>
            )}
          </div>

          {hasTerminalSidebar && (
            <aside className="flex w-36 min-w-36 flex-col border border-border/70 bg-muted/10">
              {hasTerminalToolbarActions ? (
                <div
                  data-terminal-toolbar="sidebar"
                  className="flex h-[22px] items-stretch justify-end border-b border-panel-separator"
                >
                  <div className="inline-flex h-full items-stretch">
                    {onSplitTerminal ? (
                      <TerminalActionButton
                        className={`inline-flex h-full items-center px-1 text-foreground/90 transition-colors ${
                          hasReachedSplitLimit
                            ? "cursor-not-allowed opacity-45 hover:bg-transparent"
                            : "hover:bg-accent/70"
                        }`}
                        onClick={onSplitTerminalAction}
                        label={splitTerminalActionLabel}
                        disabledReason={workspaceUnavailable}
                      >
                        <SquareSplitHorizontal className="size-3.25" />
                      </TerminalActionButton>
                    ) : null}
                    {onSplitTerminalVertical ? (
                      <TerminalActionButton
                        className={`inline-flex h-full items-center px-1 text-foreground/90 transition-colors ${
                          onSplitTerminal ? "border-l border-border/70" : ""
                        } ${
                          hasReachedSplitLimit
                            ? "cursor-not-allowed opacity-45 hover:bg-transparent"
                            : "hover:bg-accent/70"
                        }`}
                        onClick={onSplitTerminalVerticalAction}
                        label={splitTerminalVerticalActionLabel}
                        disabledReason={workspaceUnavailable}
                      >
                        <SquareSplitVertical className="size-3.25" />
                      </TerminalActionButton>
                    ) : null}
                    {onNewTerminal ? (
                      <TerminalActionButton
                        className={`inline-flex h-full items-center px-1 text-foreground/90 transition-colors hover:bg-accent/70 ${
                          onSplitTerminal || onSplitTerminalVertical
                            ? "border-l border-border/70"
                            : ""
                        }`}
                        onClick={onNewTerminalAction}
                        label={newTerminalActionLabel}
                        disabledReason={workspaceUnavailable}
                      >
                        <Plus className="size-3.25" />
                      </TerminalActionButton>
                    ) : null}
                  </div>
                </div>
              ) : null}

              <div className="min-h-0 flex-1 overflow-y-auto px-1 py-1">
                {resolvedTerminalGroups.map((terminalGroup, groupIndex) => {
                  const isGroupActive =
                    terminalGroup.terminalIds.includes(resolvedActiveTerminalId);
                  const groupActiveTerminalId = isGroupActive
                    ? resolvedActiveTerminalId
                    : (terminalGroup.terminalIds[0] ?? resolvedActiveTerminalId);

                  return (
                    <div key={terminalGroup.id} className="pb-0.5">
                      {showGroupHeaders && (
                        <button
                          type="button"
                          className={`flex w-full items-center rounded px-1 py-0.5 text-xs uppercase tracking-[0.08em] ${
                            isGroupActive
                              ? "bg-accent/70 text-foreground"
                              : "text-muted-foreground hover:bg-accent/50 hover:text-foreground"
                          }`}
                          onClick={() => onActiveTerminalChange(groupActiveTerminalId)}
                        >
                          Group {groupIndex + 1}
                        </button>
                      )}

                      <div
                        className={showGroupHeaders ? "ml-1 border-l border-border/60 pl-1.5" : ""}
                      >
                        {terminalGroup.terminalIds.map((terminalId) => {
                          const isActive = terminalId === resolvedActiveTerminalId;
                          const closeTerminalLabel = `Close ${
                            terminalLabelById.get(terminalId) ?? "terminal"
                          }${isActive && closeShortcutLabel ? ` (${closeShortcutLabel})` : ""}`;
                          return (
                            <div
                              key={terminalId}
                              className={`group flex items-center gap-1 rounded px-1 py-0.5 text-xs ${
                                isActive
                                  ? "bg-accent text-foreground"
                                  : "text-muted-foreground hover:bg-accent/50 hover:text-foreground"
                              }`}
                            >
                              {showGroupHeaders && (
                                <span className="text-xs text-muted-foreground">└</span>
                              )}
                              <button
                                type="button"
                                className="flex min-w-0 flex-1 items-center gap-1 text-left"
                                onClick={() => onActiveTerminalChange(terminalId)}
                              >
                                <TerminalSquare className="size-3 shrink-0" />
                                <span className="truncate">
                                  {terminalLabelById.get(terminalId) ?? "Terminal"}
                                </span>
                              </button>
                              {normalizedTerminalIds.length > 1 && (
                                <Popover>
                                  <PopoverTrigger
                                    openOnHover
                                    render={
                                      <button
                                        type="button"
                                        className="inline-flex size-3.5 items-center justify-center rounded text-xs font-medium leading-none text-muted-foreground opacity-0 transition hover:bg-accent hover:text-foreground group-hover:opacity-100"
                                        onClick={() => onCloseTerminal(terminalId)}
                                        aria-label={closeTerminalLabel}
                                      />
                                    }
                                  >
                                    <XIcon className="size-2.5" />
                                  </PopoverTrigger>
                                  <PopoverPopup
                                    tooltipStyle
                                    side="bottom"
                                    sideOffset={6}
                                    align="center"
                                    className="pointer-events-none select-none"
                                  >
                                    {closeTerminalLabel}
                                  </PopoverPopup>
                                </Popover>
                              )}
                            </div>
                          );
                        })}
                      </div>
                    </div>
                  );
                })}
              </div>
            </aside>
          )}
        </div>
      </div>
    </aside>
  );
}
