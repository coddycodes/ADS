// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => {
  let resolveReady!: () => void;
  const ready = new Promise<void>((resolve) => {
    resolveReady = resolve;
  });

  return {
    mediaListeners: new Set<() => void>(),
    ready,
    rendered: [] as Array<{ mediaListeners: number; desktopThemes: string[] }>,
    resolveReady,
    setTheme: vi.fn(async (_theme: string) => undefined),
  };
});

vi.mock("./bootstrap", () => ({
  renderApplication: async () => {
    harness.rendered.push({
      mediaListeners: harness.mediaListeners.size,
      desktopThemes: harness.setTheme.mock.calls.map(([theme]) => theme),
    });
  },
}));

vi.mock("./tauriDesktopBridge", () => ({
  tauriDesktopBridgeReady: harness.ready,
}));

vi.mock("./desktopCloseShortcut", () => ({
  installDesktopCloseShortcutRouter: async () => undefined,
}));

vi.mock("./env", () => ({ isTauri: false }));
vi.mock("./linuxWebkitTypography", () => ({ applyLinuxWebkitTypography: () => undefined }));

function createStorage(): Storage {
  const store = new Map<string, string>();
  return {
    clear: () => store.clear(),
    getItem: (key) => store.get(key) ?? null,
    key: (index) => [...store.keys()][index] ?? null,
    get length() {
      return store.size;
    },
    removeItem: (key) => {
      store.delete(key);
    },
    setItem: (key, value) => {
      store.set(key, value);
    },
  };
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  Reflect.deleteProperty(window, "desktopBridge");
});

describe("app boot", () => {
  it("follows the OS theme on every route and syncs the desktop shell once its bridge exists", async () => {
    const storage = createStorage();
    storage.setItem("coddy-studio:theme", "dark");
    vi.stubGlobal("localStorage", storage);
    vi.spyOn(window, "matchMedia").mockImplementation(
      (query: string) =>
        ({
          matches: false,
          media: query,
          onchange: null,
          addEventListener: (_type: string, listener: () => void) =>
            harness.mediaListeners.add(listener),
          removeEventListener: (_type: string, listener: () => void) =>
            harness.mediaListeners.delete(listener),
          addListener: () => undefined,
          removeListener: () => undefined,
          dispatchEvent: () => false,
        }) as unknown as MediaQueryList,
    );

    await import("./main");

    // Boot is still waiting for the desktop bridge, which installs itself asynchronously.
    expect(harness.rendered).toEqual([]);
    expect(document.documentElement.classList.contains("dark")).toBe(true);

    Object.defineProperty(window, "desktopBridge", {
      configurable: true,
      value: { setTheme: harness.setTheme },
    });
    harness.resolveReady();

    await vi.waitFor(() => expect(harness.rendered).toHaveLength(1));
    expect(harness.rendered[0]).toEqual({ mediaListeners: 1, desktopThemes: ["dark"] });
  });
});
