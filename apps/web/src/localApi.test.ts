import {
  DEFAULT_CLIENT_SETTINGS,
  type ContextMenuItem,
  type DesktopBridge,
} from "@coddy/contracts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const showContextMenuFallbackMock =
  vi.fn<
    <T extends string>(
      items: readonly ContextMenuItem<T>[],
      position?: { x: number; y: number },
    ) => Promise<T | null>
  >();

vi.mock("./contextMenuFallback", () => ({
  showContextMenuFallback: showContextMenuFallbackMock,
}));

function createLocalStorageStub(): Storage {
  const values = new Map<string, string>();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => {
      values.set(key, value);
    },
    removeItem: (key) => {
      values.delete(key);
    },
    clear: () => values.clear(),
    key: (index) => [...values.keys()][index] ?? null,
    get length() {
      return values.size;
    },
  };
}

function testWindow(): Window & typeof globalThis {
  return globalThis.window ?? (globalThis as unknown as Window & typeof globalThis);
}

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  if (globalThis.window === undefined) {
    Object.defineProperty(globalThis, "window", {
      configurable: true,
      value: globalThis,
    });
  }
  Reflect.deleteProperty(testWindow(), "desktopBridge");
  Reflect.deleteProperty(testWindow(), "nativeApi");
  Object.defineProperty(testWindow(), "localStorage", {
    configurable: true,
    value: createLocalStorageStub(),
  });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("LocalApi", () => {
  it("keeps backend operations unavailable in the browser facade", async () => {
    const { createLocalApi } = await import("./localApi");
    const api = createLocalApi();

    await expect(api.server.getConfig()).rejects.toThrow(
      "Local backend API is unavailable before a backend is paired.",
    );
    await expect(api.shell.openInEditor("/tmp", "cursor")).rejects.toThrow(
      "Local backend API is unavailable before a backend is paired.",
    );
  }, 30_000);

  it("uses the browser context-menu fallback without a desktop bridge", async () => {
    showContextMenuFallbackMock.mockResolvedValue("rename");
    const { createLocalApi } = await import("./localApi");
    const items = [{ id: "rename", label: "Rename" }] as const;

    await expect(createLocalApi().contextMenu.show(items, { x: 4, y: 5 })).resolves.toBe("rename");
    expect(showContextMenuFallbackMock).toHaveBeenCalledWith(items, { x: 4, y: 5 });
  });

  it("delegates host capabilities and persistence to the desktop bridge", async () => {
    const showContextMenu = vi.fn().mockResolvedValue("delete");
    const pickFolder = vi.fn().mockResolvedValue("/tmp/project");
    const getClientSettings = vi.fn().mockResolvedValue(DEFAULT_CLIENT_SETTINGS);
    const setClientSettings = vi.fn().mockResolvedValue(undefined);
    testWindow().desktopBridge = {
      showContextMenu,
      pickFolder,
      getClientSettings,
      setClientSettings,
    } as unknown as DesktopBridge;

    const { createLocalApi } = await import("./localApi");
    const api = createLocalApi();
    const items = [{ id: "delete", label: "Delete" }] as const;

    await expect(api.contextMenu.show(items)).resolves.toBe("delete");
    await expect(api.dialogs.pickFolder({ initialPath: "/tmp" })).resolves.toBe("/tmp/project");
    await expect(api.persistence.getClientSettings()).resolves.toEqual(DEFAULT_CLIENT_SETTINGS);
    await api.persistence.setClientSettings(DEFAULT_CLIENT_SETTINGS);

    expect(showContextMenu).toHaveBeenCalledWith(items, undefined);
    expect(pickFolder).toHaveBeenCalledWith({ initialPath: "/tmp" });
    expect(getClientSettings).toHaveBeenCalledTimes(1);
    expect(setClientSettings).toHaveBeenCalledWith(DEFAULT_CLIENT_SETTINGS);
  });

  it("persists client settings in browser storage", async () => {
    const { createLocalApi } = await import("./localApi");
    const api = createLocalApi();
    const settings = {
      ...DEFAULT_CLIENT_SETTINGS,
      timestampFormat: "12-hour" as const,
    };

    await api.persistence.setClientSettings(settings);
    await expect(api.persistence.getClientSettings()).resolves.toEqual(settings);
  });

  it("prefers the native LocalApi when one is injected", async () => {
    const nativeApi = { dialogs: {} };
    testWindow().nativeApi = nativeApi as never;
    const { readLocalApi } = await import("./localApi");

    expect(readLocalApi()).toBe(nativeApi);
  });

  it("uses browser dialogs and returns no folder without a desktop bridge", async () => {
    const confirm = vi.fn().mockReturnValue(true);
    Object.defineProperty(testWindow(), "confirm", { configurable: true, value: confirm });
    const { createLocalApi } = await import("./localApi");
    const api = createLocalApi();

    await expect(api.dialogs.pickFolder()).resolves.toBeNull();
    await expect(api.dialogs.confirm("Continue?")).resolves.toBe(true);
    expect(confirm).toHaveBeenCalledWith("Continue?");
  });

  it("delegates confirmation and external links to a desktop bridge", async () => {
    const confirm = vi.fn().mockResolvedValue(false);
    const openExternal = vi.fn().mockResolvedValueOnce(true).mockResolvedValueOnce(false);
    testWindow().desktopBridge = { confirm, openExternal } as unknown as DesktopBridge;
    const { createLocalApi } = await import("./localApi");
    const api = createLocalApi();

    await expect(api.dialogs.confirm("Continue?")).resolves.toBe(false);
    await expect(api.shell.openExternal("https://example.test/one")).resolves.toBeUndefined();
    await expect(api.shell.openExternal("https://example.test/two")).rejects.toThrow(
      "Unable to open link.",
    );
  });

  it("opens external links in a protected browser tab", async () => {
    const open = vi.fn();
    Object.defineProperty(testWindow(), "open", { configurable: true, value: open });
    const { createLocalApi } = await import("./localApi");

    await createLocalApi().shell.openExternal("https://example.test");

    expect(open).toHaveBeenCalledWith("https://example.test", "_blank", "noopener,noreferrer");
  });

  it("caches browser facades and rejects ensureLocalApi without a window", async () => {
    const { ensureLocalApi, readLocalApi } = await import("./localApi");
    const first = readLocalApi();
    expect(first).toBeDefined();
    expect(readLocalApi()).toBe(first);
    expect(ensureLocalApi()).toBe(first);

    vi.resetModules();
    Reflect.deleteProperty(globalThis, "window");
    const withoutWindow = await import("./localApi");
    expect(withoutWindow.readLocalApi()).toBeUndefined();
    expect(() => withoutWindow.ensureLocalApi()).toThrow("Local API not found");
  });
});
