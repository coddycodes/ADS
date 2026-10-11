import { scopeThreadRef } from "@bibcode/client-runtime/environment";
import { type EnvironmentId, ThreadId } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import * as Option from "effect/Option";
import { AsyncResult } from "effect/unstable/reactivity";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Window } from "happy-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const mocks = vi.hoisted(() => ({
  openEditor: vi.fn(),
  openFile: vi.fn(),
  openFileInPreview: vi.fn(),
  openUrlInPreview: vi.fn(),
  contextMenuShow: vi.fn(),
  openExternal: vi.fn(),
  toastAdd: vi.fn(),
  openLink: vi.fn(),
  showFileOutsideWorkspaceNotice: vi.fn(),
  copyLink: vi.fn(),
  browserLinkTarget: "app" as "app" | "system",
  previewSupported: true,
  localApiAvailable: true,
}));

vi.mock("@effect/atom-react", () => ({ useAtomValue: () => ({ availableEditors: [] }) }));
vi.mock("../hooks/useTheme", () => ({ useTheme: () => ({ resolvedTheme: "dark" }) }));
vi.mock("../editorPreferences", () => ({ useOpenInPreferredEditor: () => mocks.openEditor }));
vi.mock("../state/entities", () => ({ useActiveEnvironmentId: () => "env-1" }));
vi.mock("../state/server", () => ({
  serverEnvironment: { configValueAtom: () => ({}) },
}));
vi.mock("../state/assets", () => ({ assetEnvironment: { createUrl: {} } }));
vi.mock("../state/preview", () => ({ previewEnvironment: { open: {} } }));
vi.mock("../state/use-atom-query-runner", () => ({ useAtomQueryRunner: () => vi.fn() }));
vi.mock("../state/use-atom-command", () => ({ useAtomCommand: () => vi.fn() }));
vi.mock("../state/session", () => ({
  usePreparedConnection: () => Option.some({ httpBaseUrl: "https://server.test/" }),
}));
vi.mock("../rightPanelStore", () => ({
  useRightPanelStore: { getState: () => ({ openFile: mocks.openFile }) },
}));
vi.mock("../previewStateStore", () => ({
  isPreviewSupportedInRuntime: () => mocks.previewSupported,
  canPreviewAnyUrlInRuntime: () => mocks.previewSupported,
}));
vi.mock("../browser/openFileInPreview", () => ({
  BrowserPreviewUnavailableError: class BrowserPreviewUnavailableError extends Error {
    constructor(input: { message: string }) {
      super(input.message);
    }
  },
  isBrowserPreviewFile: () => true,
  openFileInPreview: mocks.openFileInPreview,
  openUrlInPreview: mocks.openUrlInPreview,
}));
vi.mock("../browser/openLink", () => ({ openLink: mocks.openLink }));
vi.mock("../browser/linkNotices", () => ({
  showFileOutsideWorkspaceNotice: mocks.showFileOutsideWorkspaceNotice,
  copyLink: mocks.copyLink,
}));
vi.mock("../hooks/useSettings", () => ({
  getClientSettings: () => ({ wordWrap: false, browserLinkTarget: mocks.browserLinkTarget }),
}));
vi.mock("../localApi", () => ({
  readLocalApi: () =>
    mocks.localApiAvailable
      ? {
          contextMenu: { show: mocks.contextMenuShow },
          shell: { openExternal: mocks.openExternal },
        }
      : undefined,
}));
vi.mock("./ui/toast", () => ({
  stackedThreadToast: (value: unknown) => value,
  toastManager: { add: mocks.toastAdd },
}));

import ChatMarkdown from "./ChatMarkdown";

