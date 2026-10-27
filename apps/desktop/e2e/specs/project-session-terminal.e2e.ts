// @effect-diagnostics nodeBuiltinImport:off - Packaged UI tests save native screenshots.
import { installDesktopUiMotionGuard } from "../support/motion-guard.ts";
import { refreshDesktopUiDocument } from "../support/document-navigation.ts";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import { desktopUiFixture } from "../support/test-project.ts";
import { terminalOutputEventCount } from "../support/terminal-events.ts";
import { openCenterTerminal, sendTerminalCommand } from "../support/terminal-input.ts";
import {
  ensureMainSidebarOpen,
  mockDesktopUiFolderPicker,
  setDesktopUiWindowSize,
} from "../support/ui-state.ts";

const artifactDirectory = process.env.CODDY_STUDIO_E2E_ARTIFACT_DIR;
if (!artifactDirectory) {
  throw new Error("CODDY_STUDIO_E2E_ARTIFACT_DIR is required.");
}
const stateRoot = process.env.CODDY_STUDIO_HOME;
if (!stateRoot) {
  throw new Error("CODDY_STUDIO_HOME is required.");
}
const projectPath = process.env.CODDY_STUDIO_E2E_PROJECT_PATH;
if (!projectPath) {
  throw new Error("CODDY_STUDIO_E2E_PROJECT_PATH is required.");
}

const terminalInputMarkerPath = NodePath.join(projectPath, "terminal-input-smoke.txt");

describe("packaged project session and terminal", () => {
  it("streams a fixture response, reconnects, and exercises terminal lifecycle", async () => {
    await ensureMainSidebarOpen();
    const project = browser.$(
      `//button[.//span[normalize-space()="${desktopUiFixture.projectName}"]]`,
    );
    if (!(await project.isDisplayed())) {
      const projectDataLoading = browser.$(
        "//*[normalize-space()='Project data is still loading']",
      );
      if (await projectDataLoading.isExisting()) {
        await projectDataLoading.waitForDisplayed({ reverse: true });
      }
      const addProject = browser.$('[data-testid="sidebar-add-project-trigger"]');
      await addProject.waitForDisplayed();
      await addProject.click();
      const browseFolder = browser.$(
        "//button[@data-add-project-action='true'][.//span[normalize-space()='Browse folder']]",
      );
      await browseFolder.waitForDisplayed();
      await mockDesktopUiFolderPicker(projectPath);
      await browseFolder.click();
    }
    await expect(project).toBeDisplayed();
    const primaryWorkspace = browser.$(
      '//*[@data-thread-item="true"][.//span[normalize-space()="main"]]',
    );
    if (!(await primaryWorkspace.isDisplayed())) {
      await project.click();
    }
    await expect(primaryWorkspace).toBeDisplayed();
    await primaryWorkspace.click();

    const providerModelPicker = browser.$('[data-chat-provider-model-picker="true"]');
    await expect(providerModelPicker).toBeEnabled();
    await expect(providerModelPicker).toHaveText(expect.stringContaining("GPT-5.4"));

    const composer = browser.$('[data-testid="composer-editor"]');
    await expect(composer).toBeDisplayed();
    await composer.setValue("render the deterministic fixture response");
    const send = browser.$('button[aria-label="Send message"]');
    await expect(send).toBeEnabled();
    await send.click();

    const streamedResponse = browser.$(
      `//*[contains(normalize-space(), "${desktopUiFixture.streamedResponse}")]`,
    );
    await expect(streamedResponse).toBeDisplayed();

    await openCenterTerminal();
    const terminalScreen = browser.$(".xterm-screen");
    await expect(terminalScreen).toBeDisplayed();
    await terminalScreen.click();
    expect(
      await browser.execute(() => {
        const element = document.querySelector<HTMLElement>(".xterm-helper-textarea");
        element?.focus();
        return document.activeElement === element;
      }),
    ).toBe(true);
    const outputEventsBeforeInput = terminalOutputEventCount(stateRoot);
    await sendTerminalCommand("echo CODDY_STUDIO_TERMINAL_SMOKE > terminal-input-smoke.txt");
    await browser.waitUntil(() => terminalOutputEventCount(stateRoot) > outputEventsBeforeInput, {
      timeoutMsg: "The terminal did not produce output after WebDriver keyboard input.",
    });
    await browser.waitUntil(
      () =>
        NodeFS.existsSync(terminalInputMarkerPath) &&
        NodeFS.readFileSync(terminalInputMarkerPath, "utf8").trim() === "CODDY_STUDIO_TERMINAL_SMOKE",
      {
        timeout: 5_000,
        timeoutMsg: "The terminal command was not delivered exactly once to the fixture shell.",
      },
    );

    await setDesktopUiWindowSize(960, 640);
    await browser.saveScreenshot(
      NodePath.join(artifactDirectory, "project-session-terminal-minimum-size.png"),
    );
    const closeTerminal = browser.$('button[aria-label^="Close Terminal"]');
    await expect(closeTerminal).toExist();
    const closedTerminal = await browser.execute(() => {
      const button = document.querySelector<HTMLButtonElement>(
        'button[aria-label^="Close Terminal"]',
      );
      button?.click();
      return button !== null;
    });
    expect(closedTerminal).toBe(true);
    await browser.$(".xterm-screen").waitForExist({ reverse: true });

    await refreshDesktopUiDocument();
    await installDesktopUiMotionGuard();
    await expect(
      browser.$(`//*[contains(normalize-space(), "${desktopUiFixture.streamedResponse}")]`),
    ).toBeDisplayed();
  });
});
