// @vitest-environment happy-dom

import { type AssetCreateUrlResult, EnvironmentId, ThreadId } from "@bibcode/contracts";
import { scopeThreadRef } from "@bibcode/client-runtime/environment";
import {
  AVAILABLE_CONNECTION_STATE,
  PrimaryConnectionTarget,
} from "@bibcode/client-runtime/connection";
import * as Cause from "effect/Cause";
import * as Option from "effect/Option";
import { AsyncResult, Atom, AtomRegistry } from "effect/unstable/reactivity";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  applyPreviewServerSnapshot: vi.fn(),
  openBrowser: vi.fn(),
  previewSupported: true,
  rememberPreviewUrl: vi.fn(),
}));

vi.mock("./previewStateStore", () => ({
  applyPreviewServerSnapshot: h.applyPreviewServerSnapshot,
  isPreviewSupportedInRuntime: () => h.previewSupported,
  canPreviewAnyUrlInRuntime: () => h.previewSupported,
  rememberPreviewUrl: h.rememberPreviewUrl,
}));

vi.mock("./rightPanelStore", () => ({
  useRightPanelStore: {
    getState: () => ({ openBrowser: h.openBrowser }),
  },
}));

import {
  BrowserPreviewUnavailableError,
  isBrowserPreviewFile,
  openFileInPreview,
  openUrlInPreview,
} from "./browser/openFileInPreview";
import { readPreviewAnnotationTheme } from "./browser/annotationTheme";
import { installFileEditorDismissal } from "./components/files/fileEditorDismissal";
import { describePreviewError } from "./components/preview/errorCodeMessages";
import {
  dispatchPreviewAction,
  subscribePreviewAction,
} from "./components/preview/previewActionBus";
import { isPreviewFocused } from "./lib/previewFocus";
import { createEnvironmentPresentationAtoms } from "@bibcode/client-runtime/state/presentation";
import type { AtomCommandResult } from "@bibcode/client-runtime/state/runtime";
import type { SupervisorConnectionState } from "@bibcode/client-runtime/connection";

const environmentId = EnvironmentId.make("environment-local");
const threadId = ThreadId.make("thread-1");
const threadRef = scopeThreadRef(environmentId, threadId);

beforeEach(() => {
  h.applyPreviewServerSnapshot.mockReset();
  h.openBrowser.mockReset();
  h.previewSupported = true;
  h.rememberPreviewUrl.mockReset();
  document.body.replaceChildren();
  document.documentElement.className = "";
  document.documentElement.removeAttribute("style");
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  document.body.replaceChildren();
  document.documentElement.className = "";
  document.documentElement.removeAttribute("style");
});