const threadRef = scopeThreadRef("env-1" as EnvironmentId, ThreadId.make("thread-1"));
let windowInstance: Window;
let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeEach(() => {
  windowInstance = new Window({ url: "https://bibcode.test/" });
  vi.stubGlobal("window", windowInstance);
  vi.stubGlobal("document", windowInstance.document);
  vi.stubGlobal("navigator", windowInstance.navigator);
  vi.stubGlobal("localStorage", windowInstance.localStorage);
  vi.stubGlobal("Node", windowInstance.Node);
  vi.stubGlobal("Element", windowInstance.Element);
  vi.stubGlobal("HTMLElement", windowInstance.HTMLElement);
  vi.stubGlobal("HTMLInputElement", windowInstance.HTMLInputElement);
  vi.stubGlobal("HTMLButtonElement", windowInstance.HTMLButtonElement);
  vi.stubGlobal("Event", windowInstance.Event);
  vi.stubGlobal("MouseEvent", windowInstance.MouseEvent);
  vi.stubGlobal("KeyboardEvent", windowInstance.KeyboardEvent);
  vi.stubGlobal("PointerEvent", windowInstance.PointerEvent);
  vi.stubGlobal("CustomEvent", windowInstance.CustomEvent);
  vi.stubGlobal("customElements", windowInstance.customElements);
  vi.stubGlobal("DOMParser", windowInstance.DOMParser);
  vi.stubGlobal("MutationObserver", windowInstance.MutationObserver);
  vi.stubGlobal("ResizeObserver", windowInstance.ResizeObserver);
  vi.stubGlobal("getComputedStyle", windowInstance.getComputedStyle.bind(windowInstance));
  vi.stubGlobal("requestAnimationFrame", windowInstance.requestAnimationFrame.bind(windowInstance));
  vi.stubGlobal("cancelAnimationFrame", windowInstance.cancelAnimationFrame.bind(windowInstance));
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  Object.defineProperty(windowInstance.HTMLElement.prototype, "getAnimations", {
    configurable: true,
    value: () => [],
  });
  mocks.previewSupported = true;
  mocks.localApiAvailable = true;
  mocks.browserLinkTarget = "app";
  mocks.openLink.mockReset().mockReturnValue("app");
  mocks.showFileOutsideWorkspaceNotice.mockReset();
  mocks.openEditor.mockReset().mockResolvedValue(AsyncResult.success(undefined));
  mocks.openFileInPreview.mockReset().mockResolvedValue(AsyncResult.success(undefined));
  mocks.openUrlInPreview.mockReset().mockResolvedValue(AsyncResult.success(undefined));
  mocks.contextMenuShow.mockReset().mockResolvedValue(undefined);
  mocks.openExternal.mockReset().mockResolvedValue(undefined);
  mocks.openFile.mockReset();
  mocks.toastAdd.mockReset();
});

afterEach(async () => {
  if (root) {
    await act(async () => root?.unmount());
  }
  container?.remove();
  root = null;
  container = null;
  windowInstance.close();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function mountFileLink(withThread = true): Promise<HTMLAnchorElement> {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () =>
    root?.render(
      <ChatMarkdown
        text="[index.ts](/workspace/src/index.ts#L12)"
        cwd="/workspace"
        {...(withThread ? { threadRef } : {})}
      />,
    ),
  );
  return container.querySelector<HTMLAnchorElement>(".chat-markdown-file-link")!;
}

async function mountExternalLink(withThread = true): Promise<HTMLAnchorElement> {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () =>
    root?.render(
      <ChatMarkdown
        text="[Documentation](https://example.test/docs)"
        cwd="/workspace"
        {...(withThread ? { threadRef } : {})}
      />,
    ),
  );
  return container.querySelector<HTMLAnchorElement>('a[href="https://example.test/docs"]')!;
}

async function mountMailtoLink(): Promise<HTMLAnchorElement> {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () =>
    root?.render(
      <ChatMarkdown
        text="[Email](mailto:hi@example.test)"
        cwd="/workspace"
        threadRef={threadRef}
      />,
    ),
  );
  return container.querySelector<HTMLAnchorElement>('a[href="mailto:hi@example.test"]')!;
}

async function openContextMenu(link: HTMLAnchorElement): Promise<void> {
  await act(async () =>
    link.dispatchEvent(
      new MouseEvent("contextmenu", {
        bubbles: true,
        cancelable: true,
        clientX: 14,
        clientY: 28,
      }),
    ),
  );
  await flush();
}

