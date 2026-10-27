// @effect-diagnostics nodeBuiltinImport:off - Packaged UI tests inspect disposable Git state and save screenshots.
import * as NodeChildProcess from "node:child_process";
import * as NodePath from "node:path";

import {
  pierreVisualFixture,
  preparePierreVisualFixture,
} from "../support/pierre-visual-fixture.ts";
import { desktopUiFixture } from "../support/test-project.ts";
import {
  ensureMainSidebarOpen,
  mockDesktopUiFolderPicker,
  setDesktopUiWindowSize,
} from "../support/ui-state.ts";

const artifactDirectory = process.env.CODDY_STUDIO_E2E_ARTIFACT_DIR;
const projectPath = process.env.CODDY_STUDIO_E2E_PROJECT_PATH;

if (!artifactDirectory || !projectPath) {
  throw new Error("The packaged desktop Pierre fixture environment was not prepared.");
}

const preparedArtifactDirectory: string = artifactDirectory;
const preparedProjectPath: string = projectPath;
const reviewComment = "Packaged Pierre conversation review";

interface ShadowTargetSnapshot {
  readonly centerX: number;
  readonly centerY: number;
  readonly height: number;
  readonly text: string;
  readonly width: number;
}

function git(args: ReadonlyArray<string>): string {
  const result = NodeChildProcess.spawnSync("git", ["-C", preparedProjectPath, ...args], {
    encoding: "utf8",
    shell: false,
  });
  if (result.error !== undefined) throw result.error;
  if (result.status !== 0) {
    throw new Error(`Packaged Pierre fixture Git command failed: ${result.stderr}`);
  }
  return result.stdout;
}

async function saveEvidence(name: string): Promise<void> {
  await browser.saveScreenshot(NodePath.join(preparedArtifactDirectory, `${name}.png`));
}

async function readShadowTarget(
  hostSelector: string,
  targetSelector: string,
): Promise<ShadowTargetSnapshot | null> {
  return browser.execute(
    (hostQuery: string, targetQuery: string) => {
      const hosts = [...document.querySelectorAll<HTMLElement>(hostQuery)];
      const host = hosts.find((candidate) => {
        const rect = candidate.getBoundingClientRect();
        return rect.width > 0 && rect.height > 0;
      });
      const target = host?.shadowRoot?.querySelector<HTMLElement>(targetQuery);
      if (target === undefined || target === null) return null;
      const rect = target.getBoundingClientRect();
      return {
        centerX: Math.round(rect.left + rect.width / 2),
        centerY: Math.round(rect.top + rect.height / 2),
        height: rect.height,
        text: target.textContent ?? "",
        width: rect.width,
      };
    },
    hostSelector,
    targetSelector,
  );
}

async function waitForShadowTarget(
  hostSelector: string,
  targetSelector: string,
  timeoutMsg: string,
): Promise<ShadowTargetSnapshot> {
  let snapshot: ShadowTargetSnapshot | null = null;
  try {
    await browser.waitUntil(
      async () => {
        snapshot = await readShadowTarget(hostSelector, targetSelector);
        return snapshot !== null && snapshot.width > 0 && snapshot.height > 0;
      },
      { timeoutMsg },
    );
  } catch (error) {
    const diagnostics = await browser.execute(
      (hostQuery: string, targetQuery: string) =>
        [...document.querySelectorAll<HTMLElement>(hostQuery)].map((host) => {
          const rect = host.getBoundingClientRect();
          const shadowRoot = host.shadowRoot;
          const target = shadowRoot?.querySelector<HTMLElement>(targetQuery) ?? null;
          return {
            hostHeight: rect.height,
            hostWidth: rect.width,
            hasShadowRoot: shadowRoot !== null,
            shadowChildren: shadowRoot
              ? [...shadowRoot.children].map((child) => ({
                  attributes: [...child.attributes].map((attribute) => [
                    attribute.name,
                    attribute.value,
                  ]),
                  tagName: child.tagName,
                }))
              : [],
            targetFound: target !== null,
          };
        }),
      hostSelector,
      targetSelector,
    );
    throw new Error(`${timeoutMsg} Diagnostics: ${JSON.stringify(diagnostics)}`, {
      cause: error,
    });
  }
  if (snapshot === null) throw new Error(timeoutMsg);
  return snapshot;
}

