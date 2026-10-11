import type { DesktopPreviewBridge, DesktopPreviewTabState } from "@bibcode/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

import { TauriDesktopCapabilityUnsupportedError } from "./tauriDesktopBridge";
import { createTauriPreviewBridge } from "./tauriPreviewBridge";
import { supportsPreviewRuntimeCapability } from "./previewRuntimeCapabilities";

const screenshotArtifact = {
  id: "shot-1",
  tabId: "t1",
  path: "/tmp/shot-1.png",
  mimeType: "image/png" as const,
  sizeBytes: 42,
  createdAt: "2026-07-20T00:00:00.000Z",
};

function makeBridge() {
  const invoke = vi
    .fn()
    .mockImplementation((command: string) =>
      Promise.resolve(
        command === "desktop_preview_capture_screenshot" ? screenshotArtifact : undefined,
      ),
    );
  const listeners = new Map<string, Set<(payload: unknown) => void>>();
  const listen = <T>(event: string, cb: (payload: T) => void) => {
    const eventListeners = listeners.get(event) ?? new Set<(payload: unknown) => void>();
    const listener = cb as (payload: unknown) => void;
    eventListeners.add(listener);
    listeners.set(event, eventListeners);
    return () => {
      eventListeners.delete(listener);
      if (eventListeners.size === 0) listeners.delete(event);
    };
  };
  const emit = (event: string, payload: unknown) => {
    for (const listener of listeners.get(event) ?? []) listener(payload);
  };
  return {
    bridge: createTauriPreviewBridge({ invoke, listen }),
    emit,
    invoke,
    listenerCount: (event: string) => listeners.get(event)?.size ?? 0,
  };
}

