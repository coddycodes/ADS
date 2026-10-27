import { correctDesktopUiOuterSize } from "./window-size.ts";

/**
 * Mocks the native folder picker in the current document. The service registers its mock
 * through the plugin's IPC script path, and after a session restart that registration can
 * report success while `window.__wdio_mocks__` never reaches the page (#177; the cause in the
 * service is unknown). The click then opens the real dialog, which nobody closes on a runner.
 * The app reads `window.__wdio_mocks__` before invoking the host, so a missing entry is written
 * there directly through WebDriver, which reaches the page.
 */
export async function mockDesktopUiFolderPicker(projectPath: string): Promise<void> {
  const picker = await browser.tauri.mock("desktop_bridge_pick_folder");
  await picker.mockReturnValue(projectPath);
  const installed = await browser.execute((path: string) => {
    const host = window as Window & { __wdio_mocks__?: Record<string, unknown> };
    const mocks = (host.__wdio_mocks__ ??= {});
    if (typeof mocks.desktop_bridge_pick_folder !== "function") {
      mocks.desktop_bridge_pick_folder = async () => path;
    }
    return typeof mocks.desktop_bridge_pick_folder === "function";
  }, projectPath);
  if (!installed) {
    throw new Error("The folder-picker mock could not be installed in the page.");
  }
}

export async function setDesktopUiWindowSize(width: number, height: number): Promise<void> {
  const devicePixelRatio = await browser.execute(() => window.devicePixelRatio);
  const requestedViewportSize = { width, height };
  const readViewport = () =>
    browser.execute(() => ({
      width: window.innerWidth,
      height: window.innerHeight,
    }));
  const scale = Number.isFinite(devicePixelRatio) && devicePixelRatio > 0 ? devicePixelRatio : 1;
  let previous:
    | { width: number; height: number; outerWidth: number; outerHeight: number }
    | undefined;
  await browser.waitUntil(
    async () => {
      const observed = await readViewport();
      if (observed.width === width && observed.height === height) return true;
      const outer = await browser.getWindowSize();
      const available = await browser.execute(() => ({
        width: screen.availWidth,
        height: screen.availHeight,
      }));
      // Oversized screenshots are bounded by the native display. Exact-size
      // responsive assertions remain mandatory for viewports that fit the screen.
      if (
        (width > available.width || height > available.height) &&
        previous?.width === observed.width &&
        previous.height === observed.height &&
        previous.outerWidth === outer.width &&
        previous.outerHeight === outer.height
      )
        return true;
      previous = { ...observed, outerWidth: outer.width, outerHeight: outer.height };
      const corrected = correctDesktopUiOuterSize(outer, requestedViewportSize, observed, scale);
      await browser.setWindowSize(corrected.width, corrected.height);
      // Poll actual geometry before the next correction. Initial GTK decoration
      // and webview sizes can settle independently, so a predicted delta is unsafe.
      return false;
    },
    { timeoutMsg: `The webview did not reach the requested ${width}x${height} viewport.` },
  );
}

export async function ensureMainSidebarOpen(): Promise<void> {
  await setDesktopUiWindowSize(1_000, 720);
  const wrapper = browser.$('[data-slot="sidebar-wrapper"]');
  await expect(wrapper).toBeDisplayed();

  if ((await wrapper.getAttribute("data-sidebar-state")) === "collapsed") {
    const toggle = browser.$('button[aria-label="Toggle main sidebar"]');
    await expect(toggle).toBeDisplayed();
    await toggle.click();
  }

  await expect(wrapper).toHaveAttribute("data-sidebar-state", "expanded");
}

export async function getMainSidebarVisibilityDiagnostics(): Promise<unknown> {
  return browser.execute(() => {
    const trigger = document.querySelector<HTMLElement>(
      '[data-testid="sidebar-add-project-trigger"]',
    );
    const ancestry: Array<Record<string, unknown>> = [];
    let current: HTMLElement | null = trigger;
    while (current && ancestry.length < 8) {
      const style = window.getComputedStyle(current);
      const rect = current.getBoundingClientRect();
      ancestry.push({
        tag: current.tagName,
        dataSlot: current.dataset.slot ?? null,
        dataState: current.dataset.state ?? null,
        className: current.className,
        display: style.display,
        visibility: style.visibility,
        opacity: style.opacity,
        position: style.position,
        transform: style.transform,
        width: rect.width,
        height: rect.height,
        x: rect.x,
        y: rect.y,
      });
      current = current.parentElement;
    }
    return {
      innerWidth: window.innerWidth,
      outerWidth: window.outerWidth,
      devicePixelRatio: window.devicePixelRatio,
      desktopMediaQuery: window.matchMedia("(min-width: 48rem)").matches,
      ancestry,
    };
  });
}

export async function getAddProjectDialogVisibilityDiagnostics(): Promise<unknown> {
  return browser.execute(() => {
    const describe = (element: HTMLElement | null): Record<string, unknown> | null => {
      if (!element) return null;
      const style = window.getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return {
        tag: element.tagName,
        dataStartingStyle: element.hasAttribute("data-starting-style"),
        dataOpen: element.hasAttribute("data-open"),
        display: style.display,
        visibility: style.visibility,
        opacity: style.opacity,
        pointerEvents: style.pointerEvents,
        position: style.position,
        transform: style.transform,
        width: rect.width,
        height: rect.height,
        x: rect.x,
        y: rect.y,
      };
    };
    const browse = [...document.querySelectorAll<HTMLElement>("[data-add-project-action]")].find(
      (element) => element.textContent?.includes("Browse folder"),
    );
    const popup = document.querySelector<HTMLElement>('[role="dialog"]');
    const motionRules = [...document.styleSheets].flatMap((sheet) => {
      try {
        return [...sheet.cssRules]
          .map((rule) => rule.cssText)
          .filter(
            (rule) =>
              rule.includes("[data-open][data-starting-style]") || rule.includes("[data-closed]"),
          );
      } catch {
        return [];
      }
    });
    return {
      popup: describe(popup),
      viewport: describe(document.querySelector<HTMLElement>('[data-slot="dialog-viewport"]')),
      browse: describe(browse ?? null),
      popupMatchesOpenStarting: popup?.matches("[data-open][data-starting-style]") ?? false,
      motionStyleInstalled: document.documentElement.dataset.coddyStudioDesktopUiMotion === "disabled",
      motionSheetActive: motionRules.length === 2,
      motionRules,
    };
  });
}
