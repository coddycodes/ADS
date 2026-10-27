import { describe, expect, it, vi } from "vite-plus/test";

import { dispatchDesktopUiApplicationExit, isFinalDesktopUiSpec } from "./app-lifecycle.ts";

describe("dispatchDesktopUiApplicationExit", () => {
  it("waits for native runtime shutdown before closing the main Tauri window", async () => {
    let acceptShutdown = (): void => {
      throw new Error("Shutdown acceptance was not initialized.");
    };
    const shutdownAccepted = new Promise<unknown>((resolve) => {
      acceptShutdown = () => resolve(undefined);
    });
    const invoke = vi
      .fn<(command: string, args?: Record<string, unknown>) => Promise<unknown>>()
      .mockImplementationOnce(() => shutdownAccepted)
      .mockResolvedValueOnce(undefined);

    const exitRequest = dispatchDesktopUiApplicationExit({
      __TAURI__: { core: { invoke } },
    });
    expect(invoke).toHaveBeenCalledExactlyOnceWith("desktop_e2e_prepare_for_exit");

    acceptShutdown();
    await expect(exitRequest).resolves.toBe(true);
    expect(invoke).toHaveBeenNthCalledWith(2, "plugin:window|close", { label: "main" });
  });

  it("reports that no native close request was dispatched without the Tauri bridge", async () => {
    await expect(dispatchDesktopUiApplicationExit({})).resolves.toBe(false);
  });
});

describe("isFinalDesktopUiSpec", () => {
  const configuredSpecs = ["./specs/main-window.e2e.ts", "./specs/chat-activity-panel.e2e.ts"];

  it("defers shutdown while later configured specs still need the shared application", () => {
    expect(
      isFinalDesktopUiSpec(
        ["file:///X:/CoddyStudio/apps/desktop/e2e/specs/main-window.e2e.ts"],
        configuredSpecs,
      ),
    ).toBe(false);
  });

  it("selects the final configured spec across WebdriverIO path formats", () => {
    expect(
      isFinalDesktopUiSpec(
        [String.raw`X:\CoddyStudio\apps\desktop\e2e\specs\chat-activity-panel.e2e.ts`],
        configuredSpecs,
      ),
    ).toBe(true);
  });
});
