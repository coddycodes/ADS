import { describe, expect, it, vi } from "vite-plus/test";

import { invokeTauriCommand } from "./tauriInvokeRouting";

describe("invokeTauriCommand", () => {
  it("routes an E2E command through its registered mock before native Tauri", async () => {
    const args = { options: { title: "Select Folder" } };
    const e2eMock = vi.fn(() => "/tmp/coddy-studio-ui-project");
    const globalInvoke = vi.fn(async () => "global");
    const importedInvoke = vi.fn(async () => "imported");

    await expect(
      invokeTauriCommand<string>({
        command: "desktop_bridge_pick_folder",
        args,
        e2eMock,
        globalInvoke,
        importedInvoke,
      }),
    ).resolves.toBe("/tmp/coddy-studio-ui-project");

    expect(e2eMock).toHaveBeenCalledExactlyOnceWith(args);
    expect(globalInvoke).not.toHaveBeenCalled();
    expect(importedInvoke).not.toHaveBeenCalled();
  });

  it("uses the global Tauri invoke when no E2E mock is registered", async () => {
    const globalInvoke = vi.fn(async () => "global");
    const importedInvoke = vi.fn(async () => "imported");

    await expect(
      invokeTauriCommand<string>({
        command: "desktop_bridge_get_client_settings",
        globalInvoke,
        importedInvoke,
      }),
    ).resolves.toBe("global");

    expect(globalInvoke).toHaveBeenCalledExactlyOnceWith(
      "desktop_bridge_get_client_settings",
      undefined,
    );
    expect(importedInvoke).not.toHaveBeenCalled();
  });

  it("falls back to the imported Tauri API when the global API is unavailable", async () => {
    const importedInvoke = vi.fn(async () => "imported");

    await expect(
      invokeTauriCommand<string>({
        command: "desktop_bridge_get_client_settings",
        importedInvoke,
      }),
    ).resolves.toBe("imported");

    expect(importedInvoke).toHaveBeenCalledExactlyOnceWith(
      "desktop_bridge_get_client_settings",
      undefined,
    );
  });
});
