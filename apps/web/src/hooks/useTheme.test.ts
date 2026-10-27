import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { Window } from "happy-dom";

function createStorage(overrides: Partial<Storage> = {}): Storage {
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
    ...overrides,
  };
}

const openWindows: Window[] = [];

/**
 * A browser with a controllable OS colour scheme. Stubs the globals the theme
 * module reads at import, so import the module after calling this.
 */
function installThemeBrowser({
  stored = null,
  prefersDark = false,
}: { stored?: string | null; prefersDark?: boolean } = {}) {
  const browserWindow = new Window({ url: "https://coddy.test/" });
  openWindows.push(browserWindow);
  const storage = createStorage();
  if (stored !== null) storage.setItem("coddy-studio:theme", stored);
  let matches = prefersDark;
  const mediaListeners = new Set<() => void>();
  const media = {
    get matches() {
      return matches;
    },
    addEventListener: (_type: string, listener: () => void) => mediaListeners.add(listener),
    removeEventListener: (_type: string, listener: () => void) => mediaListeners.delete(listener),
  };
  const storageListeners = new Set<(event: StorageEvent) => void>();
  Object.defineProperties(browserWindow, {
    localStorage: { configurable: true, value: storage },
    matchMedia: { configurable: true, value: () => media },
  });
  const addEventListener = browserWindow.addEventListener.bind(browserWindow);
  const removeEventListener = browserWindow.removeEventListener.bind(browserWindow);
  vi.spyOn(browserWindow, "addEventListener").mockImplementation((type, listener) => {
    if (type === "storage") {
      storageListeners.add(listener as unknown as (event: StorageEvent) => void);
    }
    addEventListener(type, listener);
  });
  vi.spyOn(browserWindow, "removeEventListener").mockImplementation((type, listener) => {
    if (type === "storage") {
      storageListeners.delete(listener as unknown as (event: StorageEvent) => void);
    }
    removeEventListener(type, listener);
  });
  vi.stubGlobal("window", browserWindow);
  vi.stubGlobal("document", browserWindow.document);
  vi.stubGlobal("getComputedStyle", browserWindow.getComputedStyle.bind(browserWindow));
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    callback(0);
    return 1;
  });
  return {
    browserWindow,
    storage,
    isDark: () => browserWindow.document.documentElement.classList.contains("dark"),
    mediaListenerCount: () => mediaListeners.size,
    storageListenerCount: () => storageListeners.size,
    switchOs: (dark: boolean) => {
      matches = dark;
      for (const listener of mediaListeners) listener();
    },
    fireStorage: (key: string) => {
      for (const listener of storageListeners) listener({ key } as StorageEvent);
    },
  };
}

/** Lets tests call `useTheme()` outside React; every call subscribes like a mounted component. */
function mockReactForHookCalls(
  onSubscribe: (unsubscribe: () => void) => void,
  listener = () => {},
) {
  vi.doMock("react", () => ({
    useCallback: <A>(callback: A) => callback,
    useEffect: (effect: () => void) => effect(),
    useSyncExternalStore: (
      subscribe: (listener: () => void) => () => void,
      getSnapshot: () => unknown,
    ) => {
      onSubscribe(subscribe(listener));
      return getSnapshot();
    },
  }));
}

afterEach(() => {
  vi.doUnmock("react");
  vi.resetModules();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  for (const browserWindow of openWindows.splice(0)) browserWindow.close();
});

