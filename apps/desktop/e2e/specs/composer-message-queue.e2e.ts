// @effect-diagnostics nodeBuiltinImport:off - Packaged queue tests read fixture logs and save screenshots.
import * as NodePath from "node:path";

import { refreshDesktopUiDocument } from "../support/document-navigation.ts";
import { installDesktopUiMotionGuard } from "../support/motion-guard.ts";
import {
  readProviderInputLog,
  waitForProviderInputLogEntry,
} from "../support/provider-input-log.ts";
import { queuedCardIdsInVisualOrder } from "../support/queued-card-order.ts";
import { completeDesktopUiSlowTurn, desktopUiFixture } from "../support/test-project.ts";
import {
  ensureMainSidebarOpen,
  mockDesktopUiFolderPicker,
  setDesktopUiWindowSize,
} from "../support/ui-state.ts";

const artifactDirectory = process.env.BIBCODE_E2E_ARTIFACT_DIR;
const projectPath = process.env.BIBCODE_E2E_PROJECT_PATH;
const providerInputLogPath = process.env.BIBCODE_E2E_PROVIDER_INPUT_LOG;
if (!artifactDirectory || !projectPath || !providerInputLogPath) {
  throw new Error("The packaged desktop message queue fixture environment was not prepared.");
}
const preparedArtifactDirectory: string = artifactDirectory;
const preparedProjectPath: string = projectPath;
const preparedProviderInputLogPath: string = providerInputLogPath;
const visibleSurfaceSelector = '[data-center-surface-host][data-visible="true"]';
const visibleComposerFormSelector = `${visibleSurfaceSelector} [data-chat-composer-form="true"]`;
const queuedRowSelector = `${visibleSurfaceSelector} [data-queued-message-row]`;

function composerForm() {
  return browser.$(visibleComposerFormSelector);
}
function composerEditor() {
  return composerForm().$('[data-testid="composer-editor"]');
}

async function waitForComposerDisplayed(): Promise<void> {
  const form = browser.$(visibleComposerFormSelector);
  await form.waitForExist();
  await form.waitForDisplayed();
  await form.$('[data-testid="composer-editor"]').waitForDisplayed();
}

async function ensureFixtureProjectImported(): Promise<void> {
  const appOrigin = await browser.execute(() => window.location.origin);
  await browser.url(`${appOrigin}/#/`);
  await ensureMainSidebarOpen();
  const projectSelector = `//button[@data-sidebar="menu-button"][.//span[normalize-space()="${desktopUiFixture.projectName}"]]`;
  const hasVisibleProject = async () => {
    for (const candidate of await browser.$$(projectSelector)) {
      if (await candidate.isDisplayed()) return true;
    }
    return false;
  };
  if (await hasVisibleProject()) {
    return;
  }

  const projectDataLoading = browser.$("//*[normalize-space()='Project data is still loading']");
  if (await projectDataLoading.isExisting()) {
    await projectDataLoading.waitForDisplayed({
      reverse: true,
      timeoutMsg: "The primary project catalog did not become ready for fixture import.",
    });
  }

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
  await browser.waitUntil(hasVisibleProject, {
    timeoutMsg: "The imported fixture project did not become visible in the main sidebar.",
  });
}

async function openInitialCodexDraft(): Promise<void> {
  const projectSelector = `//button[@data-sidebar="menu-button"][.//span[normalize-space()="${desktopUiFixture.projectName}"]]`;
  const primaryWorkspace = browser.$(
    '//*[@data-thread-item="true"][.//span[normalize-space()="main"]]',
  );
  if (!(await primaryWorkspace.isDisplayed())) {
    let projectClicked = false;
    for (const project of await browser.$$(projectSelector)) {
      if (await project.isDisplayed()) {
        await project.click();
        projectClicked = true;
        break;
      }
    }
    expect(projectClicked).toBe(true);
    await primaryWorkspace.waitForDisplayed();
  }
  await primaryWorkspace.click();
  await waitForComposerDisplayed();
}