async function pointAtShadowTarget(
  hostSelector: string,
  targetSelector: string,
  click: boolean,
): Promise<void> {
  await waitForShadowTarget(
    hostSelector,
    targetSelector,
    `The shadow target did not become visible: ${targetSelector}`,
  );
  const dispatched = await browser.execute(
    (hostQuery: string, targetQuery: string, activate: boolean) => {
      const host = [...document.querySelectorAll<HTMLElement>(hostQuery)].find((candidate) => {
        const rect = candidate.getBoundingClientRect();
        return rect.width > 0 && rect.height > 0;
      });
      const target = host?.shadowRoot?.querySelector<HTMLElement>(targetQuery);
      if (target === undefined || target === null) return false;
      const rect = target.getBoundingClientRect();
      const pointer = (type: string, buttons: number) =>
        target.dispatchEvent(
          new PointerEvent(type, {
            bubbles: true,
            button: 0,
            buttons,
            clientX: Math.round(rect.left + rect.width / 2),
            clientY: Math.round(rect.top + rect.height / 2),
            composed: true,
            isPrimary: true,
            pointerId: 73,
            pointerType: "mouse",
          }),
        );
      pointer("pointermove", 0);
      if (activate) {
        target.focus({ preventScroll: true });
        pointer("pointerdown", 1);
        pointer("pointerup", 0);
        target.click();
      }
      return true;
    },
    hostSelector,
    targetSelector,
    click,
  );
  if (!dispatched) {
    throw new Error(`The shadow pointer target disappeared: ${targetSelector}`);
  }
}

async function replaceShadowEditorText(
  hostSelector: string,
  targetSelector: string,
  text: string,
): Promise<void> {
  await pointAtShadowTarget(hostSelector, targetSelector, true);
  const result = await browser.execute(
    (hostQuery: string, targetQuery: string, replacement: string) => {
      const host = [...document.querySelectorAll<HTMLElement>(hostQuery)].find((candidate) => {
        const rect = candidate.getBoundingClientRect();
        return rect.width > 0 && rect.height > 0;
      });
      const target = host?.shadowRoot?.querySelector<HTMLElement>(targetQuery);
      if (target === undefined || target === null) return { found: false, handled: false };
      // Dispatch the selection shortcut to the editor itself. WebView2 can
      // leave WebDriver keyboard focus on the file tree after a shadow click.
      target.focus({ preventScroll: true });
      const selectAll = new KeyboardEvent("keydown", {
        key: "a",
        code: "KeyA",
        ctrlKey: !navigator.platform.includes("Mac"),
        metaKey: navigator.platform.includes("Mac"),
        bubbles: true,
        cancelable: true,
        composed: true,
      });
      target.dispatchEvent(selectAll);
      if (!selectAll.defaultPrevented) return { found: true, handled: false };
      const input = new InputEvent("beforeinput", {
        bubbles: true,
        cancelable: true,
        composed: true,
        data: replacement,
        inputType: "insertText",
      });
      target.dispatchEvent(input);
      return { found: true, handled: input.defaultPrevented };
    },
    hostSelector,
    targetSelector,
    text,
  );
  if (!result.found || !result.handled) {
    throw new Error(`Pierre did not handle the packaged editor input: ${JSON.stringify(result)}`);
  }
}

async function openFixtureWorkspace(): Promise<void> {
  await ensureMainSidebarOpen();
  const project = browser.$(
    `//button[.//span[normalize-space()="${desktopUiFixture.projectName}"]]`,
  );
  if (!(await project.isDisplayed())) {
    const projectDataLoading = browser.$("//*[normalize-space()='Project data is still loading']");
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
    await mockDesktopUiFolderPicker(preparedProjectPath);
    await browseFolder.click();
  }
  await project.waitForDisplayed();
  const primaryWorkspace = browser.$(
    '//*[@data-thread-item="true"][.//span[normalize-space()="main"]]',
  );
  if (!(await primaryWorkspace.isDisplayed())) {
    await project.click();
  }
  await primaryWorkspace.waitForDisplayed();
  await primaryWorkspace.click();
  await browser.$('[data-testid="composer-editor"]').waitForDisplayed();
}

