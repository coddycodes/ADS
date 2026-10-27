// @effect-diagnostics nodeBuiltinImport:off - Contract tests inspect the packaged UI harness source.
import * as NodeFS from "node:fs";

import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import { mockDesktopUiFolderPicker, setDesktopUiWindowSize } from "./ui-state.ts";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("mockDesktopUiFolderPicker", () => {
  const stubPage = (page: { __wdio_mocks__?: Record<string, unknown> }) => {
    const mockReturnValue = vi.fn(async () => undefined);
    const mock = vi.fn(async () => ({ mockReturnValue }));
    vi.stubGlobal("window", page);
    vi.stubGlobal("browser", {
      tauri: { mock },
      execute: async (script: (path: string) => unknown, path: string) => script(path),
    });
    return { mock, mockReturnValue };
  };

  it("returns the fixture path from the packaged Tauri folder-picker command", async () => {
    const serviceMock = vi.fn(async () => "/tmp/coddy-studio-ui-project");
    const page = { __wdio_mocks__: { desktop_bridge_pick_folder: serviceMock } };
    const { mock, mockReturnValue } = stubPage(page);

    await mockDesktopUiFolderPicker("/tmp/coddy-studio-ui-project");

    expect(mock).toHaveBeenCalledExactlyOnceWith("desktop_bridge_pick_folder");
    expect(mockReturnValue).toHaveBeenCalledExactlyOnceWith("/tmp/coddy-studio-ui-project");
    // The service's own mock reached the page, so it is kept.
    expect(page.__wdio_mocks__.desktop_bridge_pick_folder).toBe(serviceMock);
  });

  it("installs the mock in the page when the service's registration did not reach it", async () => {
    const page: { __wdio_mocks__?: Record<string, unknown> } = {};
    stubPage(page);

    await mockDesktopUiFolderPicker("/tmp/coddy-studio-ui-project");

    const installed = page.__wdio_mocks__?.desktop_bridge_pick_folder as () => Promise<string>;
    await expect(installed()).resolves.toBe("/tmp/coddy-studio-ui-project");
  });
});

describe("setDesktopUiWindowSize", () => {
  it("corrects again when startup native and webview dimensions do not yet agree", async () => {
    const viewport = { devicePixelRatio: 1, innerWidth: 1100, innerHeight: 800 };
    let outer = { width: 1060, height: 739 };
    vi.stubGlobal("window", viewport);
    vi.stubGlobal("screen", { availWidth: 1920, availHeight: 1080 });
    vi.stubGlobal("browser", {
      execute: async (callback: () => unknown) => callback(),
      getWindowSize: async () => ({ ...outer }),
      setWindowSize: async (width: number, height: number) => {
        outer = { width, height };
        viewport.innerWidth = width;
        viewport.innerHeight = height - 44;
      },
      waitUntil: async (predicate: () => Promise<boolean>) => {
        for (let attempt = 0; attempt < 5; attempt++) if (await predicate()) return;
        throw new Error("Viewport did not settle.");
      },
    });
    await setDesktopUiWindowSize(1000, 720);
    expect([viewport.innerWidth, viewport.innerHeight]).toEqual([1000, 720]);
  });

  it("waits for the webview to catch up with native resizing before correcting again", async () => {
    const viewport = { devicePixelRatio: 1, innerWidth: 980, innerHeight: 720 };
    let outer = { width: 980, height: 764 };
    let pending: (() => void) | undefined;
    const setWindowSize = vi.fn(async (width: number, height: number) => {
      outer = { width, height };
      pending = () => {
        viewport.innerWidth = width;
        viewport.innerHeight = height - 44;
      };
    });
    vi.stubGlobal("window", viewport);
    vi.stubGlobal("screen", { availWidth: 1920, availHeight: 1080 });
    vi.stubGlobal("browser", {
      execute: async (callback: () => unknown) => callback(),
      getWindowSize: async () => ({ ...outer }),
      setWindowSize,
      waitUntil: async (predicate: () => Promise<boolean>) => {
        if (await predicate()) return;
        pending?.();
        if (!(await predicate())) throw new Error("Native resize did not reach the webview.");
      },
    });
    await setDesktopUiWindowSize(981, 720);
    expect(viewport.innerWidth).toBe(981);
    expect(setWindowSize).toHaveBeenCalledTimes(1);
  });

  it.each([1, 2])(
    "observes the requested viewport at scale %s without relying on animation frames",
    async (scale) => {
      const viewport = { devicePixelRatio: scale, innerWidth: 1_100, innerHeight: 716 };
      const frame = { width: 1_100 * scale, height: 760 * scale };
      vi.stubGlobal("window", viewport);
      vi.stubGlobal("screen", { availWidth: 1920, availHeight: 1080 });
      vi.stubGlobal("browser", {
        execute: async (callback: () => unknown) => callback(),
        executeAsync: async () => {
          throw new Error("Script execution timed out: animation frames suspended.");
        },
        getWindowSize: async () => ({ ...frame }),
        setWindowSize: async (width: number, height: number) => {
          frame.width = width;
          frame.height = height;
          viewport.innerWidth = width / scale;
          viewport.innerHeight = height / scale - 44;
        },
        waitUntil: async (predicate: () => Promise<boolean>) => {
          for (let attempt = 0; attempt < 5; attempt++) if (await predicate()) return;
          throw new Error("Viewport did not settle.");
        },
      });
      await setDesktopUiWindowSize(1_200, 720);
      expect({ width: viewport.innerWidth, height: viewport.innerHeight }).toEqual({
        width: 1_200,
        height: 720,
      });
    },
  );
});

