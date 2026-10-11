import type { DesktopPreviewTabState } from "@bibcode/contracts";
import { Window } from "happy-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { supportsPreviewRuntimeCapability } from "~/previewRuntimeCapabilities";

import { createFramePreviewBridge, FRAME_UNSUPPORTED_DESCRIPTION } from "./framePreviewBridge";

const GATEWAY = "http://box.lan:41000";
const BOOTSTRAP = `${GATEWAY}/__bibcode/bootstrap?cap=C&to=%2F&ui=http%3A%2F%2Fbox.lan%3A3773`;

let window: Window;
let frameWindows: Map<HTMLIFrameElement, { postMessage: ReturnType<typeof vi.fn> }>;

// happy-dom loads no iframe pages; each frame gets a stub window to message.
function frameOf(iframe: HTMLIFrameElement) {
  let frame = frameWindows.get(iframe);
  if (!frame) {
    frame = { postMessage: vi.fn() };
    frameWindows.set(iframe, frame);
  }
  return frame;
}

/** The panel's slot for a tab, where its frame lives (`BrowserSurfaceSlot`). */
function slot(tabId: string) {
  const element = window.document.createElement("div");
  element.setAttribute("data-browser-surface-slot", tabId);
  window.document.body.append(element);
  return element as unknown as HTMLElement;
}

function makeBridge() {
  if (!window.document.querySelector('[data-browser-surface-slot="tab-1"]')) slot("tab-1");
  const bridge = createFramePreviewBridge({
    document: window.document as unknown as Document,
    window: window as unknown as globalThis.Window,
    frameWindow: (iframe) => frameOf(iframe) as unknown as globalThis.Window,
  });
  const states: Array<readonly [string, DesktopPreviewTabState]> = [];
  bridge.onStateChange((tabId, state) => states.push([tabId, state]));
  return { bridge, states };
}

const iframes = () =>
  [...window.document.querySelectorAll("iframe")] as unknown as HTMLIFrameElement[];

function report(iframe: HTMLIFrameElement, data: unknown, origin = GATEWAY) {
  const event = new window.MessageEvent("message", {
    data,
    origin,
    source: frameOf(iframe) as never,
  });
  window.dispatchEvent(event);
}

beforeEach(() => {
  window = new Window({
    url: "http://box.lan:3773/",
    settings: { disableIframePageLoading: true },
  });
  frameWindows = new Map();
});

afterEach(() => {
  window.close();
});