describe("browser preview opening", () => {
  const snapshot = {
    threadId,
    tabId: "tab-1",
    navStatus: AsyncResult.initial(false),
    canGoBack: false,
    canGoForward: false,
    updatedAt: "2026-07-16T00:00:00.000Z",
  };

  it("recognizes HTML/PDF paths without accepting unrelated extensions", () => {
    expect(isBrowserPreviewFile("index.html")).toBe(true);
    expect(isBrowserPreviewFile("INDEX.HTM?raw=1")).toBe(true);
    expect(isBrowserPreviewFile("docs/report.PDF#page=2")).toBe(true);
    expect(isBrowserPreviewFile("src/index.ts")).toBe(false);
    expect(isBrowserPreviewFile("report.pdf.txt")).toBe(false);
  });

  it("opens URL snapshots and projects their state into the preview stores", async () => {
    const openPreview = vi.fn(async () => AsyncResult.success(snapshot as never));
    const result = await openUrlInPreview({
      threadRef,
      url: "https://example.test/docs",
      openPreview,
    });

    expect(result._tag).toBe("Success");
    expect(openPreview).toHaveBeenCalledWith({
      environmentId,
      input: { threadId, url: "https://example.test/docs" },
    });
    expect(h.applyPreviewServerSnapshot).toHaveBeenCalledWith(threadRef, snapshot);
    expect(h.rememberPreviewUrl).toHaveBeenCalledWith(threadRef, "https://example.test/docs");
    expect(h.openBrowser).toHaveBeenCalledWith(threadRef, "tab-1");
  });

  it("preserves URL-open failures without mutating preview stores", async () => {
    const failure = new Error("preview rejected");
    const result = await openUrlInPreview({
      threadRef,
      url: "https://example.test/docs",
      openPreview: async () => AsyncResult.failure(Cause.fail(failure)),
    });

    expect(result._tag).toBe("Failure");
    expect(h.applyPreviewServerSnapshot).not.toHaveBeenCalled();
    expect(h.openBrowser).not.toHaveBeenCalled();
  });

  it("handles unavailable runtime, asset failures, invalid URLs, and success", async () => {
    let assetResult: AtomCommandResult<AssetCreateUrlResult, Error> = AsyncResult.success({
      relativeUrl: "/assets/file.html",
      expiresAt: 0,
    });
    const createAssetUrl = vi.fn(async () => assetResult);
    const openPreview = vi.fn(async () => AsyncResult.success(snapshot as never));

    h.previewSupported = false;
    const unavailable = await openFileInPreview({
      threadRef,
      filePath: "file.html",
      httpBaseUrl: "https://example.test",
      createAssetUrl,
      openPreview,
    });
    expect(unavailable._tag).toBe("Failure");
    if (unavailable._tag !== "Failure") throw new Error("expected unavailable preview failure");
    expect(Cause.squash(unavailable.cause)).toBeInstanceOf(BrowserPreviewUnavailableError);
    expect(createAssetUrl).not.toHaveBeenCalled();

    h.previewSupported = true;
    const assetFailure = new Error("asset failed");
    assetResult = AsyncResult.failure(Cause.fail(assetFailure)) as AtomCommandResult<
      AssetCreateUrlResult,
      Error
    >;
    const failed = await openFileInPreview({
      threadRef,
      filePath: "file.html",
      httpBaseUrl: "https://example.test",
      createAssetUrl,
      openPreview,
    });
    expect(failed._tag).toBe("Failure");
    if (failed._tag !== "Failure") throw new Error("expected asset creation failure");
    expect(Cause.squash(failed.cause)).toBe(assetFailure);

    assetResult = AsyncResult.success({ relativeUrl: "http://[", expiresAt: 0 });
    const invalid = await openFileInPreview({
      threadRef,
      filePath: "file.html",
      httpBaseUrl: "https://example.test",
      createAssetUrl,
      openPreview,
    });
    expect(invalid._tag).toBe("Failure");

    assetResult = AsyncResult.success({ relativeUrl: "/assets/file.html", expiresAt: 0 });
    const success = await openFileInPreview({
      threadRef,
      filePath: "folder/file.html",
      httpBaseUrl: "https://example.test/base/",
      createAssetUrl,
      openPreview,
    });
    expect(success._tag).toBe("Success");
    expect(createAssetUrl).toHaveBeenLastCalledWith({
      environmentId,
      input: {
        resource: { _tag: "workspace-file", threadId, path: "folder/file.html" },
      },
    });
    expect(openPreview).toHaveBeenLastCalledWith({
      environmentId,
      input: { threadId, url: "https://example.test/assets/file.html" },
    });
  });
});

describe("preview browser UI helpers", () => {
  it("reads CSS variables, fallbacks, and light/dark schemes", () => {
    const light = readPreviewAnnotationTheme();
    expect(light.colorScheme).toBe("light");
    expect(light.radius).toBe("0.625rem");
    expect(light.fontSans).toBeTruthy();

    document.documentElement.classList.add("dark");
    document.documentElement.style.setProperty("--radius", "1rem");
    document.documentElement.style.setProperty("--primary", "blue");
    document.documentElement.style.setProperty("--font-mono", "Test Mono");
    const dark = readPreviewAnnotationTheme();
    expect(dark).toMatchObject({
      colorScheme: "dark",
      radius: "1rem",
      primary: "blue",
      fontMono: "Test Mono",
    });
  });

  it("maps known descriptions and uses description/code fallbacks", () => {
    expect(describePreviewError(-105, "ERR_NAME_NOT_RESOLVED")).toBe(
      "DNS address could not be found",
    );
    expect(describePreviewError(-1, "Custom failure")).toBe("Custom failure");
    expect(describePreviewError(-7, "")).toBe("Network error (-7)");
  });

  it("dispatches and unsubscribes typed preview actions while rejecting non-string details", () => {
    const listener = vi.fn();
    const unsubscribe = subscribePreviewAction(listener);
    dispatchPreviewAction("refresh");
    window.dispatchEvent(new CustomEvent("bibcode:preview-action", { detail: 42 }));
    expect(listener).toHaveBeenCalledOnce();
    expect(listener).toHaveBeenCalledWith("refresh");
    unsubscribe();
    dispatchPreviewAction("zoom-in");
    expect(listener).toHaveBeenCalledOnce();

    const actualWindow = window;
    vi.stubGlobal("window", undefined);
    expect(() => dispatchPreviewAction("focus-url")).not.toThrow();
    expect(() => subscribePreviewAction(listener)()).not.toThrow();
    vi.stubGlobal("window", actualWindow);
  });

  it("detects preview-panel and webview focus with disconnected/non-element guards", () => {
    const plain = document.createElement("button");
    document.body.append(plain);
    plain.focus();
    expect(isPreviewFocused()).toBe(false);

    const panel = document.createElement("div");
    panel.setAttribute("data-preview-panel-mode", "embedded");
    const input = document.createElement("input");
    panel.append(input);
    document.body.append(panel);
    input.focus();
    expect(isPreviewFocused()).toBe(true);

    const webview = document.createElement("webview");
    webview.tabIndex = 0;
    document.body.append(webview);
    webview.focus();
    expect(isPreviewFocused()).toBe(true);

    const detached = document.createElement("button");
    vi.spyOn(document, "activeElement", "get")
      .mockReturnValueOnce(detached)
      .mockReturnValueOnce(null);
    expect(isPreviewFocused()).toBe(false);
    expect(isPreviewFocused()).toBe(false);
  });
});