describe("desktop UI motion stabilization", () => {
  it("keeps the WDIO motion guard from overriding open or closed portal styles", () => {
    const configuration = NodeFS.readFileSync(
      new URL("./motion-guard.ts", import.meta.url),
      "utf8",
    );

    // The guard is one marked stylesheet installed once per document; it never
    // sets inline styles or observes and rewrites portal lifecycle attributes.
    expect(configuration).toContain("style[data-coddy-studio-desktop-ui-automation]");
    expect(configuration).toContain("style.dataset.coddyStudioDesktopUiAutomation");
    expect(configuration).toContain(
      'document.documentElement.dataset.coddyStudioDesktopUiMotion = "disabled"',
    );
    expect(configuration).not.toMatch(
      /style\.setProperty\("(?:opacity|scale|translate|transform)"/,
    );
    expect(configuration).not.toContain("new MutationObserver");
    expect(configuration).not.toMatch(/removeAttribute\("data-(?:starting|ending)-style"\)/);
  });

  it("settles stuck opening portals and hides closed portals through state-aware CSS", () => {
    const configuration = NodeFS.readFileSync(
      new URL("./motion-guard.ts", import.meta.url),
      "utf8",
    );

    expect(configuration).toContain("[data-open][data-starting-style]");
    expect(configuration).toMatch(
      /\[data-open\]\[data-starting-style\]\s*\{[^}]*opacity:\s*1\s*!important;[^}]*\}/s,
    );
    expect(configuration).toMatch(/\[data-closed\]\s*\{[^}]*display:\s*none\s*!important;[^}]*\}/s);
  });

  it("settles auto-animated project rows without overriding unrelated content", () => {
    const configuration = NodeFS.readFileSync(
      new URL("./motion-guard.ts", import.meta.url),
      "utf8",
    );

    expect(configuration).toMatch(
      /\[data-slot="sidebar-group"\]\[data-testid="sidebar-projects-group"\]\s+ul\[data-sidebar="menu"\]\s*>\s*li\s*\{[^}]*opacity:\s*1\s*!important;[^}]*\}/s,
    );
    expect(configuration).not.toContain("new-main-chat-button");
    expect(configuration).not.toMatch(/(?:^|,)\s*li\s*\{[^}]*opacity:/s);
  });

  it("leaves portal lifecycle state to Base UI in every smoke spec", () => {
    for (const spec of ["../specs/main-window.e2e.ts", "../specs/platform-capabilities.e2e.ts"]) {
      const source = NodeFS.readFileSync(new URL(spec, import.meta.url), "utf8");
      expect(source).not.toContain("stabilizeDesktopUiTransitions");
    }
  });
});

