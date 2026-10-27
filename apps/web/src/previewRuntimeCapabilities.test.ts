import type { DesktopPreviewBridge } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import { supportsPreviewRuntimeCapability } from "./previewRuntimeCapabilities";

describe("previewRuntimeCapabilities", () => {
  it("defaults unmarked generic bridges to supporting deferred capabilities", () => {
    const bridge = {} as DesktopPreviewBridge;

    expect(supportsPreviewRuntimeCapability(bridge, "picker")).toBe(true);
    expect(supportsPreviewRuntimeCapability(bridge, "recording")).toBe(true);
    expect(supportsPreviewRuntimeCapability(bridge, "automation")).toBe(true);
    expect(supportsPreviewRuntimeCapability(bridge, "imageClipboard")).toBe(true);
  });
});
