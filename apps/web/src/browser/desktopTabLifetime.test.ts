import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const { closeTab, createTab, previewNavigate } = vi.hoisted(() => ({
  closeTab: vi.fn(async () => undefined),
  createTab: vi.fn<() => Promise<void>>(),
  previewNavigate: vi.fn(async () => undefined),
}));

vi.mock("~/components/preview/previewBridge", () => ({
  previewBridge: { closeTab, createTab, navigate: previewNavigate },
}));

import * as desktopTabLifetime from "./desktopTabLifetime";

const { acquireDesktopTab } = desktopTabLifetime;

describe("desktopTabLifetime", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubGlobal("window", { setTimeout, clearTimeout });
    closeTab.mockClear();
    createTab.mockClear();
    previewNavigate.mockClear();
  });

  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("creates the native tab in its environment's preview storage", async () => {
    createTab.mockResolvedValueOnce(undefined).mockResolvedValueOnce(undefined);
    const remote = acquireDesktopTab("tab_remote", "env-remote");
    await remote.ready;
    const local = acquireDesktopTab("tab_local", null);
    await local.ready;

    expect(createTab).toHaveBeenCalledWith("tab_remote", "env-remote");
    expect(createTab).toHaveBeenCalledWith("tab_local", null);
    remote.release();
    local.release();
    await vi.runAllTimersAsync();
  });

  it("shares tab creation readiness across concurrent leases", async () => {
    let resolveCreation: (() => void) | undefined;
    createTab.mockReturnValueOnce(
      new Promise<void>((resolve) => {
        resolveCreation = resolve;
      }),
    );

    const first = acquireDesktopTab("tab_readiness", null);
    const second = acquireDesktopTab("tab_readiness", null);

    expect(createTab).toHaveBeenCalledOnce();
    expect(first.ready).toBe(second.ready);

    let ready = false;
    void first.ready.then(() => {
      ready = true;
    });
    await Promise.resolve();
    expect(ready).toBe(false);

    resolveCreation?.();
    await first.ready;
    expect(ready).toBe(true);
  });

  it("closes a tab only after the last lease releases", async () => {
    const first = acquireDesktopTab("tab_shared", null);
    const second = acquireDesktopTab("tab_shared", null);
    await first.ready;

    first.release();
    await vi.runAllTimersAsync();
    expect(closeTab).not.toHaveBeenCalled();

    second.release();
    await vi.runAllTimersAsync();
    expect(closeTab).toHaveBeenCalledWith("tab_shared");

    second.release();
    await vi.runAllTimersAsync();
    expect(closeTab).toHaveBeenCalledOnce();
  });

  it("cancels a pending close when the tab is acquired again", async () => {
    const first = acquireDesktopTab("tab_reacquired", null);
    first.release();
    const second = acquireDesktopTab("tab_reacquired", null);

    await vi.runAllTimersAsync();
    expect(closeTab).not.toHaveBeenCalled();
    expect(second.ready).toBe(first.ready);

    second.release();
    await vi.runAllTimersAsync();
    expect(closeTab).toHaveBeenCalledWith("tab_reacquired");
  });

  it("waits for shared creation readiness before interactive navigation", async () => {
    let resolveCreation: (() => void) | undefined;
    createTab.mockReturnValueOnce(
      new Promise<void>((resolve) => {
        resolveCreation = resolve;
      }),
    );
    const owner = acquireDesktopTab("tab_interactive", null);
    const navigateDesktopTab = Reflect.get(desktopTabLifetime, "navigateDesktopTab") as
      | ((tabId: string, partition: string | null, url: string) => Promise<void>)
      | undefined;

    expect(navigateDesktopTab).toEqual(expect.any(Function));
    const navigation = navigateDesktopTab!("tab_interactive", null, "https://interactive.test/");
    await Promise.resolve();
    expect(previewNavigate).not.toHaveBeenCalled();

    resolveCreation?.();
    await navigation;
    expect(previewNavigate).toHaveBeenCalledExactlyOnceWith(
      "tab_interactive",
      "https://interactive.test/",
    );

    owner.release();
    await vi.runAllTimersAsync();
  });

  it("treats each acquired lease release as idempotent", async () => {
    const first = acquireDesktopTab("tab_idempotent", null);
    const second = acquireDesktopTab("tab_idempotent", null);
    await first.ready;

    first.release();
    first.release();
    await vi.runAllTimersAsync();
    expect(closeTab).not.toHaveBeenCalled();

    second.release();
    await vi.runAllTimersAsync();
    expect(closeTab).toHaveBeenCalledExactlyOnceWith("tab_idempotent");
  });

  it("retries a rejected creation generation instead of poisoning later navigation", async () => {
    createTab.mockRejectedValueOnce(new Error("create boom")).mockResolvedValueOnce(undefined);
    const owner = acquireDesktopTab("tab_create_retry", null);
    await expect(owner.ready).rejects.toThrow("create boom");
    const navigateDesktopTab = Reflect.get(desktopTabLifetime, "navigateDesktopTab") as (
      tabId: string,
      partition: string | null,
      url: string,
    ) => Promise<void>;

    await navigateDesktopTab("tab_create_retry", null, "https://recovered.test/");

    expect(createTab).toHaveBeenCalledTimes(2);
    expect(previewNavigate).toHaveBeenCalledExactlyOnceWith(
      "tab_create_retry",
      "https://recovered.test/",
    );
    owner.release();
    await vi.runAllTimersAsync();
  });

  it("releases its temporary lease when navigation rejects", async () => {
    previewNavigate.mockRejectedValueOnce(new Error("navigate boom"));
    const navigateDesktopTab = Reflect.get(desktopTabLifetime, "navigateDesktopTab") as (
      tabId: string,
      partition: string | null,
      url: string,
    ) => Promise<void>;

    await expect(
      navigateDesktopTab("tab_navigation_failure", null, "https://failure.test/"),
    ).rejects.toThrow("navigate boom");
    await vi.runAllTimersAsync();

    expect(closeTab).toHaveBeenCalledExactlyOnceWith("tab_navigation_failure");
  });
});