async function flush(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe("ChatMarkdown file-link behavior", () => {
  it("opens browser-preview-capable files from a normal click", async () => {
    const link = await mountFileLink();

    await act(async () => link.click());
    await flush();

    expect(mocks.openFileInPreview).toHaveBeenCalledWith(
      expect.objectContaining({ threadRef, filePath: "/workspace/src/index.ts" }),
    );
    expect(mocks.openEditor).not.toHaveBeenCalled();
  });

  it("falls back to the file panel or preferred editor when preview is unavailable", async () => {
    mocks.previewSupported = false;
    const link = await mountFileLink();
    await act(async () => link.click());
    expect(mocks.openFile).toHaveBeenCalledWith(threadRef, "src/index.ts", 12);

    await act(async () => root?.unmount());
    root = null;
    (container as HTMLDivElement | null)?.remove();
    container = null;
    const editorLink = await mountFileLink(false);
    await act(async () => editorLink.click());
    await flush();
    expect(mocks.openEditor).toHaveBeenCalledWith("/workspace/src/index.ts:12");
  });

  it("reports failed and thrown editor opens", async () => {
    mocks.previewSupported = false;
    const link = await mountFileLink(false);
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    mocks.openEditor.mockResolvedValueOnce(AsyncResult.failure(Cause.fail(new Error("denied"))));
    await act(async () => link.click());
    await flush();
    expect(mocks.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Unable to open file", description: "denied" }),
    );

    mocks.openEditor.mockRejectedValueOnce("boom");
    await act(async () => link.click());
    await flush();
    expect(mocks.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Unable to open file", description: "An error occurred." }),
    );
    expect(consoleError).toHaveBeenCalled();

    mocks.openEditor.mockResolvedValueOnce(AsyncResult.failure(Cause.fail("unknown")));
    await act(async () => link.click());
    await flush();
    expect(mocks.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Unable to open file", description: "An error occurred." }),
    );
  });

  it("runs every file context-menu action and handles unavailable native APIs", async () => {
    const link = await mountFileLink();
    const dispatch = async () => {
      await act(async () =>
        link.dispatchEvent(
          new MouseEvent("contextmenu", {
            bubbles: true,
            cancelable: true,
            clientX: 10,
            clientY: 20,
          }),
        ),
      );
      await flush();
    };

    mocks.contextMenuShow.mockResolvedValueOnce("open");
    await dispatch();
    expect(mocks.openEditor).toHaveBeenCalled();

    mocks.contextMenuShow.mockResolvedValueOnce("open-in-browser");
    await dispatch();
    expect(mocks.openFileInPreview).toHaveBeenCalled();

    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    mocks.contextMenuShow.mockResolvedValueOnce("copy-relative");
    await dispatch();
    mocks.contextMenuShow.mockResolvedValueOnce("copy-full");
    await dispatch();
    expect(writeText).toHaveBeenCalledWith("workspace/src/index.ts:12");
    expect(writeText).toHaveBeenCalledWith("/workspace/src/index.ts:12");

    mocks.localApiAvailable = false;
    await dispatch();
    expect(mocks.contextMenuShow).toHaveBeenCalledTimes(4);
  });

  it("reports context-menu and clipboard failures with safe fallbacks", async () => {
    const link = await mountFileLink();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    const dispatch = async () => {
      await act(async () =>
        link.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true })),
      );
      await flush();
    };

    mocks.contextMenuShow.mockRejectedValueOnce(new Error("menu failed"));
    await dispatch();
    expect(consoleError).toHaveBeenCalledWith(
      "[chat-markdown] action failed",
      expect.objectContaining({ operation: "show-file-context-menu" }),
      expect.any(Error),
    );

    Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
    mocks.contextMenuShow.mockResolvedValueOnce("copy-relative");
    await dispatch();
    expect(mocks.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Failed to copy relative path" }),
    );

    const failure = new Error("clipboard failed");
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: vi.fn(async () => Promise.reject(failure)) },
    });
    mocks.contextMenuShow.mockResolvedValueOnce("copy-full");
    await dispatch();
    expect(mocks.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Failed to copy full path",
        description: "clipboard failed",
      }),
    );
  });

  it("reports browser preview failures and ignores successful external-link opens", async () => {
    const link = await mountFileLink();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    mocks.openFileInPreview.mockResolvedValueOnce(
      AsyncResult.failure(Cause.fail(new Error("preview unavailable"))),
    );
    await act(async () => link.click());
    await flush();
    expect(mocks.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Unable to open file in browser" }),
    );

    mocks.contextMenuShow.mockResolvedValueOnce("open-in-browser");
    mocks.openFileInPreview.mockRejectedValueOnce("preview threw");
    await act(async () =>
      link.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true })),
    );
    await flush();
    expect(mocks.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Unable to open file in browser" }),
    );
    expect(consoleError).toHaveBeenCalled();

    mocks.openFileInPreview.mockResolvedValueOnce(AsyncResult.failure(Cause.fail("unknown")));
    await act(async () => link.click());
    await flush();
    expect(mocks.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Unable to open file in browser",
        description: "An error occurred.",
      }),
    );
  });

  it("opens the editor instead of the browser for a modified click on a previewable file", async () => {
    const link = await mountFileLink();
    await act(async () =>
      link.dispatchEvent(
        new MouseEvent("click", { bubbles: true, cancelable: true, button: 0, ctrlKey: true }),
      ),
    );
    await flush();
    expect(mocks.openEditor).toHaveBeenCalledWith("/workspace/src/index.ts:12");
    expect(mocks.openFileInPreview).not.toHaveBeenCalled();
  });

  it("explains files outside the workspace and offers the editor", async () => {
    const link = await mountFileLink();
    mocks.openFileInPreview.mockResolvedValueOnce(
      AsyncResult.failure(Cause.fail({ _tag: "AssetWorkspacePathValidationError" })),
    );
    await act(async () => link.click());
    await flush();

    expect(mocks.showFileOutsideWorkspaceNotice).toHaveBeenCalledOnce();
    expect(mocks.toastAdd).not.toHaveBeenCalled();

    expect(mocks.openEditor).not.toHaveBeenCalled();
    mocks.showFileOutsideWorkspaceNotice.mock.calls[0]![0].onOpenInEditor();
    await flush();
    expect(mocks.openEditor).toHaveBeenCalledWith("/workspace/src/index.ts:12");
  });
});