describe("createFramePreviewBridge", () => {
  it("hosts each tab in a sandboxed iframe inside its panel slot", async () => {
    const panel = slot("tab-1");
    const { bridge } = makeBridge();
    await bridge.createTab("tab-1");
    await bridge.setBounds("tab-1", { x: 10, y: 20, width: 300, height: 200 }, true);

    const [iframe] = iframes();
    expect(iframe?.parentElement).toBe(panel);
    expect(iframe?.getAttribute("sandbox")).toBe(
      "allow-scripts allow-same-origin allow-forms allow-popups allow-popups-to-escape-sandbox allow-modals allow-downloads",
    );
    expect(iframe?.getAttribute("allow")).toBe("clipboard-read; clipboard-write; fullscreen");
    expect(iframe?.getAttribute("referrerpolicy")).toBe("no-referrer");
    expect(iframe?.style.display).toBe("block");

    await bridge.setBounds("tab-1", { x: 10, y: 20, width: 300, height: 200 }, false);
    expect(iframe?.style.display).toBe("none");
  });

  it("shows a tab whose slot became visible before the tab existed", async () => {
    slot("tab-1");
    const { bridge } = makeBridge();
    // The panel slot measures (a layout effect) before the tab host creates the tab.
    await bridge.setBounds("tab-1", { x: 10, y: 20, width: 300, height: 200 }, true);
    await bridge.createTab("tab-1");
    expect(iframes()[0]?.style.display).toBe("block");
  });

  it("loads gateway bootstrap links and follows the page's navigation reports", async () => {
    const { bridge, states } = makeBridge();
    await bridge.createTab("tab-1");
    await bridge.navigate("tab-1", BOOTSTRAP);
    const [iframe] = iframes();
    expect(iframe?.getAttribute("src")).toBe(BOOTSTRAP);
    expect(states.at(-1)?.[1].navStatus).toEqual({ kind: "Loading", url: BOOTSTRAP, title: "" });

    // The frame's load event comes first; the page reports from its own load handler.
    iframe?.dispatchEvent(new window.Event("load") as unknown as Event);
    report(iframe!, {
      type: "bibcode-preview-frame",
      url: `${GATEWAY}/docs?x=1`,
      title: "Docs",
      canGoBack: true,
      canGoForward: false,
    });
    expect(states.at(-1)).toEqual([
      "tab-1",
      expect.objectContaining({
        navStatus: { kind: "Success", url: `${GATEWAY}/docs?x=1`, title: "Docs" },
        canGoBack: true,
        canGoForward: false,
      }),
    ]);
    // A later document that never reports (a link to a page whose policy blocks
    // the reporter) keeps the last known address but has no history controls.
    iframe?.dispatchEvent(new window.Event("load") as unknown as Event);
    expect(states.at(-1)?.[1]).toMatchObject({
      navStatus: { kind: "Success", url: `${GATEWAY}/docs?x=1`, title: "Docs" },
      canGoBack: false,
      canGoForward: false,
    });
  });

  it("ignores the replaced page's late reports while a navigation loads", async () => {
    const { bridge, states } = makeBridge();
    await bridge.createTab("tab-1");
    await bridge.navigate("tab-1", BOOTSTRAP);
    const [iframe] = iframes();
    iframe?.dispatchEvent(new window.Event("load") as unknown as Event);
    const page = {
      type: "bibcode-preview-frame",
      title: "A",
      canGoBack: false,
      canGoForward: false,
    };
    report(iframe!, { ...page, url: `${GATEWAY}/a` });

    await bridge.navigate("tab-1", BOOTSTRAP);
    report(iframe!, { ...page, url: `${GATEWAY}/a`, title: "A again" });
    expect(states.at(-1)?.[1].navStatus.kind).toBe("Loading");
  });

  it("settles a page that never reports five seconds after its frame loads", async () => {
    vi.useFakeTimers();
    try {
      const { bridge, states } = makeBridge();
      await bridge.createTab("tab-1");
      await bridge.navigate("tab-1", BOOTSTRAP);
      iframes()[0]?.dispatchEvent(new window.Event("load") as unknown as Event);
      // A reporting page settles from its own report, title and history included.
      expect(states.at(-1)?.[1].navStatus.kind).toBe("Loading");
      vi.advanceTimersByTime(5_000);
      expect(states.at(-1)?.[1].navStatus).toEqual({ kind: "Success", url: BOOTSTRAP, title: "" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("drops a previous page's history until the new page reports its own", async () => {
    const { bridge, states } = makeBridge();
    await bridge.createTab("tab-1");
    await bridge.navigate("tab-1", BOOTSTRAP);
    const [iframe] = iframes();
    iframe?.dispatchEvent(new window.Event("load") as unknown as Event);
    report(iframe!, {
      type: "bibcode-preview-frame",
      url: `${GATEWAY}/a`,
      title: "A",
      canGoBack: true,
      canGoForward: true,
    });

    await bridge.navigate("tab-1", BOOTSTRAP);
    expect(states.at(-1)?.[1]).toMatchObject({ canGoBack: false, canGoForward: false });
    // A page whose policy blocks the reporter never enables them.
    iframe?.dispatchEvent(new window.Event("load") as unknown as Event);
    expect(states.at(-1)?.[1]).toMatchObject({ canGoBack: false, canGoForward: false });
  });

  it("ignores messages from other frames, other origins, and other shapes", async () => {
    const { bridge, states } = makeBridge();
    await bridge.createTab("tab-1");
    await bridge.navigate("tab-1", BOOTSTRAP);
    const [iframe] = iframes();
    iframe?.dispatchEvent(new window.Event("load") as unknown as Event);
    const before = states.length;
    const valid = {
      type: "bibcode-preview-frame",
      url: `${GATEWAY}/x`,
      title: "X",
      canGoBack: false,
      canGoForward: false,
    };

    report(iframe!, valid, "http://box.lan:42000");
    report(iframe!, { ...valid, url: 42 });
    // The page writes its own reports: only its own gateway origin is believed.
    report(iframe!, { ...valid, url: "javascript:alert(document.domain)" });
    report(iframe!, { ...valid, url: "http://box.lan:3773/" });
    report(iframe!, { type: "other" });
    window.dispatchEvent(
      new window.MessageEvent("message", { data: valid, origin: GATEWAY, source: {} as never }),
    );
    expect(states.length).toBe(before);
  });

  it("hands the page's links to the server's localhost to BiBCode as new tabs", async () => {
    const { bridge } = makeBridge();
    const opened: Array<[string, string]> = [];
    bridge.onNewWindowRequest((tabId, url) => opened.push([tabId, url]));
    await bridge.createTab("tab-1");
    await bridge.navigate("tab-1", BOOTSTRAP);
    const [iframe] = iframes();
    // An interactive page can be clicked before its frame's load event (a slow image).
    report(iframe!, { type: "bibcode-preview-open", url: "http://localhost:8123/docs" });
    report(iframe!, { type: "bibcode-preview-open", url: "javascript:alert(1)" });
    report(
      iframe!,
      { type: "bibcode-preview-open", url: "http://localhost:1/" },
      "http://box.lan:42000",
    );
    expect(opened).toEqual([["tab-1", "http://localhost:8123/docs"]]);
  });

  it("sends Back, Forward, and Reload to the page's own origin", async () => {
    const { bridge } = makeBridge();
    await bridge.createTab("tab-1");
    await bridge.navigate("tab-1", BOOTSTRAP);
    const frame = frameOf(iframes()[0]!);

    await bridge.goBack("tab-1");
    await bridge.goForward("tab-1");
    await bridge.refresh("tab-1");
    expect(frame?.postMessage.mock.calls).toEqual([
      [{ type: "bibcode-preview-command", command: "back" }, GATEWAY],
      [{ type: "bibcode-preview-command", command: "forward" }, GATEWAY],
      [{ type: "bibcode-preview-command", command: "reload" }, GATEWAY],
    ]);
  });

  it.each([
    ["another site", "https://example.com/"],
    // The gateway lets only a page on its own host frame it.
    ["another host's gateway", "http://other.lan:41000/__bibcode/bootstrap?cap=C&to=%2F"],
  ])("explains %s instead of showing a blank page", async (_, url) => {
    const { bridge, states } = makeBridge();
    await bridge.createTab("tab-1");
    await bridge.navigate("tab-1", url);
    expect(iframes()[0]?.getAttribute("src")).toBeNull();
    expect(states.at(-1)?.[1].navStatus).toEqual({
      kind: "LoadFailed",
      url,
      title: "",
      code: 0,
      description: FRAME_UNSUPPORTED_DESCRIPTION,
    });
  });

  it("stops following the previous page once an address is refused", async () => {
    const { bridge, states } = makeBridge();
    await bridge.createTab("tab-1");
    await bridge.navigate("tab-1", BOOTSTRAP);
    const [iframe] = iframes();
    iframe?.dispatchEvent(new window.Event("load") as unknown as Event);
    const page = {
      type: "bibcode-preview-frame",
      title: "A",
      canGoBack: true,
      canGoForward: false,
    };
    report(iframe!, { ...page, url: `${GATEWAY}/a` });

    await bridge.navigate("tab-1", "https://example.com/");
    expect(states.at(-1)?.[1]).toMatchObject({ canGoBack: false, canGoForward: false });
    // The old page changing its title must not hide the refusal.
    report(iframe!, { ...page, url: `${GATEWAY}/a`, title: "A2" });
    expect(states.at(-1)?.[1].navStatus.kind).toBe("LoadFailed");
    await bridge.goBack("tab-1");
    expect(frameOf(iframe!).postMessage).not.toHaveBeenCalled();
  });

  it("follows its slot into a new panel, reopening the page it showed", async () => {
    const inline = slot("tab-1");
    const { bridge } = makeBridge();
    await bridge.createTab("tab-1");
    await bridge.navigate("tab-1", BOOTSTRAP);
    const [iframe] = iframes();
    iframe?.dispatchEvent(new window.Event("load") as unknown as Event);
    report(iframe!, {
      type: "bibcode-preview-frame",
      url: `${GATEWAY}/docs`,
      title: "Docs",
      canGoBack: false,
      canGoForward: false,
    });

    // The window narrows: the panel moves into the sheet, and its slot with it.
    inline.remove();
    const sheet = slot("tab-1");
    await bridge.setBounds("tab-1", { x: 0, y: 0, width: 300, height: 200 }, true);
    expect(iframe?.parentElement).toBe(sheet);
    // Moving a frame reloads it; the single-use bootstrap link would have expired.
    expect(iframe?.getAttribute("src")).toBe(`${GATEWAY}/docs`);
  });

  it("reopens the opened address when a page that never reported moves", async () => {
    const inline = slot("tab-1");
    const { bridge } = makeBridge();
    await bridge.createTab("tab-1");
    await bridge.navigate("tab-1", BOOTSTRAP);
    iframes()[0]?.dispatchEvent(new window.Event("load") as unknown as Event);

    inline.remove();
    const sheet = slot("tab-1");
    await bridge.setBounds("tab-1", { x: 0, y: 0, width: 300, height: 200 }, true);
    const [iframe] = [...sheet.querySelectorAll("iframe")];
    // The bootstrap's own target, which the gateway session (its cookie) still opens.
    expect(iframe?.getAttribute("src")).toBe(`${GATEWAY}/`);
  });

  it("keeps a bootstrap that hasn't loaded when its frame moves", async () => {
    const inline = slot("tab-1");
    const { bridge } = makeBridge();
    await bridge.createTab("tab-1");
    await bridge.navigate("tab-1", BOOTSTRAP);

    // No gateway cookie yet: the target page would answer with the expired page.
    inline.remove();
    const sheet = slot("tab-1");
    await bridge.setBounds("tab-1", { x: 0, y: 0, width: 300, height: 200 }, true);
    expect(sheet.querySelector("iframe")?.getAttribute("src")).toBe(BOOTSTRAP);
  });

  it("removes a closed tab's iframe and stops listening for it", async () => {
    const { bridge, states } = makeBridge();
    await bridge.createTab("tab-1");
    await bridge.navigate("tab-1", BOOTSTRAP);
    const [iframe] = iframes();
    await bridge.closeTab("tab-1");
    expect(iframes()).toHaveLength(0);
    const before = states.length;
    report(iframe!, {
      type: "bibcode-preview-frame",
      url: `${GATEWAY}/late`,
      title: "",
      canGoBack: false,
      canGoForward: false,
    });
    expect(states.length).toBe(before);
  });

  it("advertises no native-only preview tools", () => {
    const { bridge } = makeBridge();
    for (const capability of [
      "automation",
      "picker",
      "recording",
      "imageClipboard",
      "screenshot",
      "pageTools",
    ] as const) {
      expect(supportsPreviewRuntimeCapability(bridge, capability)).toBe(false);
    }
  });
});