async function openRightPanelSurface(label: "Diff" | "Files"): Promise<void> {
  const rightPanel = browser.$("[data-right-panel-tabbar]");
  if (!(await rightPanel.isDisplayed())) {
    const toggle = browser.$('button[aria-label^="Toggle right panel"]');
    await toggle.waitForDisplayed();
    await toggle.click();
    await rightPanel.waitForDisplayed();
  }

  const existingTab = browser.$(
    `//*[@data-right-panel-tab-list]//button[normalize-space()="${label}"]`,
  );
  if (await existingTab.isDisplayed()) {
    await existingTab.click();
    return;
  }

  const emptyStateAction = browser.$(
    `//button[.//span[normalize-space()="${label}"] and not(@aria-disabled="true")]`,
  );
  if (await emptyStateAction.isDisplayed()) {
    await emptyStateAction.click();
    return;
  }

  const addSurface = browser.$('button[aria-label="Add panel surface"]');
  await addSurface.waitForDisplayed();
  await addSurface.click();
  const menuItem = browser.$(`//*[@role="menuitem" and normalize-space()="${label}"]`);
  await menuItem.waitForDisplayed();
  await menuItem.click();
}

async function closeRightPanel(): Promise<void> {
  const rightPanel = browser.$("[data-right-panel-tabbar]");
  if (!(await rightPanel.isDisplayed())) return;
  const toggle = browser.$('button[aria-label^="Toggle right panel"]');
  await toggle.click();
  await rightPanel.waitForDisplayed({ reverse: true });
}

async function expectConversationDiff(): Promise<void> {
  await browser.waitUntil(
    () =>
      browser.execute((expectedComment: string) => {
        for (const host of document.querySelectorAll("diffs-container")) {
          let ancestor: HTMLElement | null = host.parentElement;
          for (let depth = 0; ancestor !== null && depth < 8; depth += 1) {
            if (ancestor.textContent?.includes(expectedComment)) {
              return (
                host.shadowRoot?.querySelector('pre[data-diff-type="single"]') !== null &&
                host.getBoundingClientRect().height > 0
              );
            }
            ancestor = ancestor.parentElement;
          }
        }
        return false;
      }, reviewComment),
    {
      timeoutMsg: "The sent review comment did not render its unified conversation diff.",
    },
  );
}