describe("theme failure handling", () => {
  it("preserves exact storage causes and operation context", async () => {
    const readCause = new Error("storage read blocked");
    const writeCause = new Error("storage quota exceeded");
    vi.stubGlobal("window", {
      localStorage: createStorage({
        getItem: () => {
          throw readCause;
        },
        setItem: () => {
          throw writeCause;
        },
      }),
    });

    const { readThemePreference, ThemeStorageError, writeThemePreference } =
      await import("./useTheme");

    try {
      readThemePreference();
      expect.unreachable("expected the theme read to fail");
    } catch (error) {
      expect(error).toBeInstanceOf(ThemeStorageError);
      expect(error).toMatchObject({
        operation: "read",
        storageKey: "coddy-studio:theme",
        cause: readCause,
      });
    }

    try {
      writeThemePreference("dark");
      expect.unreachable("expected the theme write to fail");
    } catch (error) {
      expect(error).toBeInstanceOf(ThemeStorageError);
      expect(error).toMatchObject({
        operation: "write",
        storageKey: "coddy-studio:theme",
        theme: "dark",
        cause: writeCause,
      });
    }
  });

  it("falls back during initial theme application and logs only safe attributes", async () => {
    const cause = new Error("private browsing storage failure");
    const errorLog = vi.spyOn(console, "error").mockImplementation(() => {});
    vi.stubGlobal("window", {
      localStorage: createStorage({
        getItem: () => {
          throw cause;
        },
      }),
      matchMedia: () => ({ matches: false }),
    });
    vi.stubGlobal("document", {
      documentElement: {
        classList: { toggle: vi.fn() },
      },
    });

    await expect(import("./useTheme")).resolves.toBeDefined();

    expect(errorLog).toHaveBeenCalledWith(
      "Failed to read theme preference for coddy-studio:theme.",
      expect.objectContaining({
        operation: "read",
        storageKey: "coddy-studio:theme",
        errorTag: "ThemeStorageError",
      }),
    );
    const attributes = errorLog.mock.calls[0]?.[1];
    expect(attributes).not.toHaveProperty("cause");
    expect(JSON.stringify(attributes)).not.toContain(cause.message);
  });

  it("retries a failed storage read only after a relevant storage event", async () => {
    const cause = new Error("persistent storage failure");
    const getItem = vi.fn(() => {
      throw cause;
    });
    const errorLog = vi.spyOn(console, "error").mockImplementation(() => {});
    let readSnapshot: (() => unknown) | undefined;
    let storageHandler: ((event: StorageEvent) => void) | undefined;
    vi.doMock("react", () => ({
      useCallback: <A>(callback: A) => callback,
      useEffect: () => undefined,
      useSyncExternalStore: (_subscribe: unknown, getSnapshot: () => unknown) => {
        readSnapshot = getSnapshot;
        return getSnapshot();
      },
    }));
    vi.stubGlobal("window", {
      addEventListener: (type: string, listener: (event: StorageEvent) => void) => {
        if (type === "storage") storageHandler = listener;
      },
      localStorage: createStorage({ getItem }),
      matchMedia: () => ({
        matches: false,
        addEventListener: () => undefined,
        removeEventListener: () => undefined,
      }),
      removeEventListener: () => undefined,
    });
    vi.stubGlobal("document", {
      documentElement: {
        classList: { toggle: vi.fn() },
      },
    });

    const { useTheme } = await import("./useTheme");
    useTheme();
    readSnapshot?.();
    readSnapshot?.();

    expect(getItem).toHaveBeenCalledTimes(1);
    expect(errorLog).toHaveBeenCalledTimes(1);

    storageHandler?.({ key: "unrelated" } as StorageEvent);
    readSnapshot?.();
    expect(getItem).toHaveBeenCalledTimes(1);

    storageHandler?.({ key: "coddy-studio:theme" } as StorageEvent);
    readSnapshot?.();

    expect(getItem).toHaveBeenCalledTimes(2);
    expect(errorLog).toHaveBeenCalledTimes(2);
  });

  it("preserves desktop sync causes and retries after a failed cosmetic sync", async () => {
    const cause = new Error("desktop IPC unavailable");
    const errorLog = vi.spyOn(console, "error").mockImplementation(() => {});
    const setTheme = vi.fn().mockRejectedValue(cause);
    vi.stubGlobal("window", { desktopBridge: { setTheme } });

    const { DesktopThemeSyncError, syncDesktopTheme, syncDesktopThemePreference } =
      await import("./useTheme");

    const error = await syncDesktopThemePreference({ setTheme }, "dark").then(
      () => undefined,
      (failure: unknown) => failure,
    );
    expect(error).toBeInstanceOf(DesktopThemeSyncError);
    expect(error).toMatchObject({ theme: "dark", cause });

    setTheme.mockClear();
    syncDesktopTheme("dark");
    await Promise.resolve();
    await Promise.resolve();
    syncDesktopTheme("dark");
    await Promise.resolve();
    await Promise.resolve();

    expect(setTheme).toHaveBeenCalledTimes(2);
    expect(errorLog).toHaveBeenCalledWith(
      "Failed to sync the dark theme to the desktop shell.",
      expect.objectContaining({
        theme: "dark",
        errorTag: "DesktopThemeSyncError",
      }),
    );
    for (const [, attributes] of errorLog.mock.calls) {
      expect(attributes).not.toHaveProperty("cause");
      expect(JSON.stringify(attributes)).not.toContain(cause.message);
    }
  });

  it("uses server-safe defaults and validates stored browser preferences", async () => {
    vi.stubGlobal("window", undefined);
    const serverModule = await import("./useTheme");
    expect(serverModule.readThemePreference()).toBe("system");
    expect(() => serverModule.writeThemePreference("dark")).not.toThrow();
    expect(() => serverModule.syncDesktopTheme("dark")).not.toThrow();

    vi.resetModules();
    const storage = createStorage();
    vi.stubGlobal("window", { localStorage: storage });
    const browserModule = await import("./useTheme");
    for (const theme of ["light", "dark", "system"] as const) {
      storage.setItem("coddy-studio:theme", theme);
      expect(browserModule.readThemePreference()).toBe(theme);
    }
    storage.setItem("coddy-studio:theme", "invalid");
    expect(browserModule.readThemePreference()).toBe("system");
    browserModule.writeThemePreference("light");
    expect(storage.getItem("coddy-studio:theme")).toBe("light");
  });

  it("synchronizes browser chrome colors and reuses its dynamic meta tag", async () => {
    const browserWindow = new Window({ url: "https://coddy.test/" });
    const surface = browserWindow.document.createElement("main");
    surface.setAttribute("data-slot", "sidebar-inset");
    surface.style.backgroundColor = "rgb(12, 34, 56)";
    browserWindow.document.body.style.backgroundColor = "rgb(90, 80, 70)";
    browserWindow.document.body.append(surface);
    vi.stubGlobal("window", browserWindow);
    vi.stubGlobal("document", browserWindow.document);
    vi.stubGlobal("getComputedStyle", browserWindow.getComputedStyle.bind(browserWindow));
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      callback(0);
      return 1;
    });

    const { syncBrowserChromeTheme } = await import("./useTheme");
    syncBrowserChromeTheme();
    expect(browserWindow.document.documentElement.style.backgroundColor).toBe("rgb(12, 34, 56)");
    expect(
      browserWindow.document.querySelector('meta[name="theme-color"]')?.getAttribute("content"),
    ).toBe("rgb(12, 34, 56)");

    surface.style.backgroundColor = "transparent";
    browserWindow.document.body.style.backgroundColor = "rgb(90, 80, 70)";
    syncBrowserChromeTheme();
    expect(browserWindow.document.body.style.backgroundColor).toBe("rgb(90, 80, 70)");
    expect(browserWindow.document.querySelectorAll('meta[name="theme-color"]')).toHaveLength(1);

    surface.remove();
    browserWindow.document.body.style.backgroundColor = "rgba(0, 0, 0, 0)";
    syncBrowserChromeTheme();
    expect(browserWindow.document.querySelectorAll('meta[name="theme-color"]')).toHaveLength(1);
    browserWindow.close();
  });

  it("applies system changes, storage events, and successful desktop sync through the hook", async () => {
    const browser = installThemeBrowser({ stored: "system", prefersDark: true });
    const setTheme = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(browser.browserWindow, "desktopBridge", {
      configurable: true,
      value: { setTheme },
    });
    let unsubscribe: (() => void) | undefined;
    let emitCount = 0;
    mockReactForHookCalls(
      (next) => {
        unsubscribe = next;
      },
      () => {
        emitCount += 1;
      },
    );

    const { useTheme } = await import("./useTheme");
    const theme = useTheme();
    expect(theme).toMatchObject({ theme: "system", resolvedTheme: "dark" });
    await Promise.resolve();
    expect(setTheme).toHaveBeenCalledWith("system");

    browser.switchOs(true);
    browser.fireStorage("unrelated");
    browser.storage.setItem("coddy-studio:theme", "dark");
    browser.fireStorage("coddy-studio:theme");
    expect(emitCount).toBe(2);

    theme.setTheme("light");
    expect(browser.storage.getItem("coddy-studio:theme")).toBe("light");
    expect(browser.isDark()).toBe(false);
    expect(emitCount).toBe(3);

    // Components come and go; the app-wide OS and storage listeners stay installed.
    unsubscribe?.();
    expect(browser.mediaListenerCount()).toBe(1);
    expect(browser.storageListenerCount()).toBe(1);
  });

  it("logs hook-level storage write failures without changing the current theme", async () => {
    const cause = new Error("quota");
    const errorLog = vi.spyOn(console, "error").mockImplementation(() => {});
    vi.doMock("react", () => ({
      useCallback: <A>(callback: A) => callback,
      useEffect: () => undefined,
      useSyncExternalStore: (_subscribe: unknown, getSnapshot: () => unknown) => getSnapshot(),
    }));
    vi.stubGlobal("window", {
      localStorage: createStorage({
        setItem: () => {
          throw cause;
        },
      }),
      matchMedia: () => ({ matches: false }),
    });
    const { useTheme } = await import("./useTheme");

    const theme = useTheme();
    theme.setTheme("dark");

    expect(errorLog).toHaveBeenCalledWith(
      "Failed to write theme preference for coddy-studio:theme.",
      expect.objectContaining({ operation: "write", storageKey: "coddy-studio:theme", theme: "dark" }),
    );
  });
});

