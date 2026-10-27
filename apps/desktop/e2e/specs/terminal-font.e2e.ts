// @effect-diagnostics nodeBuiltinImport:off - Packaged UI tests save native screenshots.
import * as NodePath from "node:path";

import { desktopUiFixture } from "../support/test-project.ts";
import { terminalOutputEventCount } from "../support/terminal-events.ts";
import { openCenterTerminal, sendTerminalText } from "../support/terminal-input.ts";
import {
  ensureMainSidebarOpen,
  mockDesktopUiFolderPicker,
  setDesktopUiWindowSize,
} from "../support/ui-state.ts";

const artifactDirectory = process.env.CODDY_STUDIO_E2E_ARTIFACT_DIR;
const projectPath = process.env.CODDY_STUDIO_E2E_PROJECT_PATH;
const stateRoot = process.env.CODDY_STUDIO_HOME;
if (!artifactDirectory || !projectPath || !stateRoot) {
  throw new Error("The packaged desktop UI fixture environment was not prepared.");
}
const preparedArtifactDirectory: string = artifactDirectory;
const preparedProjectPath: string = projectPath;
const preparedStateRoot: string = stateRoot;

const fontGlyphProbe = "\ue0b0 \uf115 \u{f0001}";
const terminalGlyphProbe = "\ue0b0 \uf115";

async function openGeneralSettings(): Promise<void> {
  const appOrigin = await browser.execute(() => window.location.origin);
  await browser.url(`${appOrigin}/#/settings/terminal`);
  await expect(browser.$('[aria-label="Terminal font"]')).toBeDisplayed();
}

async function selectTerminalFont(label: string): Promise<void> {
  const selector = browser.$('[aria-label="Terminal font"]');
  await selector.scrollIntoView();
  await selector.click();
  const option = browser.$(`//*[@role="option" and normalize-space()="${label}"]`);
  await option.waitForDisplayed();
  await option.click();
}

async function ensureFixtureProjectImported(): Promise<void> {
  const existingProject = browser.$(
    `//button[.//span[normalize-space()="${desktopUiFixture.projectName}"]]`,
  );
  if (await existingProject.isExisting()) return;

  const addProject = browser.$('[data-testid="sidebar-add-project-trigger"]');
  await expect(addProject).toBeDisplayed();
  await addProject.click();
  await browser.$('[role="dialog"]').waitForExist();
  const browseFolder = browser.$(
    "//button[@data-add-project-action='true'][.//span[normalize-space()='Browse folder']]",
  );
  await browseFolder.waitForDisplayed();
  await mockDesktopUiFolderPicker(preparedProjectPath);
  await browseFolder.click();
  await existingProject.waitForDisplayed();
}

async function waitForTerminalOutputToSettle(previousCount: number): Promise<void> {
  await browser.waitUntil(() => terminalOutputEventCount(preparedStateRoot) > previousCount, {
    timeoutMsg: "The terminal did not produce output for the Nerd Font glyph probe.",
  });

  let lastCount = terminalOutputEventCount(preparedStateRoot);
  let stablePolls = 0;
  await browser.waitUntil(
    () => {
      const currentCount = terminalOutputEventCount(preparedStateRoot);
      if (currentCount === lastCount) {
        stablePolls += 1;
      } else {
        lastCount = currentCount;
        stablePolls = 0;
      }
      return stablePolls >= 5;
    },
    {
      interval: 100,
      timeout: 5_000,
      timeoutMsg: "Terminal output did not settle before the Nerd Font screenshot.",
    },
  );
}

describe("packaged terminal font support", () => {
  it("loads bundled Nerd glyphs, persists a device-local preset, and restores the default", async () => {
    await setDesktopUiWindowSize(1_000, 720);
    await openGeneralSettings();

    const selector = browser.$('[aria-label="Terminal font"]');
    await expect(selector).toHaveText(expect.stringContaining("Bundled Nerd Font"));

    const fontProbe = await browser.executeAsync((probe, done) => {
      void document.fonts
        .load('12px "Coddy Studio JetBrainsMono Nerd Font Mono"', probe)
        .then(() => {
          done({
            loaded: document.fonts.check('12px "Coddy Studio JetBrainsMono Nerd Font Mono"', probe),
            familyRegistered: [...document.fonts].some(
              (face) => face.family === "Coddy Studio JetBrainsMono Nerd Font Mono",
            ),
          });
        })
        .catch((error: unknown) => {
          done({ loaded: false, familyRegistered: false, error: String(error) });
        });
    }, fontGlyphProbe);
    expect(fontProbe).toEqual(expect.objectContaining({ loaded: true, familyRegistered: true }));

    await selectTerminalFont("System monospace");
    await expect(selector).toHaveText(expect.stringContaining("System monospace"));
    await browser.reloadSession();

    await openGeneralSettings();
    await expect(browser.$('[aria-label="Terminal font"]')).toHaveText(
      expect.stringContaining("System monospace"),
    );

    const reset = browser.$('button[aria-label="Reset terminal font to default"]');
    await expect(reset).toBeDisplayed();
    await reset.click();
    await expect(browser.$('[aria-label="Terminal font"]')).toHaveText(
      expect.stringContaining("Bundled Nerd Font"),
    );

    const appOrigin = await browser.execute(() => window.location.origin);
    await browser.url(`${appOrigin}/#/`);
    await ensureMainSidebarOpen();
    await ensureFixtureProjectImported();
    const project = browser.$(
      `//button[.//span[normalize-space()="${desktopUiFixture.projectName}"]]`,
    );
    await expect(project).toBeDisplayed();
    const primaryWorkspace = browser.$(
      '//*[@data-thread-item="true"][.//span[normalize-space()="main"]]',
    );
    if (!(await primaryWorkspace.isDisplayed())) {
      await project.click();
    }
    await expect(primaryWorkspace).toBeDisplayed();
    await primaryWorkspace.click();

    await openCenterTerminal();
    const terminalScreen = browser.$(".xterm-screen");
    await expect(terminalScreen).toBeDisplayed();
    await terminalScreen.click();

    const outputEventsBeforeInput = terminalOutputEventCount(preparedStateRoot);
    await sendTerminalText(terminalGlyphProbe);
    await waitForTerminalOutputToSettle(outputEventsBeforeInput);

    await browser.saveScreenshot(
      NodePath.join(preparedArtifactDirectory, "terminal-nerd-font-glyphs.png"),
    );
  });
});