describe("packaged composer acceptance contract", () => {
  it("finds sidebar workspace cards without requiring an anchor element", () => {
    const specsDirectory = new URL("../specs/", import.meta.url);
    const anchoredSpecs = NodeFS.readdirSync(specsDirectory)
      .filter((name) => name.endsWith(".e2e.ts"))
      .filter((name) =>
        NodeFS.readFileSync(new URL(name, specsDirectory), "utf8").includes(
          "//a[@data-thread-item",
        ),
      )
      .toSorted();
    expect(anchoredSpecs).toEqual([]);
  });

  const readComposerSpec = (): string =>
    NodeFS.readFileSync(
      new URL("../specs/composer-native-triggers.e2e.ts", import.meta.url),
      "utf8",
    );

  it("checks every visible model row through semantic provider and model attributes", () => {
    const source = readComposerSpec();

    expect(source).toContain('[data-model-picker-content="true"] [data-slot="combobox-item"]');
    expect(source).toContain("element.dataset.modelPickerInstanceId");
    expect(source).toContain("element.dataset.modelPickerModelSlug");
    expect(source).toContain("row.instanceId === scenario.provider");
    expect(source).toContain("row.modelSlug.length > 0");
    expect(source).not.toContain("foreignModels");
  });

  it("uses browser visibility semantics before clicking portal-backed composer items", () => {
    const source = readComposerSpec();

    expect(source).toContain("element.checkVisibility({");
    expect(source).toContain("opacityProperty: true");
    expect(source).toContain("visibilityProperty: true");
    // The click target is resolved inside the visible composer form only after
    // the menu has settled its workspace search, and its ownership is asserted
    // right before the click; there is no global lookup, retry, or sleep.
    expect(source).toContain("await waitForComposerMenuSettled();");
    expect(source).toContain('(await menu.getAttribute("data-composer-menu-loading")) === "false"');
    expect(source).toContain(
      'const candidate = composerForm().$(`${composerMenuSelector} [data-composer-item-id="${id}"]`)',
    );
    expect(source).toContain("await candidate.waitForExist({");
    expect(source).toContain("await candidate.waitForDisplayed({");
    expect(source).toContain("await candidate.waitForEnabled({");
    // The ownership script receives the resolved element, never the chainable
    // wrapper, and the click reuses that same resolved reference.
    expect(source).toContain("const element = await candidate;");
    expect(source).toMatch(
      /browser\.execute\(\s*\(element: HTMLElement, itemId: string\) => \{[\s\S]*?\},\s*element,\s*id,\s*\);/,
    );
    expect(source).toMatch(
      /expect\(ownership\)\.toEqual\(\{[\s\S]*?\}\);\s*await element\.click\(\);/,
    );
    expect(source).toContain("connected: element.isConnected");
    expect(source).toContain('hostVisible: "true"');
    expect(source).not.toContain("const candidate = browser.$(selector)");
    expect(source).not.toContain("for (const candidate of await browser.$$(selector))");
    expect(source).not.toMatch(/browser\.pause\(|setTimeout\(resolve/);
  });

  it("restarts the packaged session and compares the complete native provider payload sequence", () => {
    const source = readComposerSpec();

    expect(source).toContain("await browser.reloadSession()");
    expect(source).not.toContain("await browser.refresh()");
    expect(source).toContain(
      "const composerLogBaseline = readProviderInputLog(preparedProviderInputLogPath).length",
    );
    expect(source).toContain(".slice(composerLogBaseline)");
    expect(source.match(/await activateProviderPanel\("Codex"\)/g)).toHaveLength(2);
    expect(source.match(/await appendAndSelectComposerItem/g)).toHaveLength(3);
    expect(source).not.toContain("await waitForComposerValue(expectedValue)");
    expect(source).toContain('await composerEditor().addValue("$refactor ")');
    expect(source).toContain("expect(actualProviderInputs).toEqual(expectedProviderInputs)");
    for (const prompt of [
      '"$refactor"',
      '"@README.md"',
      '"/compact"',
      '"/docs"',
      '"/review"',
      '"@reviewer"',
    ]) {
      expect(source).toContain(prompt);
    }
    expect(source).not.toContain('provider: "grok"');
    expect(source).not.toContain('prompt: "/skills"');
  });
});

describe("packaged activity viewport contract", () => {
  it("starts responsive coverage at the native window minimum", () => {
    const configuration = JSON.parse(
      NodeFS.readFileSync(new URL("../../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
    ) as { app: { windows: Array<{ minWidth: number }> } };
    const source = NodeFS.readFileSync(
      new URL("../specs/chat-activity-panel.e2e.ts", import.meta.url),
      "utf8",
    );

    expect(configuration.app.windows[0]?.minWidth).toBe(960);
    expect(source).toContain("for (const width of [960, 980, 981, 1_199, 1_200] as const)");
    expect(source).not.toContain("for (const width of [800,");
  });
});