// main.tsx loads this module at boot, so these listeners serve every route,
// including routes (such as the home route) that render no `useTheme` consumer.
describe("app-wide theme subscription", () => {
  it("sends the stored choice to a desktop bridge installed after the module loaded", async () => {
    const browser = installThemeBrowser({ stored: "dark" });
    const { applyStoredTheme } = await import("./useTheme");
    expect(browser.isDark()).toBe(true);

    const setTheme = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(browser.browserWindow, "desktopBridge", {
      configurable: true,
      value: { setTheme },
    });
    applyStoredTheme();

    expect(setTheme).toHaveBeenCalledOnce();
    expect(setTheme).toHaveBeenCalledWith("dark");
    expect(browser.isDark()).toBe(true);
  });

  it("follows OS light/dark switches with no useTheme consumer mounted", async () => {
    const browser = installThemeBrowser();
    await import("./useTheme");
    expect(browser.isDark()).toBe(false);

    browser.switchOs(true);
    expect(browser.isDark()).toBe(true);

    browser.switchOs(false);
    expect(browser.isDark()).toBe(false);
  });

  it("follows a theme chosen in another tab with no useTheme consumer mounted", async () => {
    const browser = installThemeBrowser();
    await import("./useTheme");

    browser.storage.setItem("coddy-studio:theme", "dark");
    browser.fireStorage("coddy-studio:theme");
    expect(browser.isDark()).toBe(true);
  });

  it.each(["light", "dark"] as const)(
    "keeps an explicit %s choice while the OS switches",
    async (choice) => {
      const expectedDark = choice === "dark";
      const browser = installThemeBrowser({ stored: choice, prefersDark: !expectedDark });
      await import("./useTheme");
      expect(browser.isDark()).toBe(expectedDark);

      browser.switchOs(expectedDark);
      expect(browser.isDark()).toBe(expectedDark);
      browser.switchOs(!expectedDark);
      expect(browser.isDark()).toBe(expectedDark);
    },
  );

  it("installs one OS listener and one storage listener however many components read the theme", async () => {
    const browser = installThemeBrowser();
    const unsubscribers: Array<() => void> = [];
    mockReactForHookCalls((unsubscribe) => unsubscribers.push(unsubscribe));
    const { useTheme } = await import("./useTheme");
    useTheme();
    useTheme();
    useTheme();

    expect(unsubscribers).toHaveLength(3);
    expect(browser.mediaListenerCount()).toBe(1);
    expect(browser.storageListenerCount()).toBe(1);

    for (const unsubscribe of unsubscribers) unsubscribe();
    browser.switchOs(true);
    expect(browser.isDark()).toBe(true);
  });
});