async function replaceComposerText(text: string): Promise<void> {
  const editor = composerEditor();
  await editor.click();
  await browser.execute(() => {
    const editor = document.activeElement;
    if (!(editor instanceof HTMLElement) || editor.dataset.testid !== "composer-editor") {
      throw new Error("The composer editor did not receive focus.");
    }
    const range = document.createRange();
    range.selectNodeContents(editor);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
  });
  await browser.keys("Backspace");
  await editor.addValue(text);
  await expect(editor).toHaveText(text);
}

async function submitWithEnter(text: string): Promise<void> {
  await replaceComposerText(text);
  await browser.keys("Enter");
  await expect(composerEditor()).toHaveText("");
}

async function assertQueuedCards(prompts: readonly string[]): Promise<string[]> {
  let visibleIds: string[] = [];
  await browser.waitUntil(
    async () => {
      const cards = await browser.$$(queuedRowSelector);
      const observations = await Promise.all(
        Array.from(cards, async (card) => ({
          id: (await card.getAttribute("data-queued-message-row")) ?? "",
          text: await card.getText(),
          y: await card.getLocation("y"),
          displayed: await card.isDisplayed(),
        })),
      );
      // LegendList positions pooled slots independently of DOM order and
      // reorders its DOM later. Observe visual FIFO before retaining row IDs.
      const ids = queuedCardIdsInVisualOrder(observations, prompts);
      if (ids === null) return false;
      visibleIds = ids;
      return true;
    },
    {
      timeoutMsg: `Expected ${prompts.length} queued cards: ${prompts.join(", ")}.`,
    },
  );
  const wrappers = await browser.$$(
    `${visibleSurfaceSelector} [data-timeline-row-kind="queued-message"]`,
  );
  expect(await wrappers.length).toBe(prompts.length);
  const ids: string[] = [];
  for (let index = 0; index < visibleIds.length; index += 1) {
    const card = queuedCard(visibleIds[index]!);
    await expect(card).toBeDisplayed();
    await expect(card).toHaveText(expect.stringContaining(prompts[index]!));
    await expect(card).toHaveText(expect.stringContaining("Queued"));
    const id = await card.getAttribute("data-queued-message-row");
    if (!id) throw new Error("A queued card did not expose its durable message ID.");
    ids.push(id);
  }
  return ids;
}

/**
 * LegendList measures rows before it settles their positions, so one reading can catch the
 * working row below a queued card that is in fact under it (Linux saw 251 against 175 while
 * the settled page had them the right way round). Waits for the settled order instead.
 */
async function expectWorkingRowAboveQueuedCard(messageId: string): Promise<void> {
  let observed = "no displayed working row";
  try {
    await browser.waitUntil(async () => {
      for (const row of await browser.$$(
        `${visibleSurfaceSelector} [data-timeline-row-kind="working"]`,
      )) {
        if (!(await row.isDisplayed())) continue;
        const working = await row.getLocation("y");
        const card = await queuedCard(messageId).getLocation("y");
        observed = `working row at ${working}, queued card at ${card}`;
        return working < card;
      }
      return false;
    });
  } catch (error) {
    // The spec reporter drops `cause`, so the wait's own failure goes in the message too.
    throw new Error(
      `The working row did not settle above queued card ${messageId}: ${observed}. ${String(error)}`,
      { cause: error },
    );
  }
}

function queuedCard(messageId: string) {
  return browser.$(`${visibleSurfaceSelector} [data-queued-message-row="${messageId}"]`);
}

function inputSequence(baseline: number) {
  return readProviderInputLog(preparedProviderInputLogPath)
    .slice(baseline)
    .map(({ kind, prompt }) => ({ kind, prompt }));
}

