// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

// The bridge is chosen when the module first loads; each case loads it fresh.
async function loadPreviewBridge(protocol: string) {
  vi.resetModules();
  vi.stubGlobal("location", { protocol, hostname: "box.lan", origin: `${protocol}//box.lan:3773` });
  return (await import("./previewBridge")).previewBridge;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
  document.body.replaceChildren();
});

describe("previewBridge", () => {
  it("frames gateway previews in a browser tab served over plain HTTP", async () => {
    const bridge = await loadPreviewBridge("http:");
    expect(bridge).not.toBeNull();
    const slot = document.createElement("div");
    slot.setAttribute("data-browser-surface-slot", "tab-1");
    document.body.append(slot);
    await bridge!.createTab("tab-1");
    const { acquireBrowserSurface } = await import("~/browser/browserSurfaceStore");
    // The panel slot's presentation reaches the frame without a desktop host.
    acquireBrowserSurface("tab-1").present({ x: 4, y: 8, width: 300, height: 200 }, true);
    await Promise.resolve();
    const iframe = document.querySelector("iframe");
    expect(iframe?.parentElement).toBe(slot);
    expect(iframe?.style.display).toBe("block");
  });

  it("has no preview host on an HTTPS page, which can't frame the HTTP gateway", async () => {
    expect(await loadPreviewBridge("https:")).toBeNull();
  });

  it("uses the desktop's native views when there is a desktop host", async () => {
    const preview = {};
    window.desktopBridge = { preview } as never;
    try {
      expect(await loadPreviewBridge("http:")).toBe(preview);
    } finally {
      delete window.desktopBridge;
    }
  });
});
