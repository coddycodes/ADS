// @effect-diagnostics nodeBuiltinImport:off - Packaged UI tests save native screenshots.
import * as NodePath from "node:path";

import { desktopUiFixture } from "../support/test-project.ts";
import {
  ensureMainSidebarOpen,
  getAddProjectDialogVisibilityDiagnostics,
  getMainSidebarVisibilityDiagnostics,
  mockDesktopUiFolderPicker,
  setDesktopUiWindowSize,
} from "../support/ui-state.ts";

const projectPath = process.env.CODDY_STUDIO_E2E_PROJECT_PATH;
const artifactDirectory = process.env.CODDY_STUDIO_E2E_ARTIFACT_DIR;

if (!projectPath || !artifactDirectory) {
  throw new Error("The packaged desktop UI fixture environment was not prepared.");
}

describe("packaged main window and add-project flow", () => {
  it("adds a Git project through Browse folder and persists it after restart", async () => {
    await setDesktopUiWindowSize(1_000, 720);
    await expect(browser.$("#root")).toBeDisplayed();
    await ensureMainSidebarOpen();

    const addProject = browser.$('[data-testid="sidebar-add-project-trigger"]');
    if (!(await addProject.isDisplayed())) {
      throw new Error(
        `The expanded sidebar is not visible: ${JSON.stringify(
          await getMainSidebarVisibilityDiagnostics(),
        )}`,
      );
    }
    await expect(addProject).toBeDisplayed();
    await addProject.click();
    await browser.$('[role="dialog"]').waitForExist();
    const browseFolder = browser.$(
      "//button[@data-add-project-action='true'][.//span[normalize-space()='Browse folder']]",
    );
    try {
      await browseFolder.waitForDisplayed();
    } catch {
      throw new Error(
        `The add-project dialog is not visible: ${JSON.stringify(
          await getAddProjectDialogVisibilityDiagnostics(),
        )}`,
      );
    }
    await mockDesktopUiFolderPicker(projectPath);
    const pickerMockDiagnostics = await browser.execute(() => {
      const e2eWindow = window as Window & {
        readonly __TAURI__?: {
          readonly core?: {
            readonly invoke?: unknown;
            readonly _wdioInvokeInterceptor?: boolean;
          };
        };
        readonly __wdio_mocks__?: Record<string, unknown>;
        readonly __wdio_original_core__?: {
          readonly invoke?: unknown;
        };
        readonly __wdio_tauri_internals_invoke_interceptor__?: boolean;
        readonly __TAURI_INTERNALS__?: {
          readonly invoke?: unknown;
        };
      };
      const internals = e2eWindow.__TAURI_INTERNALS__;
      const internalInvokeDescriptor = internals
        ? Object.getOwnPropertyDescriptor(internals, "invoke")
        : undefined;
      return {
        hasMock: typeof e2eWindow.__wdio_mocks__?.desktop_bridge_pick_folder === "function",
        hasGlobalInvoke: typeof e2eWindow.__TAURI__?.core?.invoke === "function",
        hasInvokeInterceptor:
          e2eWindow.__TAURI__?.core?._wdioInvokeInterceptor === true ||
          e2eWindow.__wdio_tauri_internals_invoke_interceptor__ === true,
        hasOriginalCore: typeof e2eWindow.__wdio_original_core__?.invoke === "function",
        hasInternalInvoke: typeof internals?.invoke === "function",
        internalInvokeConfigurable: internalInvokeDescriptor?.configurable ?? null,
        internalInvokeWritable: internalInvokeDescriptor?.writable ?? null,
        internalsExtensible: internals ? Object.isExtensible(internals) : null,
      };
    });
    if (!pickerMockDiagnostics.hasMock || !pickerMockDiagnostics.hasGlobalInvoke) {
      throw new Error(
        `The packaged folder-picker mock is not active: ${JSON.stringify(pickerMockDiagnostics)}`,
      );
    }
    await browseFolder.click();

    const project = browser.$(`//*[normalize-space()="${desktopUiFixture.projectName}"]`);
    try {
      await project.waitForDisplayed();
    } catch {
      const projectVisibility = await browser.execute((projectName) => {
        const projectElement = [...document.querySelectorAll<HTMLElement>("*")].find(
          (element) => element.textContent?.trim() === projectName,
        );
        const ancestry: Array<Record<string, unknown>> = [];
        let current: HTMLElement | null = projectElement ?? null;
        while (current && ancestry.length < 12) {
          const style = window.getComputedStyle(current);
          const rect = current.getBoundingClientRect();
          ancestry.push({
            tag: current.tagName,
            dataClosed: current.hasAttribute("data-closed"),
            dataStartingStyle: current.hasAttribute("data-starting-style"),
            ariaHidden: current.getAttribute("aria-hidden"),
            inert: current.hasAttribute("inert"),
            display: style.display,
            visibility: style.visibility,
            opacity: style.opacity,
            position: style.position,
            width: rect.width,
            height: rect.height,
            x: rect.x,
            y: rect.y,
          });
          current = current.parentElement;
        }
        return {
          found: projectElement !== undefined,
          ancestry,
        };
      }, desktopUiFixture.projectName);
      throw new Error(`The imported project is not visible: ${JSON.stringify(projectVisibility)}`);
    }
    await browser.reloadSession();
    await expect(
      browser.$(`//*[normalize-space()="${desktopUiFixture.projectName}"]`),
    ).toBeDisplayed();

    await setDesktopUiWindowSize(1280, 900);
    await browser.saveScreenshot(NodePath.join(artifactDirectory, "main-window-marketing.png"));

    await setDesktopUiWindowSize(960, 640);
    await browser.saveScreenshot(NodePath.join(artifactDirectory, "main-window-minimum-size.png"));
  });
});