function statePayload(tabId: string, zoomFactor = 1, url = "https://example.com/") {
  const state: DesktopPreviewTabState = {
    tabId,
    webContentsId: null,
    navStatus: { kind: "Success", url, title: "Example" },
    canGoBack: false,
    canGoForward: false,
    zoomFactor,
    controller: "human",
    updatedAt: "2026-07-20T00:00:00.000Z",
  };
  return { tabId, state };
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

interface CommandCase {
  readonly name: string;
  readonly run: (bridge: DesktopPreviewBridge) => Promise<unknown>;
  readonly command: string;
  readonly args: Record<string, unknown>;
  readonly result?: unknown;
}

const commandCases: readonly CommandCase[] = [
  {
    name: "createTab",
    run: (bridge) => bridge.createTab("t1"),
    command: "desktop_preview_create_tab",
    args: { tabId: "t1" },
  },
  {
    name: "closeTab",
    run: (bridge) => bridge.closeTab("t1"),
    command: "desktop_preview_close_tab",
    args: { tabId: "t1" },
  },
  {
    name: "setBounds",
    run: (bridge) => bridge.setBounds("t1", { x: 1, y: 2, width: 3, height: 4 }, true),
    command: "desktop_preview_set_bounds",
    args: { tabId: "t1", bounds: { x: 1, y: 2, width: 3, height: 4 }, visible: true },
  },
  {
    name: "navigate",
    run: (bridge) => bridge.navigate("t1", "https://example.com"),
    command: "desktop_preview_navigate",
    args: { tabId: "t1", url: "https://example.com" },
  },
  {
    name: "goBack",
    run: (bridge) => bridge.goBack("t1"),
    command: "desktop_preview_go_back",
    args: { tabId: "t1" },
  },
  {
    name: "goForward",
    run: (bridge) => bridge.goForward("t1"),
    command: "desktop_preview_go_forward",
    args: { tabId: "t1" },
  },
  {
    name: "refresh",
    run: (bridge) => bridge.refresh("t1"),
    command: "desktop_preview_refresh",
    args: { tabId: "t1" },
  },
  {
    name: "hardReload",
    run: (bridge) => bridge.hardReload("t1"),
    command: "desktop_preview_hard_reload",
    args: { tabId: "t1" },
  },
  {
    name: "zoomIn",
    run: (bridge) => bridge.zoomIn("t1"),
    command: "desktop_preview_set_zoom",
    args: { tabId: "t1", factor: 1.1 },
  },
  {
    name: "zoomOut",
    run: (bridge) => bridge.zoomOut("t1"),
    command: "desktop_preview_set_zoom",
    args: { tabId: "t1", factor: 0.9 },
  },
  {
    name: "resetZoom",
    run: (bridge) => bridge.resetZoom("t1"),
    command: "desktop_preview_set_zoom",
    args: { tabId: "t1", factor: 1 },
  },
  {
    name: "openDevTools",
    run: (bridge) => bridge.openDevTools("t1"),
    command: "desktop_preview_open_devtools",
    args: { tabId: "t1" },
  },
  {
    name: "clearCookies",
    run: (bridge) => bridge.clearCookies("t1"),
    command: "desktop_preview_clear_data",
    args: { tabId: "t1", cookies: true, cache: false, storage: true },
  },
  {
    name: "clearCache",
    run: (bridge) => bridge.clearCache("t1"),
    command: "desktop_preview_clear_data",
    args: { tabId: "t1", cookies: false, cache: true, storage: false },
  },
  {
    name: "captureScreenshot",
    run: (bridge) => bridge.captureScreenshot("t1"),
    command: "desktop_preview_capture_screenshot",
    args: { tabId: "t1" },
    result: screenshotArtifact,
  },
  {
    name: "revealArtifact",
    run: (bridge) => bridge.revealArtifact("/tmp/shot-1.png"),
    command: "desktop_preview_reveal_artifact",
    args: { path: "/tmp/shot-1.png" },
  },
];

describe("tauriPreviewBridge", () => {
  it("reuses one native host across logical browser tabs", async () => {
    const { bridge, invoke } = makeBridge();

    await bridge.createTab("logical-a");
    await bridge.closeTab("logical-a");
    await bridge.createTab("logical-b");
    await bridge.navigate("logical-b", "https://b.test/");

    expect(
      invoke.mock.calls.filter(([command]) => command === "desktop_preview_create_tab"),
    ).toEqual([["desktop_preview_create_tab", { tabId: "logical-a" }]]);
    expect(invoke.mock.calls.some(([command]) => command === "desktop_preview_close_tab")).toBe(
      false,
    );
    expect(invoke).toHaveBeenLastCalledWith("desktop_preview_navigate", {
      tabId: "logical-a",
      url: "https://b.test/",
    });
  });

  it("keeps one long-lived native host per environment's preview storage", async () => {
    const { bridge, invoke } = makeBridge();

    await bridge.createTab("local-a");
    await bridge.closeTab("local-a");
    await bridge.createTab("remote-a", "env-remote");
    await bridge.closeTab("remote-a");
    await bridge.createTab("remote-b", "env-remote");
    await bridge.closeTab("remote-b");
    await bridge.createTab("local-b", null);

    expect(
      invoke.mock.calls.filter(([command]) => command === "desktop_preview_create_tab"),
    ).toEqual([
      ["desktop_preview_create_tab", { tabId: "local-a" }],
      ["desktop_preview_create_tab", { tabId: "remote-a", environmentId: "env-remote" }],
    ]);
    expect(invoke.mock.calls.some(([command]) => command === "desktop_preview_close_tab")).toBe(
      false,
    );
  });

  it("never shares the local environment's storage with a falsy environment id", async () => {
    const { bridge, invoke } = makeBridge();
    await bridge.createTab("local-a");
    await bridge.closeTab("local-a");
    await bridge.createTab("odd-a", "");

    expect(
      invoke.mock.calls.filter(([command]) => command === "desktop_preview_create_tab"),
    ).toEqual([
      ["desktop_preview_create_tab", { tabId: "local-a" }],
      ["desktop_preview_create_tab", { tabId: "odd-a", environmentId: "" }],
    ]);
  });

  it("routes each logical tab through its own environment's native host", async () => {
    const { bridge, invoke, emit } = makeBridge();
    const received: Array<readonly [string, string]> = [];
    bridge.onStateChange((tabId, state) => {
      if (state.navStatus.kind !== "Idle") received.push([tabId, state.navStatus.url]);
    });
    await bridge.createTab("local-a");
    await bridge.closeTab("local-a");
    await bridge.createTab("remote-a", "env-remote");
    invoke.mockClear();

    await bridge.refresh("remote-a");
    expect(invoke.mock.calls).toEqual([["desktop_preview_refresh", { tabId: "remote-a" }]]);

    // The hidden local host shows no tab, so its events go nowhere.
    emit("preview://state", statePayload("local-a", 1, "https://local.test/"));
    emit("preview://state", statePayload("remote-a", 1, "https://remote.test/"));
    expect(received).toEqual([["remote-a", "https://remote.test/"]]);

    await bridge.closeTab("remote-a");
    await bridge.createTab("local-b");
    invoke.mockClear();
    await bridge.refresh("local-b");
    expect(invoke.mock.calls).toEqual([["desktop_preview_refresh", { tabId: "local-a" }]]);
  });

  it("clears the preview storage of the tab's own environment", async () => {
    const { bridge, invoke } = makeBridge();
    await bridge.createTab("local-a");
    await bridge.closeTab("local-a");
    await bridge.createTab("remote-a", "env-remote");
    await bridge.closeTab("remote-a");
    await bridge.createTab("remote-b", "env-remote");
    invoke.mockClear();

    await bridge.clearCookies("remote-b");

    expect(invoke.mock.calls).toEqual([
      [
        "desktop_preview_clear_data",
        { tabId: "remote-a", cookies: true, cache: false, storage: true },
      ],
    ]);
  });

  it("gives a new environment host the latest bounds set while it was created", async () => {
    const { bridge, invoke } = makeBridge();
    const early = { x: 5, y: 6, width: 400, height: 250 };
    const late = { x: 7, y: 8, width: 500, height: 300 };
    await bridge.createTab("local-a");
    await bridge.closeTab("local-a");
    await bridge.setBounds("remote-a", early, true);
    let finishCreate!: () => void;
    invoke.mockImplementation((command: string) =>
      command === "desktop_preview_create_tab"
        ? new Promise<void>((resolve) => {
            finishCreate = resolve;
          })
        : Promise.resolve(undefined),
    );
    const creating = bridge.createTab("remote-a", "env-remote");
    await Promise.resolve();
    // A resize or modal occlusion lands while the native host is being created.
    await bridge.setBounds("remote-a", late, false);
    finishCreate();
    await creating;

    expect(invoke).toHaveBeenLastCalledWith("desktop_preview_set_bounds", {
      tabId: "remote-a",
      bounds: late,
      visible: false,
    });
  });

  it("gives a new environment host the bounds its tab already has", async () => {
    const { bridge, invoke } = makeBridge();
    const bounds = { x: 5, y: 6, width: 400, height: 250 };
    await bridge.createTab("local-a");
    await bridge.closeTab("local-a");
    await bridge.setBounds("remote-a", bounds, true);
    await bridge.createTab("remote-a", "env-remote");

    expect(invoke).toHaveBeenLastCalledWith("desktop_preview_set_bounds", {
      tabId: "remote-a",
      bounds,
      visible: true,
    });
  });

  it("reports reused native host state under the active logical tab", async () => {
    const { bridge, emit } = makeBridge();
    const received: Array<readonly [string, DesktopPreviewTabState]> = [];
    bridge.onStateChange((tabId, state) => received.push([tabId, state]));
    await bridge.createTab("logical-a");
    await bridge.closeTab("logical-a");
    await bridge.createTab("logical-b");

    emit("preview://state", statePayload("logical-a", 1, "https://b.test/"));

    expect(received.filter(([, state]) => state.navStatus.kind !== "Idle")).toEqual([
      [
        "logical-b",
        { ...statePayload("logical-a", 1, "https://b.test/").state, tabId: "logical-b" },
      ],
    ]);
  });

  it("reports reused native host new-window requests under the active logical tab", async () => {
    const { bridge, emit, listenerCount } = makeBridge();
    const received: Array<readonly [string, string]> = [];
    const stop = bridge.onNewWindowRequest((tabId, url) => received.push([tabId, url]));
    await bridge.createTab("tab_a");
    // Another thread's logical tab becomes active on the same native child.
    await bridge.createTab("tab_b");

    emit("preview://new-window", { tabId: "tab_a", url: "https://popup.test/" });
    expect(received).toEqual([["tab_b", "https://popup.test/"]]);

    await bridge.closeTab("tab_b");
    emit("preview://new-window", { tabId: "tab_a", url: "https://ignored.test/" });
    expect(received).toHaveLength(1);

    stop();
    expect(listenerCount("preview://new-window")).toBe(0);
  });

  it("hides and repositions the native host while switching logical tabs", async () => {
    const { bridge, invoke } = makeBridge();
    const firstBounds = { x: 1, y: 2, width: 300, height: 200 };
    const secondBounds = { x: 5, y: 6, width: 400, height: 250 };
    await bridge.createTab("logical-a");
    await bridge.setBounds("logical-a", firstBounds, true);
    await bridge.closeTab("logical-a");
    await bridge.setBounds("logical-b", secondBounds, true);
    await bridge.createTab("logical-b");

    expect(
      invoke.mock.calls.filter(([command]) => command === "desktop_preview_set_bounds"),
    ).toEqual([
      ["desktop_preview_set_bounds", { tabId: "logical-a", bounds: firstBounds, visible: true }],
      ["desktop_preview_set_bounds", { tabId: "logical-a", bounds: firstBounds, visible: false }],
      ["desktop_preview_set_bounds", { tabId: "logical-a", bounds: secondBounds, visible: true }],
    ]);
  });

  it("routes active logical tab commands through the reused native host", async () => {
    const { bridge, invoke } = makeBridge();
    await bridge.createTab("logical-a");
    await bridge.closeTab("logical-a");
    await bridge.createTab("logical-b");
    invoke.mockClear();

    await bridge.goBack("logical-b");
    await bridge.goForward("logical-b");
    await bridge.refresh("logical-b");
    await bridge.hardReload("logical-b");
    await bridge.zoomIn("logical-b");
    await bridge.openDevTools("logical-b");
    await bridge.captureScreenshot("logical-b");

    expect(invoke.mock.calls).toEqual([
      ["desktop_preview_go_back", { tabId: "logical-a" }],
      ["desktop_preview_go_forward", { tabId: "logical-a" }],
      ["desktop_preview_refresh", { tabId: "logical-a" }],
      ["desktop_preview_hard_reload", { tabId: "logical-a" }],
      ["desktop_preview_set_zoom", { tabId: "logical-a", factor: 1.1 }],
      ["desktop_preview_open_devtools", { tabId: "logical-a" }],
      ["desktop_preview_capture_screenshot", { tabId: "logical-a" }],
    ]);
  });

  it("does not reset native zoom while activating a reused logical tab", async () => {
    const { bridge, invoke } = makeBridge();
    await bridge.createTab("logical-a");
    await bridge.zoomIn("logical-a");
    await bridge.closeTab("logical-a");
    await bridge.createTab("logical-b");

    expect(invoke.mock.calls.filter(([command]) => command === "desktop_preview_set_zoom")).toEqual(
      [["desktop_preview_set_zoom", { tabId: "logical-a", factor: 1.1 }]],
    );
  });

  it("attributes reused native host screenshots to the active logical tab", async () => {
    const { bridge } = makeBridge();
    await bridge.createTab("logical-a");
    await bridge.closeTab("logical-a");
    await bridge.createTab("logical-b");

    const artifact = await bridge.captureScreenshot("logical-b");

    expect(artifact).toEqual({ ...screenshotArtifact, tabId: "logical-b" });
  });

  it("reports image clipboard unsupported while retaining its rejecting stub", async () => {
    const { bridge } = makeBridge();

    await expect(bridge.copyArtifactToClipboard("/tmp/shot-1.png")).rejects.toMatchObject({
      code: "tauri_capability_unsupported",
    });
    expect(supportsPreviewRuntimeCapability(bridge, "imageClipboard")).toBe(false);
  });

  it("reports deferred picker, recording, and automation capabilities as unsupported", () => {
    const { bridge } = makeBridge();

    expect(supportsPreviewRuntimeCapability(bridge, "picker")).toBe(false);
    expect(supportsPreviewRuntimeCapability(bridge, "recording")).toBe(false);
    expect(supportsPreviewRuntimeCapability(bridge, "automation")).toBe(false);
  });

  it.each(commandCases)(
    "maps $name to its exact desktop_preview command and payload",
    async (entry) => {
      const { bridge, invoke } = makeBridge();
      const result = await entry.run(bridge);

      expect(invoke).toHaveBeenCalledTimes(1);
      expect(invoke).toHaveBeenCalledWith(entry.command, entry.args);
      if ("result" in entry) expect(result).toEqual(entry.result);
    },
  );

  it("clamps zoom factors to the host bounds", async () => {
    const lower = makeBridge();
    for (let index = 0; index < 20; index += 1) await lower.bridge.zoomOut("t1");
    expect(lower.invoke).toHaveBeenLastCalledWith("desktop_preview_set_zoom", {
      tabId: "t1",
      factor: 0.25,
    });

    const upper = makeBridge();
    for (let index = 0; index < 30; index += 1) await upper.bridge.zoomIn("t1");
    expect(upper.invoke).toHaveBeenLastCalledWith("desktop_preview_set_zoom", {
      tabId: "t1",
      factor: 3,
    });
  });

  it("fans state events out through one native listener and honors each unsubscribe", () => {
    const { bridge, emit, listenerCount } = makeBridge();
    const first: string[] = [];
    const second: string[] = [];
    const unsubscribeFirst = bridge.onStateChange((tabId) => first.push(tabId));
    const unsubscribeSecond = bridge.onStateChange((tabId) => second.push(tabId));

    expect(listenerCount("preview://state")).toBe(1);
    emit("preview://state", statePayload("t1"));
    unsubscribeFirst();
    emit("preview://state", statePayload("t2"));

    expect(first).toEqual(["t1"]);
    expect(second).toEqual(["t1", "t2"]);
    expect(listenerCount("preview://state")).toBe(1);

    unsubscribeSecond();
    expect(listenerCount("preview://state")).toBe(0);
  });

  it("serializes rapid zoom operations per tab", async () => {
    const { bridge, invoke } = makeBridge();
    const firstInvoke = deferred<void>();
    invoke.mockImplementationOnce(() => firstInvoke.promise).mockResolvedValueOnce(undefined);

    const firstZoom = bridge.zoomIn("t1");
    const secondZoom = bridge.zoomIn("t1");

    expect(invoke).toHaveBeenCalledTimes(1);
    firstInvoke.resolve(undefined);
    await firstZoom;
    await secondZoom;

    expect(invoke.mock.calls[1]?.[0]).toBe("desktop_preview_set_zoom");
    const secondArgs = invoke.mock.calls[1]?.[1] as { factor: number; tabId: string } | undefined;
    expect(secondArgs).toMatchObject({ tabId: "t1" });
    expect(secondArgs?.factor).toBeCloseTo(1.2);
  });

  it("orders deferred zoom, close, and recreate on the same tab queue", async () => {
    const { bridge, invoke } = makeBridge();
    const zoomInvoke = deferred<void>();
    const closeInvoke = deferred<void>();
    invoke
      .mockImplementationOnce(() => zoomInvoke.promise)
      .mockImplementationOnce(() => closeInvoke.promise)
      .mockResolvedValueOnce(undefined);

    const zoom = bridge.zoomIn("t1");
    const close = bridge.closeTab("t1");
    const recreate = bridge.createTab("t1");

    expect(invoke.mock.calls.map(([command]) => command)).toEqual(["desktop_preview_set_zoom"]);

    zoomInvoke.resolve(undefined);
    await zoom;
    await Promise.resolve();
    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "desktop_preview_set_zoom",
      "desktop_preview_close_tab",
    ]);

    closeInvoke.resolve(undefined);
    await close;
    await recreate;
    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "desktop_preview_set_zoom",
      "desktop_preview_close_tab",
      "desktop_preview_create_tab",
    ]);
  });

  it("waits for an unresolved close before recreating the same tab", async () => {
    const { bridge, invoke } = makeBridge();
    const closeInvoke = deferred<void>();
    invoke.mockImplementationOnce(() => closeInvoke.promise).mockResolvedValueOnce(undefined);

    const close = bridge.closeTab("t1");
    const recreate = bridge.createTab("t1");

    expect(invoke.mock.calls.map(([command]) => command)).toEqual(["desktop_preview_close_tab"]);

    closeInvoke.resolve(undefined);
    await close;
    await recreate;
    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "desktop_preview_close_tab",
      "desktop_preview_create_tab",
    ]);
  });

  it("invokes create immediately when the tab has no pending operation", async () => {
    const { bridge, invoke } = makeBridge();

    const create = bridge.createTab("t1");

    expect(invoke).toHaveBeenCalledWith("desktop_preview_create_tab", { tabId: "t1" });
    await create;
  });

  it("continues to recreate after close fails without clearing committed zoom", async () => {
    const { bridge, invoke } = makeBridge();
    const closeInvoke = deferred<void>();
    const error = new Error("close failed");
    await bridge.zoomIn("t1");
    invoke
      .mockImplementationOnce(() => closeInvoke.promise)
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce(undefined);

    const close = bridge.closeTab("t1");
    const closeResult = close.then(
      () => undefined,
      (closeError: unknown) => closeError,
    );
    const recreate = bridge.createTab("t1");

    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "desktop_preview_set_zoom",
      "desktop_preview_close_tab",
    ]);

    closeInvoke.reject(error);
    expect(await closeResult).toBe(error);
    await recreate;
    await bridge.zoomIn("t1");

    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "desktop_preview_set_zoom",
      "desktop_preview_close_tab",
      "desktop_preview_create_tab",
      "desktop_preview_set_zoom",
    ]);
    const lastArgs = invoke.mock.lastCall?.[1] as { factor: number; tabId: string } | undefined;
    expect(lastArgs?.factor).toBeCloseTo(1.2);
  });

  it("recovers from a rejected zoom using the last successfully committed factor", async () => {
    const { bridge, invoke } = makeBridge();
    const error = new Error("set zoom failed");
    invoke.mockRejectedValueOnce(error).mockResolvedValueOnce(undefined);

    const failedZoom = bridge.zoomIn("t1");
    const recoveredZoom = bridge.zoomOut("t1");

    await expect(failedZoom).rejects.toBe(error);
    await recoveredZoom;
    expect(invoke).toHaveBeenNthCalledWith(2, "desktop_preview_set_zoom", {
      tabId: "t1",
      factor: 0.9,
    });
  });

  it("uses host state events as the committed factor for subsequent zooms", async () => {
    const { bridge, emit, invoke } = makeBridge();
    bridge.onStateChange(() => {});
    emit("preview://state", statePayload("t1", 1.5));

    await bridge.zoomIn("t1");

    expect(invoke).toHaveBeenCalledWith("desktop_preview_set_zoom", {
      tabId: "t1",
      factor: 1.6,
    });
  });

  it("publishes a committed zoom to state listeners", async () => {
    const { bridge, emit } = makeBridge();
    const states: DesktopPreviewTabState[] = [];
    bridge.onStateChange((_tabId, state) => states.push(state));
    emit("preview://state", statePayload("t1"));
    states.length = 0;

    await bridge.zoomIn("t1");

    expect(states).toHaveLength(1);
    expect(states[0]).toMatchObject({
      tabId: "t1",
      zoomFactor: 1.1,
      navStatus: { kind: "Success", url: "https://example.com/" },
    });
  });

  it("clears committed zoom after close succeeds so a recreated tab starts at one", async () => {
    const { bridge, invoke } = makeBridge();
    await bridge.zoomIn("t1");
    await bridge.closeTab("t1");
    await bridge.createTab("t1");
    await bridge.zoomIn("t1");

    const zoomCalls = invoke.mock.calls.filter(
      ([command]) => command === "desktop_preview_set_zoom",
    );
    expect(zoomCalls).toEqual([
      ["desktop_preview_set_zoom", { tabId: "t1", factor: 1.1 }],
      ["desktop_preview_set_zoom", { tabId: "t1", factor: 1.1 }],
    ]);
  });

  it("preserves committed zoom when close fails", async () => {
    const { bridge, invoke } = makeBridge();
    const error = new Error("close failed");
    await bridge.zoomIn("t1");
    invoke.mockRejectedValueOnce(error);

    await expect(bridge.closeTab("t1")).rejects.toBe(error);
    await bridge.zoomIn("t1");

    const lastCall = invoke.mock.lastCall;
    expect(lastCall?.[0]).toBe("desktop_preview_set_zoom");
    const lastArgs = lastCall?.[1] as { factor: number; tabId: string } | undefined;
    expect(lastArgs).toMatchObject({ tabId: "t1" });
    expect(lastArgs?.factor).toBeCloseTo(1.2);
  });

  it("reports tab status without automation support", async () => {
    const { bridge, emit } = makeBridge();
    bridge.onStateChange(() => {});
    const { state } = statePayload("tab_1");
    emit("preview://state", {
      tabId: "tab_1",
      state: { ...state, navStatus: { kind: "Loading", url: "http://x/", title: "" } },
    });

    await expect(bridge.automation.status("tab_1")).resolves.toEqual({
      available: true,
      visible: true,
      tabId: "tab_1",
      url: "http://x/",
      title: "",
      loading: true,
    });
    emit("preview://state", statePayload("tab_1", 1, "http://x/"));
    await expect(bridge.automation.status("tab_1")).resolves.toMatchObject({
      available: true,
      loading: false,
      title: "Example",
    });
    await expect(bridge.automation.status("missing")).resolves.toMatchObject({
      available: false,
      url: null,
      loading: false,
    });
    await expect(bridge.automation.click("tab_1", {} as never)).rejects.toThrow(/not supported/);
  });

  it("reports a navigated tab as loading the new URL before the native event arrives", async () => {
    const { bridge, emit, invoke } = makeBridge();
    bridge.onStateChange(() => {});
    await bridge.createTab("tab_1");
    emit("preview://state", statePayload("tab_1", 1, "http://old.test/"));

    let finishNavigate: () => void = () => {};
    invoke.mockImplementationOnce(
      () => new Promise<void>((resolve) => (finishNavigate = () => resolve())),
    );
    const navigation = bridge.navigate("tab_1", "http://new.test/");

    await expect(bridge.automation.status("tab_1")).resolves.toMatchObject({
      url: "http://new.test/",
      loading: true,
    });
    finishNavigate();
    await navigation;
  });

  it("does not report a same-document fragment navigation as loading", async () => {
    const { bridge, emit } = makeBridge();
    bridge.onStateChange(() => {});
    await bridge.createTab("tab_1");
    emit("preview://state", statePayload("tab_1", 1, "http://old.test/page"));

    await bridge.navigate("tab_1", "http://old.test/page#section");

    await expect(bridge.automation.status("tab_1")).resolves.toMatchObject({
      url: "http://old.test/page",
      loading: false,
    });
  });

  it("reports a freshly created blank tab as available before any page loads", async () => {
    const { bridge } = makeBridge();
    const states: Array<[string, DesktopPreviewTabState]> = [];
    bridge.onStateChange((tabId, state) => states.push([tabId, state]));

    await bridge.createTab("tab_1");
    await bridge.createTab("tab_2");

    await expect(bridge.automation.status("tab_2")).resolves.toMatchObject({
      available: true,
      url: null,
      loading: false,
    });
    expect(states.map(([tabId, state]) => [tabId, state.navStatus.kind])).toEqual([
      ["tab_1", "Idle"],
      ["tab_2", "Idle"],
    ]);
  });

  it("restores the previous state when native navigation fails", async () => {
    const { bridge, emit, invoke } = makeBridge();
    bridge.onStateChange(() => {});
    await bridge.createTab("tab_1");
    emit("preview://state", statePayload("tab_1", 1, "http://old.test/"));
    invoke.mockRejectedValueOnce(new Error("navigate failed"));

    await expect(bridge.navigate("tab_1", "http://new.test/")).rejects.toThrow("navigate failed");
    await expect(bridge.automation.status("tab_1")).resolves.toMatchObject({
      url: "http://old.test/",
      loading: false,
    });
  });

  it("rejects every unsupported Promise surface with the stable Tauri capability error", async () => {
    const { bridge } = makeBridge();
    const unsupportedCalls: ReadonlyArray<() => Promise<unknown>> = [
      () => bridge.pickElement("t1"),
      () => bridge.copyArtifactToClipboard("/tmp/shot-1.png"),
      () => bridge.recording.startScreencast("t1"),
      () => bridge.recording.stopScreencast("t1"),
      () => bridge.recording.save("t1", "video/webm", new Uint8Array()),
      () => bridge.automation.snapshot("t1"),
      () => bridge.automation.click("t1", undefined as never),
      () => bridge.automation.type("t1", undefined as never),
      () => bridge.automation.press("t1", undefined as never),
      () => bridge.automation.scroll("t1", undefined as never),
      () => bridge.automation.evaluate("t1", undefined as never),
      () => bridge.automation.waitFor("t1", undefined as never),
    ];

    for (const call of unsupportedCalls) {
      try {
        await call();
        expect.unreachable("unsupported preview surface resolved");
      } catch (error) {
        expect(error).toBeInstanceOf(TauriDesktopCapabilityUnsupportedError);
        expect((error as TauriDesktopCapabilityUnsupportedError).code).toBe(
          "tauri_capability_unsupported",
        );
      }
    }
  });

  it("keeps allowed Phase-2/4 no-op surfaces harmless", async () => {
    const { bridge } = makeBridge();

    await expect(bridge.setAnnotationTheme(undefined as never)).resolves.toBeUndefined();
    await expect(bridge.cancelPickElement("t1")).resolves.toBeUndefined();

    const unsubscribeFrame = bridge.recording.onFrame(() => {});
    const unsubscribePointer = bridge.onPointerEvent(() => {});
    expect(unsubscribeFrame).toBeTypeOf("function");
    expect(unsubscribePointer).toBeTypeOf("function");
    expect(() => unsubscribeFrame()).not.toThrow();
    expect(() => unsubscribePointer()).not.toThrow();
  });
});