describe("file editor dismissal", () => {
  it("dismisses outside clicks and focused Escape while respecting guards and cleanup", () => {
    const root = document.createElement("div");
    const file = document.createElement("diffs-container");
    const shadow = file.attachShadow({ mode: "open" });
    const editorContent = document.createElement("button");
    editorContent.setAttribute("data-content", "");
    shadow.append(editorContent);
    root.append(file);
    document.body.append(root);
    const outside = document.createElement("button");
    document.body.append(outside);

    let blocked = true;
    const onDismiss = vi.fn();
    const setSelections = vi.fn();
    const blur = vi.spyOn(editorContent, "blur");
    const cleanup = installFileEditorDismissal({
      root,
      editor: { setSelections },
      isBlocked: () => blocked,
      onDismiss,
    });

    outside.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, composed: true }));
    expect(onDismiss).not.toHaveBeenCalled();
    blocked = false;
    root.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, composed: true }));
    expect(onDismiss).not.toHaveBeenCalled();
    outside.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, composed: true }));
    expect(onDismiss).toHaveBeenCalledOnce();
    expect(setSelections).toHaveBeenCalledWith([]);

    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(onDismiss).toHaveBeenCalledOnce();
    editorContent.focus();
    vi.spyOn(shadow, "activeElement", "get").mockReturnValue(editorContent);
    blocked = true;
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(onDismiss).toHaveBeenCalledOnce();
    blocked = false;
    const escape = new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
    });
    document.dispatchEvent(escape);
    expect(escape.defaultPrevented).toBe(true);
    expect(onDismiss).toHaveBeenCalledTimes(2);
    expect(blur).toHaveBeenCalledOnce();

    cleanup();
    outside.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, composed: true }));
    expect(onDismiss).toHaveBeenCalledTimes(2);
  });
});

describe("environment presentation atoms", () => {
  function entry(label: string) {
    return {
      target: new PrimaryConnectionTarget({
        environmentId,
        label,
        httpBaseUrl: "http://127.0.0.1:4321",
        wsBaseUrl: "ws://127.0.0.1:4321/ws",
      }),
      profile: Option.none(),
    };
  }

  it("projects catalog/state/config data and preserves stable aggregate maps", () => {
    const catalogAtom = Atom.make({
      isReady: true,
      entries: new Map([[environmentId, entry("Local")]]),
    });
    const stateAtom = Atom.make(AsyncResult.success(AVAILABLE_CONNECTION_STATE));
    const configAtom = Atom.make<unknown>(null);
    const atoms = createEnvironmentPresentationAtoms({
      catalogValueAtom: catalogAtom,
      stateAtom: () => stateAtom,
      serverConfigValueAtom: () => configAtom as never,
    });
    const registry = AtomRegistry.make();

    expect(registry.get(atoms.presentationAtom(EnvironmentId.make("missing")))).toBeNull();
    expect(registry.get(atoms.presentationAtom(environmentId))).toMatchObject({
      entry: { target: { label: "Local" } },
      connection: { phase: "available" },
      serverConfig: null,
    });
    const first = registry.get(atoms.presentationsAtom);
    expect(first.get(environmentId)?.entry.target.label).toBe("Local");
    expect(registry.get(atoms.presentationsAtom)).toBe(first);

    registry.set(configAtom, { providers: [] } as never);
    const withConfig = registry.get(atoms.presentationsAtom);
    expect(withConfig).not.toBe(first);
    expect(withConfig.get(environmentId)?.serverConfig).toEqual({ providers: [] });

    registry.set(catalogAtom, { isReady: true, entries: new Map() });
    expect(registry.get(atoms.presentationsAtom).size).toBe(0);
    registry.dispose();
  });

  it("falls back to the available state when an async state has no value", () => {
    const catalogAtom = Atom.make({
      isReady: true,
      entries: new Map([[environmentId, entry("Fallback")]]),
    });
    const stateAtom = Atom.make(
      AsyncResult.failure(Cause.fail(new Error("offline"))) as AsyncResult.AsyncResult<
        SupervisorConnectionState,
        Error
      >,
    );
    const configAtom = Atom.make(null);
    const atoms = createEnvironmentPresentationAtoms({
      catalogValueAtom: catalogAtom,
      stateAtom: () => stateAtom,
      serverConfigValueAtom: () => configAtom,
    });
    const registry = AtomRegistry.make();
    expect(registry.get(atoms.presentationAtom(environmentId))?.connection.phase).toBe("available");
    registry.dispose();
  });
});