describe("ChatMarkdown external-link behavior", () => {
  const url = "https://example.test/docs";

  it("routes a primary click through the link router without inverting", async () => {
    const link = await mountExternalLink();
    const click = new MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });

    expect(link.target).toBe("_blank");
    expect(link.rel).toBe("noopener noreferrer");

    await act(async () => link.dispatchEvent(click));

    expect(click.defaultPrevented).toBe(true);
    expect(mocks.openLink).toHaveBeenCalledOnce();
    expect(mocks.openLink).toHaveBeenCalledWith(
      expect.objectContaining({ url, threadRef, invert: false, onError: expect.any(Function) }),
    );
  });

  it("inverts the destination for modified clicks", async () => {
    const link = await mountExternalLink();

    for (const init of [
      { ctrlKey: true },
      { metaKey: true },
      { shiftKey: true },
      { altKey: true },
    ]) {
      mocks.openLink.mockClear();
      const click = new MouseEvent("click", {
        bubbles: true,
        cancelable: true,
        button: 0,
        ...init,
      });
      await act(async () => link.dispatchEvent(click));
      expect(click.defaultPrevented).toBe(true);
      expect(mocks.openLink).toHaveBeenCalledWith(expect.objectContaining({ url, invert: true }));
    }
  });

  it("inverts the destination for a middle click", async () => {
    const link = await mountExternalLink();
    const auxclick = new MouseEvent("auxclick", { bubbles: true, cancelable: true, button: 1 });

    await act(async () => link.dispatchEvent(auxclick));

    expect(auxclick.defaultPrevented).toBe(true);
    expect(mocks.openLink).toHaveBeenCalledWith(expect.objectContaining({ url, invert: true }));

    mocks.openLink.mockClear();
    await act(async () =>
      link.dispatchEvent(
        new MouseEvent("auxclick", { bubbles: true, cancelable: true, button: 2 }),
      ),
    );
    expect(mocks.openLink).not.toHaveBeenCalled();
  });

  it("routes links without a thread so the router can use the system browser", async () => {
    const link = await mountExternalLink(false);
    await act(async () => link.click());
    expect(mocks.openLink).toHaveBeenCalledWith(
      expect.objectContaining({ url, threadRef: null, invert: false }),
    );
  });

  it("keeps same-document fragment links out of the router", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () =>
      root?.render(
        <ChatMarkdown
          text={"[jump](#target)\n\n<a id='target'></a>"}
          cwd="/workspace"
          threadRef={threadRef}
        />,
      ),
    );
    const link = container.querySelector<HTMLAnchorElement>('a[href="#target"]')!;
    await act(async () => link.click());
    expect(mocks.openLink).not.toHaveBeenCalled();
  });

  it("reports router errors and an unavailable local API with operation context", async () => {
    const link = await mountExternalLink();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});

    const cause = new Error("preview rejected");
    await act(async () => link.click());
    const onError = mocks.openLink.mock.calls[0]![0].onError as (cause: unknown) => void;
    onError(cause);
    expect(consoleError).toHaveBeenCalledWith(
      "[chat-markdown] action failed",
      { operation: "open-link", target: url },
      cause,
    );

    consoleError.mockClear();
    mocks.openLink.mockReturnValueOnce("unavailable");
    await act(async () => link.click());
    expect(consoleError).toHaveBeenCalledWith(
      "[chat-markdown] action failed",
      { operation: "open-link", target: url },
      expect.any(Error),
    );
  });

  it("does not route non-HTTP schemes", async () => {
    const link = await mountMailtoLink();
    const click = new MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });
    await act(async () => link.dispatchEvent(click));
    expect(click.defaultPrevented).toBe(false);
    expect(mocks.openLink).not.toHaveBeenCalled();

    await openContextMenu(link);
    expect(mocks.contextMenuShow).not.toHaveBeenCalled();
  });

  it("maps the native menu items through the browserLinkTarget setting", async () => {
    const link = await mountExternalLink();

    mocks.contextMenuShow.mockResolvedValueOnce("open-in-browser");
    await openContextMenu(link);
    expect(mocks.contextMenuShow).toHaveBeenCalledWith(
      [
        { id: "open-in-browser", label: "Open in BiBCode browser" },
        { id: "open-external", label: "Open in system browser" },
        { id: "copy-link", label: "Copy link" },
      ],
      { x: 14, y: 28 },
    );
    expect(mocks.openLink).toHaveBeenLastCalledWith(
      expect.objectContaining({ url, threadRef, invert: false }),
    );

    mocks.contextMenuShow.mockResolvedValueOnce("open-external");
    await openContextMenu(link);
    expect(mocks.openLink).toHaveBeenLastCalledWith(expect.objectContaining({ url, invert: true }));

    mocks.browserLinkTarget = "system";
    mocks.contextMenuShow.mockResolvedValueOnce("open-in-browser");
    await openContextMenu(link);
    expect(mocks.openLink).toHaveBeenLastCalledWith(expect.objectContaining({ url, invert: true }));

    mocks.contextMenuShow.mockResolvedValueOnce("open-external");
    await openContextMenu(link);
    expect(mocks.openLink).toHaveBeenLastCalledWith(
      expect.objectContaining({ url, invert: false }),
    );

    const calls = mocks.openLink.mock.calls.length;
    mocks.contextMenuShow.mockResolvedValueOnce(undefined);
    await openContextMenu(link);
    expect(mocks.openLink).toHaveBeenCalledTimes(calls);

    mocks.contextMenuShow.mockResolvedValueOnce("copy-link");
    await openContextMenu(link);
    expect(mocks.copyLink).toHaveBeenCalledWith(url);
    expect(mocks.openLink).toHaveBeenCalledTimes(calls);
  });

  it("reports context-menu failures with operation context", async () => {
    const link = await mountExternalLink();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});

    mocks.contextMenuShow.mockRejectedValueOnce(new Error("menu unavailable"));
    await openContextMenu(link);
    expect(consoleError).toHaveBeenCalledWith(
      "[chat-markdown] action failed",
      { operation: "show-link-context-menu", target: url },
      expect.any(Error),
    );

    consoleError.mockClear();
    mocks.openLink.mockReturnValueOnce("unavailable");
    mocks.contextMenuShow.mockResolvedValueOnce("open-external");
    await openContextMenu(link);
    expect(consoleError).toHaveBeenCalledWith(
      "[chat-markdown] action failed",
      { operation: "open-link", target: url },
      expect.any(Error),
    );
  });

  it("does not intercept external-link context menus without all preview capabilities", async () => {
    mocks.localApiAvailable = false;
    const withoutApi = await mountExternalLink();
    await openContextMenu(withoutApi);
    expect(mocks.contextMenuShow).not.toHaveBeenCalled();

    await act(async () => root?.unmount());
    root = null;
    (container as HTMLDivElement | null)?.remove();
    container = null;
    mocks.localApiAvailable = true;
    mocks.previewSupported = false;
    const withoutPreview = await mountExternalLink();
    await openContextMenu(withoutPreview);
    expect(mocks.contextMenuShow).not.toHaveBeenCalled();

    await act(async () => root?.unmount());
    root = null;
    (container as HTMLDivElement | null)?.remove();
    container = null;
    mocks.previewSupported = true;
    const withoutThread = await mountExternalLink(false);
    await openContextMenu(withoutThread);
    expect(mocks.contextMenuShow).not.toHaveBeenCalled();
  });
});