describe("packaged Pierre diff and editor interactions", () => {
  it("covers diff review, partial staging, editor history, and conversation rendering", async () => {
    preparePierreVisualFixture(preparedProjectPath);
    await setDesktopUiWindowSize(1_800, 1_050);
    await openFixtureWorkspace();

    await openRightPanelSurface("Diff");
    const diffScope = browser.$('button[aria-label^="Diff scope:"]');
    await diffScope.waitForDisplayed();
    await diffScope.click();
    const workingTree = browser.$(
      '//*[@role="menuitem" and .//*[normalize-space()="Working tree"]]',
    );
    await workingTree.waitForDisplayed();
    await workingTree.click();

    const diffHostSelector = "[data-preview-panel-mode] diffs-container";
    await waitForShadowTarget(
      diffHostSelector,
      'pre[data-diff-type="single"]',
      "The packaged working-tree diff did not render in unified mode.",
    );
    const additionGutterSelector =
      '[data-gutter] [data-line-type="change-addition"][data-column-number]';
    await pointAtShadowTarget(diffHostSelector, additionGutterSelector, false);
    const gutterUtilitySelector = "[data-gutter-utility-slot] [data-utility-button]";
    await waitForShadowTarget(
      diffHostSelector,
      gutterUtilitySelector,
      "Hovering a Pierre diff line did not reveal the gutter utility.",
    );
    await saveEvidence("pierre-unified-hover");

    await pointAtShadowTarget(diffHostSelector, additionGutterSelector, true);
    await waitForShadowTarget(
      diffHostSelector,
      "[data-selected-line]",
      "The Pierre gutter gesture did not select its diff line.",
    );
    const commentInput = browser.$('textarea[aria-label^="Comment on lines"]');
    await commentInput.waitForDisplayed();
    await saveEvidence("pierre-unified-selection");
    await commentInput.setValue(reviewComment);
    const saveComment = browser.$('//button[normalize-space()="Comment"]');
    await saveComment.click();
    await browser.$(`//*[normalize-space()="${reviewComment}"]`).waitForDisplayed();

    const splitToggle = browser.$('button[aria-label="Split diff view"]');
    await splitToggle.click();
    await waitForShadowTarget(
      diffHostSelector,
      'pre[data-diff-type="split"]',
      "The packaged diff did not switch to split mode.",
    );
    await saveEvidence("pierre-split-diff");

    const composer = browser.$('[data-testid="composer-editor"]');
    await composer.setValue("Review the selected packaged diff.");
    const send = browser.$('button[aria-label="Send message"]');
    await send.waitForEnabled();
    await send.click();
    await browser
      .$(`//*[contains(normalize-space(), "${desktopUiFixture.streamedResponse}")]`)
      .waitForDisplayed();
    await closeRightPanel();
    await expectConversationDiff();
    await saveEvidence("pierre-conversation-diff");

    await ensureMainSidebarOpen();
    const project = browser.$(
      `//button[.//span[normalize-space()="${desktopUiFixture.projectName}"]]`,
    );
    await project.waitForDisplayed();
    const pullRequestsOpened = await browser.execute((label: string) => {
      const button = document.querySelector<HTMLButtonElement>(
        `button[aria-label="Pull Requests for ${CSS.escape(label)}"]`,
      );
      button?.click();
      return button !== null;
    }, desktopUiFixture.projectName);
    expect(pullRequestsOpened).toBe(true);
    await browser.waitUntil(async () => (await browser.getUrl()).includes("/pull-requests"), {
      timeoutMsg: "The packaged Pull Requests route did not open.",
    });
    await saveEvidence("pierre-pull-requests-route");

    const gitManagerOpened = await browser.execute((label: string) => {
      const button = document.querySelector<HTMLButtonElement>(
        `button[aria-label="Git Manager for ${CSS.escape(label)}"]`,
      );
      button?.click();
      return button !== null;
    }, desktopUiFixture.projectName);
    expect(gitManagerOpened).toBe(true);
    await browser.waitUntil(async () => (await browser.getUrl()).includes("/git"), {
      timeoutMsg: "The packaged Git Manager route did not open.",
    });
    const changesTab = browser.$('//button[@role="tab" and normalize-space()="Changes"]');
    await changesTab.waitForDisplayed();
    await changesTab.click();

    const changeRow = browser.$(`[role="option"][data-path="${pierreVisualFixture.diffFileName}"]`);
    await changeRow.waitForDisplayed();
    await changeRow.click();
    const stagingGutter = browser.$('aside[aria-label="Partial staging selection gutter"]');
    await stagingGutter.waitForDisplayed();
    const firstRun = stagingGutter.$(
      'button[aria-label="Toggle changed-line run starting at line 1"]',
    );
    await firstRun.waitForEnabled();
    await firstRun.click();
    const stageSelected = stagingGutter.$('.//button[normalize-space()="Stage selected lines"]');
    await stageSelected.waitForEnabled();
    await stageSelected.click();
    await browser.waitUntil(
      () => {
        const staged = git(["diff", "--cached", "--", pierreVisualFixture.diffFileName]);
        const unstaged = git(["diff", "--", pierreVisualFixture.diffFileName]);
        return (
          staged.includes('first = "changed one"') &&
          !staged.includes('second = "changed two"') &&
          !staged.includes('third = "changed three"') &&
          !unstaged.includes('first = "changed one"') &&
          unstaged.includes('second = "changed two"') &&
          unstaged.includes('third = "changed three"')
        );
      },
      { timeoutMsg: "The packaged partial-stage action did not stage exactly the first hunk." },
    );

    const stagedArea = browser.$(
      `//section[@aria-label="Diff for ${pierreVisualFixture.diffFileName}"]//button[normalize-space()="Staged"]`,
    );
    await stagedArea.waitForDisplayed();
    await stagedArea.click();
    await browser
      .$(
        'aside[aria-label="Partial staging selection gutter"] button[aria-label^="Toggle changed-line run"]',
      )
      .waitForEnabled();
    await waitForShadowTarget(
      `section[aria-label="Diff for ${pierreVisualFixture.diffFileName}"] diffs-container`,
      'pre[data-diff-type="single"]',
      "The staged Pierre diff did not finish rendering.",
    );
    await saveEvidence("pierre-partial-stage");

    const stagedRun = browser.$(
      'aside[aria-label="Partial staging selection gutter"] button[aria-label^="Toggle changed-line run"]',
    );
    await stagedRun.click();
    const unstageSelected = browser.$(
      '//aside[@aria-label="Partial staging selection gutter"]//button[normalize-space()="Unstage selected lines"]',
    );
    await unstageSelected.waitForEnabled();
    await saveEvidence("pierre-partial-unstage-selection");
    await unstageSelected.click();
    await browser.waitUntil(
      () => {
        const staged = git(["diff", "--cached", "--", pierreVisualFixture.diffFileName]);
        const unstaged = git(["diff", "--", pierreVisualFixture.diffFileName]);
        return (
          staged.trim().length === 0 &&
          unstaged.includes('first = "changed one"') &&
          unstaged.includes('second = "changed two"') &&
          unstaged.includes('third = "changed three"')
        );
      },
      { timeoutMsg: "The packaged partial-unstage action did not restore all three hunks." },
    );
    await waitForShadowTarget(
      `section[aria-label="Diff for ${pierreVisualFixture.diffFileName}"] diffs-container`,
      'pre[data-diff-type="single"]',
      "The unstaged Pierre diff did not finish rendering.",
    );
    await saveEvidence("pierre-partial-unstage");

    await openFixtureWorkspace();
    await openRightPanelSurface("Files");
    const fileTreeHostSelector = "[data-preview-panel-mode] file-tree-container";
    const editTreeItemSelector = `[role="treeitem"][data-item-path="${pierreVisualFixture.editFileName}"]`;
    await pointAtShadowTarget(fileTreeHostSelector, editTreeItemSelector, true);
    const editorHostSelector = "[data-preview-panel-mode] diffs-container";
    const editorSelector = `[role="textbox"][aria-label="${pierreVisualFixture.editFileName}"]`;
    await waitForShadowTarget(
      editorHostSelector,
      editorSelector,
      "The packaged editable Pierre file did not open.",
    );
    await replaceShadowEditorText(
      editorHostSelector,
      editorSelector,
      pierreVisualFixture.editedFileContents.trimEnd(),
    );
    await browser.waitUntil(
      async () =>
        (await readShadowTarget(editorHostSelector, editorSelector))?.text.includes(
          "edited packaged text",
        ) === true,
      { timeoutMsg: "The packaged Pierre editor did not accept the deterministic edit." },
    );
    await browser.$('button[aria-label="Undo"]').waitForEnabled();
    await saveEvidence("pierre-edit-before-tab-switch");

    const diffTab = browser.$('//*[@data-right-panel-tab-list]//button[normalize-space()="Diff"]');
    await diffTab.click();
    await browser.$('button[aria-label^="Diff scope:"]').waitForDisplayed();
    const editFileTab = browser.$(
      `//*[@data-right-panel-tab-list]//button[normalize-space()="${pierreVisualFixture.editFileName}"]`,
    );
    await editFileTab.click();
    await browser.waitUntil(
      async () =>
        (await readShadowTarget(editorHostSelector, editorSelector))?.text.includes(
          "edited packaged text",
        ) === true,
      { timeoutMsg: "The edited Pierre document did not survive the right-panel tab switch." },
    );
    const undo = browser.$('button[aria-label="Undo"]');
    await undo.waitForEnabled();
    await saveEvidence("pierre-edit-history-after-tab-switch");
    await undo.click();
    await browser.waitUntil(
      async () =>
        (await readShadowTarget(editorHostSelector, editorSelector))?.text.includes(
          "original packaged text",
        ) === true,
      { timeoutMsg: "The Pierre undo history did not survive the right-panel tab switch." },
    );
    await browser.$('button[aria-label="Redo"]').waitForEnabled();
    await saveEvidence("pierre-edit-history-undo");
  });
});