describe("packaged composer message queue", () => {
  it("queues in order, steers the head, restores Cancel, reloads, and promotes after settle", async () => {
    await ensureFixtureProjectImported();
    await openInitialCodexDraft();
    await setDesktopUiWindowSize(1_100, 900);
    await expect(composerForm().$('[data-chat-provider-model-picker="true"]')).toHaveText(
      expect.stringContaining("GPT-5.4"),
    );
    const baseline = readProviderInputLog(preparedProviderInputLogPath).length;
    const prompt = "first [[slow]]";
    let slowTurnId: string | undefined;
    let released = false;
    try {
      await submitWithEnter(prompt);
      const start = await waitForProviderInputLogEntry(preparedProviderInputLogPath, baseline, {
        provider: "codex",
        prompt,
        kind: "start",
      });
      expect(start.turnId).toBeTruthy();
      slowTurnId = start.turnId;
      await expect(composerForm().$('button[aria-label="Stop generation"]')).toBeDisplayed();
      await expect(
        browser.$(`${visibleSurfaceSelector} [data-timeline-row-kind="working"]`),
      ).toBeDisplayed();

      await submitWithEnter("second");
      await assertQueuedCards(["second"]);
      await submitWithEnter("third");
      const [secondId, thirdId] = await assertQueuedCards(["second", "third"]);
      expect(inputSequence(baseline)).toEqual([{ kind: "start", prompt }]);
      await expectWorkingRowAboveQueuedCard(secondId!);
      await expect(queuedCard(secondId!).$('[data-queued-message-steer="true"]')).toBeEnabled();
      await expect(queuedCard(thirdId!).$('[data-queued-message-steer="true"]')).not.toExist();
      await browser.saveScreenshot(
        NodePath.join(preparedArtifactDirectory, "message-queue-two-cards.png"),
      );

      await queuedCard(secondId!).$('[data-queued-message-steer="true"]').click();
      const steer = await waitForProviderInputLogEntry(preparedProviderInputLogPath, baseline + 1, {
        provider: "codex",
        prompt: "second",
        kind: "steer",
      });
      expect(steer.turnId).toBe(start.turnId);
      await queuedCard(secondId!).waitForExist({ reverse: true });
      expect(await assertQueuedCards(["third"])).toEqual([thirdId]);
      await expect(composerForm().$('button[aria-label="Stop generation"]')).toBeDisplayed();

      await queuedCard(thirdId!).$('[data-queued-message-cancel="true"]').click();
      await assertQueuedCards([]);
      await expect(composerEditor()).toHaveText("third");
      await submitWithEnter("fourth");
      const fourthIds = await assertQueuedCards(["fourth"]);
      expect(inputSequence(baseline)).toEqual([
        { kind: "start", prompt },
        { kind: "steer", prompt: "second" },
      ]);

      // Uses browser.refresh() and waits for the native document lifecycle;
      // reloadSession() would restart the provider instead of testing a reload.
      await refreshDesktopUiDocument();
      await installDesktopUiMotionGuard();
      await waitForComposerDisplayed();
      expect(await assertQueuedCards(["fourth"])).toEqual(fourthIds);
      await expect(composerForm().$('button[aria-label="Stop generation"]')).toBeDisplayed();
      expect(inputSequence(baseline)).toEqual([
        { kind: "start", prompt },
        { kind: "steer", prompt: "second" },
      ]);
      await browser.saveScreenshot(
        NodePath.join(preparedArtifactDirectory, "message-queue-restored.png"),
      );

      // Release only after the durable queue has been observed in the new page.
      completeDesktopUiSlowTurn(preparedProjectPath, start.turnId!);
      released = true;
      const promoted = await waitForProviderInputLogEntry(
        preparedProviderInputLogPath,
        baseline + 2,
        {
          provider: "codex",
          prompt: "fourth",
          kind: "start",
        },
      );
      expect(promoted.turnId).toBeTruthy();
      expect(promoted.turnId).not.toBe(start.turnId);
      await assertQueuedCards([]);
      await composerForm()
        .$('button[aria-label="Stop generation"]')
        .waitForExist({ reverse: true });
      expect(inputSequence(baseline)).toEqual([
        { kind: "start", prompt },
        { kind: "steer", prompt: "second" },
        { kind: "start", prompt: "fourth" },
      ]);
      await browser.saveScreenshot(
        NodePath.join(preparedArtifactDirectory, "message-queue-promoted.png"),
      );
    } finally {
      if (slowTurnId && !released) completeDesktopUiSlowTurn(preparedProjectPath, slowTurnId);
    }
  });
});
