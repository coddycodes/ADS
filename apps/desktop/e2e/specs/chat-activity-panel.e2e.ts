// @effect-diagnostics nodeBuiltinImport:off - Packaged UI tests retain native acceptance artifacts.
import { installDesktopUiMotionGuard, setDesktopUiMotionMode } from "../support/motion-guard.ts";
import { refreshDesktopUiDocument } from "../support/document-navigation.ts";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import {
  configureDesktopActivityCodexExecutable,
  materializeDesktopActivitySession,
  startDesktopActivityComposerFollowupTurn,
  startDesktopActivityFollowupTurn,
} from "../support/activity-session.ts";
import {
  desktopActivityAccessibleNames,
  desktopActivityFixture,
} from "../support/activity-events.ts";
import {
  describeDesktopUiFocus,
  type DesktopUiFocusedElement,
  type DesktopUiKeyboardResult,
  focusDesktopUiElement,
  sendFocusedKeyboardKey,
} from "../support/keyboard-input.ts";
import { clearDesktopActivityMarker } from "../support/test-project.ts";
import { ensureMainSidebarOpen, setDesktopUiWindowSize } from "../support/ui-state.ts";

const artifactDirectory = process.env.CODDY_STUDIO_E2E_ARTIFACT_DIR;
const projectPath = process.env.CODDY_STUDIO_E2E_PROJECT_PATH;
const shimDirectory = process.env.CODDY_STUDIO_E2E_SHIM_DIRECTORY;
if (!artifactDirectory || !projectPath || !shimDirectory) {
  throw new Error("The packaged desktop UI fixture environment was not prepared.");
}
const preparedArtifactDirectory: string = artifactDirectory;
const preparedProjectPath: string = projectPath;
const isWindowsE2e = process.env.CODDY_STUDIO_E2E_PLATFORM === "win";
const supportsCodexTerminalActivity = !isWindowsE2e;
const preparedCodexExecutable = NodePath.join(shimDirectory, isWindowsE2e ? "codex.cmd" : "codex");

afterEach(async () => {
  try {
    clearDesktopActivityMarker(preparedProjectPath);
  } finally {
    await restoreRuntimeMotionEmulation();
  }
});

interface RectSnapshot {
  readonly bottom: number;
  readonly height: number;
  readonly left: number;
  readonly right: number;
  readonly top: number;
  readonly width: number;
}

interface ActivityGeometry {
  readonly composer: RectSnapshot;
  readonly dock: RectSnapshot;
  readonly workspaceHeader: RectSnapshot;
  readonly inspector: RectSnapshot | null;
  readonly sheet: RectSnapshot | null;
  readonly viewportHeight: number;
  readonly viewportWidth: number;
}

interface SurfacePresentation {
  readonly background: string;
  readonly foreground: string;
  readonly focusRing: string;
}

interface RuntimeMotionPresentation {
  readonly activeTransitionProperties: ReadonlyArray<string>;
  readonly countActiveTransitionProperties: ReadonlyArray<string>;
  readonly runtimeMode: string | undefined;
  readonly transitionDuration: string;
  readonly transitionProperty: string;
}

interface ProviderTerminalGeometry {
  readonly dock: RectSnapshot;
  readonly host: RectSnapshot;
  readonly owner: RectSnapshot;
  readonly toolbar: RectSnapshot;
  readonly viewportHeight: number;
  readonly viewportWidth: number;
}

function rectanglesOverlap(left: RectSnapshot, right: RectSnapshot): boolean {
  return (
    Math.min(left.right, right.right) > Math.max(left.left, right.left) &&
    Math.min(left.bottom, right.bottom) > Math.max(left.top, right.top)
  );
}

function isFullyContainedInViewport(
  rectangle: RectSnapshot,
  viewportWidth: number,
  viewportHeight: number,
): boolean {
  return (
    rectangle.width > 0 &&
    rectangle.height > 0 &&
    rectangle.top >= 0 &&
    rectangle.left >= 0 &&
    rectangle.bottom <= viewportHeight &&
    rectangle.right <= viewportWidth
  );
}

async function openMaterializedFixtureChat(): Promise<{
  readonly projectId: string;
  readonly providerInstanceId: string;
  readonly threadId: string;
}> {
  await ensureMainSidebarOpen();
  const materialized = await materializeDesktopActivitySession(preparedProjectPath);
  await configureDesktopActivityCodexExecutable(preparedCodexExecutable);
  expect(materialized.threadId).toBe(desktopActivityFixture.thread.id);
  expect(materialized.projectId).not.toBe("");
  expect(materialized.providerInstanceId).toBe("codex");
  NodeFS.writeFileSync(
    NodePath.join(preparedArtifactDirectory, "activity-materialization.json"),
    JSON.stringify(
      {
        ...materialized,
      },
      null,
      2,
    ),
  );
  await refreshDesktopUiDocument();
  await installDesktopUiMotionGuard();
  await browser.waitUntil(
    async () => {
      for (const project of await browser.$$(
        `//*[normalize-space()="${desktopActivityFixture.project.title}"]`,
      )) {
        if (await project.isDisplayed()) return true;
      }
      return false;
    },
    { timeoutMsg: "The RPC-materialized activity project did not enter the connected shell." },
  );
  const activityThreadSelector = `//*[starts-with(@data-testid, "thread-row-") and .//*[normalize-space()="${desktopActivityFixture.thread.title}"]]`;
  const activityProject = browser.$(
    `//button[@data-sidebar="menu-button" and .//*[normalize-space()="${desktopActivityFixture.project.title}"]]`,
  );
  if (!(await browser.$(activityThreadSelector).isDisplayed())) {
    await activityProject.waitForDisplayed();
    if ((await activityProject.getAttribute("aria-expanded")) !== "true") {
      await activityProject.click();
    }
    await browser.$(activityThreadSelector).waitForDisplayed({
      timeoutMsg: "The RPC-materialized activity thread did not expand in the main sidebar.",
    });
  }
  let openedActivityThread = false;
  for (const candidate of await browser.$$(activityThreadSelector)) {
    if (await candidate.isDisplayed()) {
      await candidate.click();
      openedActivityThread = true;
      break;
    }
  }
  if (!openedActivityThread) {
    throw new Error("The RPC-materialized activity thread was not visible in the main sidebar.");
  }
  const composer = browser.$('[data-testid="composer-editor"]');
  await composer.waitForExist();
  if (!(await composer.isDisplayed())) {
    const mainPanel = browser.$(
      '//*[@data-center-panel-tab-list]//button[.//span[normalize-space()="Codex"]]',
    );
    if (await mainPanel.isExisting()) {
      await mainPanel.click();
    }
  }
  await composer.waitForDisplayed({
    timeoutMsg: "The RPC-materialized activity thread route did not open.",
  });
  for (;;) {
    const closeTerminal = browser.$('button[aria-label="Close Codex Terminal"]');
    if (!(await closeTerminal.isExisting())) break;
    const previousCount = (await browser.$$('button[aria-label="Close Codex Terminal"]')).length;
    await closeTerminal.click();
    await browser.waitUntil(
      async () =>
        (await browser.$$('button[aria-label="Close Codex Terminal"]')).length < previousCount,
      { timeoutMsg: "A persisted Codex terminal panel did not close." },
    );
  }
  await expect(browser.$('[data-testid="activity-dock"]')).toBeDisplayed();
  const expandedSummary = browser.$(
    `button[aria-label="${desktopActivityAccessibleNames.expandedSummary}"]`,
  );
  if (await expandedSummary.isExisting()) {
    await focusDesktopUiElement(
      `button[aria-label="${desktopActivityAccessibleNames.expandedSummary}"]`,
    );
    await sendFocusedKeyboardKey("Escape");
    await browser
      .$(`button[aria-label="${desktopActivityAccessibleNames.collapsedSummary}"]`)
      .waitForExist();
  }
  return materialized;
}

async function readActivityGeometry(): Promise<ActivityGeometry> {
  return browser.execute(() => {
    const rectangle = (
      selector: string,
      required: boolean,
      allowModalInert = false,
    ): RectSnapshot | null => {
      const element = document.querySelector<HTMLElement>(selector);
      if (!element) {
        if (required) throw new Error(`Required geometry element is missing: ${selector}`);
        return null;
      }
      const style = getComputedStyle(element);
      if (
        element.hidden ||
        (!allowModalInert && element.closest("[hidden], [inert], [aria-hidden='true']")) ||
        style.display === "none" ||
        style.visibility === "hidden" ||
        Number.parseFloat(style.opacity) === 0
      ) {
        throw new Error(`Geometry element is not visibly rendered: ${selector}`);
      }
      const { bottom, height, left, right, top, width } = element.getBoundingClientRect();
      return { bottom, height, left, right, top, width };
    };
    return {
      composer: rectangle('[data-chat-composer-form="true"]', true, true)!,
      dock: rectangle('[data-testid="activity-dock"]', true, true)!,
      workspaceHeader: rectangle("[data-center-panel-group-header]", true, true)!,
      inspector: rectangle("[data-activity-panel]", false),
      sheet: rectangle('[role="dialog"]:has([data-activity-panel])', false),
      viewportHeight: window.innerHeight,
      viewportWidth: window.innerWidth,
    };
  });
}

async function assertBoundedActivityGeometry(
  width: 960 | 980 | 981 | 1_199 | 1_200,
  expectInspector: boolean,
): Promise<void> {
  await setDesktopUiWindowSize(width, 720);
  await browser.waitUntil(
    async () => {
      const candidate = await readActivityGeometry();
      return (
        candidate.viewportWidth === width &&
        [candidate.workspaceHeader, candidate.dock, candidate.composer].every((rectangle) =>
          isFullyContainedInViewport(rectangle, candidate.viewportWidth, candidate.viewportHeight),
        )
      );
    },
    {
      timeoutMsg: `The activity surface did not settle inside the ${width}px viewport after resize.`,
    },
  );
  const geometry = await readActivityGeometry();
  expect(geometry.viewportWidth).toBe(width);
  for (const [name, rectangle] of [
    ["workspace header", geometry.workspaceHeader],
    ["activity dock", geometry.dock],
    ["composer", geometry.composer],
  ] as const) {
    if (!isFullyContainedInViewport(rectangle, geometry.viewportWidth, geometry.viewportHeight)) {
      throw new Error(
        `${name} escaped the ${geometry.viewportWidth}x${geometry.viewportHeight} viewport: ${JSON.stringify(rectangle)}`,
      );
    }
  }
  expect(geometry.dock.top).toBeGreaterThanOrEqual(geometry.workspaceHeader.bottom);
  expect(rectanglesOverlap(geometry.dock, geometry.composer)).toBe(false);
  if (expectInspector) {
    expect(geometry.inspector).not.toBeNull();
    expect(
      isFullyContainedInViewport(
        geometry.inspector!,
        geometry.viewportWidth,
        geometry.viewportHeight,
      ),
    ).toBe(true);
    expect(rectanglesOverlap(geometry.dock, geometry.inspector!)).toBe(false);
    if (width <= 980) {
      expect(geometry.sheet).not.toBeNull();
      expect(
        isFullyContainedInViewport(
          geometry.sheet!,
          geometry.viewportWidth,
          geometry.viewportHeight,
        ),
      ).toBe(true);
      expect(geometry.inspector!.left).toBeGreaterThanOrEqual(geometry.sheet!.left);
      expect(geometry.inspector!.right).toBeLessThanOrEqual(geometry.sheet!.right);
      expect(geometry.inspector!.top).toBeGreaterThanOrEqual(geometry.sheet!.top);
      expect(geometry.inspector!.bottom).toBeLessThanOrEqual(geometry.sheet!.bottom);
      expect(rectanglesOverlap(geometry.dock, geometry.sheet!)).toBe(false);
    } else {
      expect(geometry.sheet).toBeNull();
    }
  } else {
    expect(geometry.inspector).toBeNull();
    expect(geometry.sheet).toBeNull();
  }
}

async function assertActivityPresentation(width: 960 | 980 | 981 | 1_199 | 1_200): Promise<void> {
  await assertBoundedActivityGeometry(width, false);
  const readPresentation = () =>
    browser.execute(() => {
      const dock = [
        ...document.querySelectorAll<HTMLElement>('[data-testid="activity-dock"]'),
      ].find((candidate) => candidate.closest("[data-provider-terminal-activity-host]") === null);
      const summary = dock?.querySelector<HTMLButtonElement>(
        'button[aria-label^="Expand activity summary"]',
      );
      if (!dock || !summary) throw new Error("The chat activity summary is missing.");
      return {
        activeCounts: [...summary.querySelectorAll('[data-activity-count="active"]')].map(
          (element) => element.textContent?.trim() ?? "",
        ),
        doneCounts: [...summary.querySelectorAll('[data-activity-count="done"]')].map(
          (element) => element.textContent?.trim() ?? "",
        ),
        glyphCount: summary.querySelectorAll("[data-activity-provider-glyph]").length,
        text: summary.textContent ?? "",
      };
    });
  await browser.waitUntil(
    async () => {
      const candidate = await readPresentation();
      return width < 1_200
        ? candidate.activeCounts.length === 1
        : candidate.activeCounts.length === 0;
    },
    { timeoutMsg: `Activity dock presentation did not settle at ${width}px.` },
  );
  const presentation = await readPresentation();
  expect(presentation.glyphCount).toBe(1);
  if (width < 1_200) {
    expect(presentation.activeCounts).toEqual(["Active 2"]);
    expect(presentation.doneCounts).toEqual(["Done 0"]);
    expect(presentation.text).toContain("Active 2");
    expect(presentation.text).toContain("Done 0");
  } else {
    expect(presentation.activeCounts).toEqual([]);
    expect(presentation.doneCounts).toEqual([]);
    expect(presentation.text).toContain("Active 2");
    expect(presentation.text).toContain("Done 0");
  }
}

async function openCodexProviderTerminal(): Promise<string> {
  const newPanel = browser.$('button[aria-label="New panel"]');
  await newPanel.waitForDisplayed();
  await newPanel.click();
  const codexTerminal = browser.$(
    '//*[@role="menuitem"][.//span[normalize-space()="Codex Terminal"]]',
  );
  await codexTerminal.waitForDisplayed();
  await codexTerminal.waitForEnabled();
  await codexTerminal.click();
  const host = browser.$("[data-provider-terminal-activity-host]");
  await host.waitForExist({
    timeoutMsg: "The eligible Codex provider terminal did not mount its activity host.",
  });
  const terminalId = await host.getAttribute("data-provider-terminal-activity-host");
  if (!terminalId) throw new Error("The provider terminal activity host has no terminal id.");
  const requestedExecutable = await browser.execute((id: string): string | null => {
    const persisted = window.localStorage.getItem("coddy-studio:center-panel-state:v1");
    if (!persisted) return null;
    const decoded = JSON.parse(persisted) as {
      readonly state?: {
        readonly byThreadKey?: Readonly<
          Record<
            string,
            {
              readonly surfaces?: ReadonlyArray<{
                readonly command?: { readonly executable?: unknown };
                readonly terminalId?: unknown;
              }>;
            }
          >
        >;
      };
    };
    for (const state of Object.values(decoded.state?.byThreadKey ?? {})) {
      const surface = state.surfaces?.find((candidate) => candidate.terminalId === id);
      if (typeof surface?.command?.executable === "string") {
        return surface.command.executable;
      }
    }
    return null;
  }, terminalId);
  expect(requestedExecutable).toBe(preparedCodexExecutable);
  const terminalDock = `[data-provider-terminal-activity-host="${terminalId}"] [data-testid="activity-dock"]`;
  if (supportsCodexTerminalActivity) {
    try {
      await browser.$(terminalDock).waitForDisplayed({
        timeout: 60_000,
        timeoutMsg: "The live Codex terminal activity dock did not become visible.",
      });
    } catch (error) {
      const diagnostics = await browser
        .execute(() => ({
          visibility: document.visibilityState,
          focused: document.hasFocus(),
          elements: [
            ...document.querySelectorAll<HTMLElement>(
              "[data-center-panel-body-target], [data-center-surface-host], [data-provider-terminal-activity-host], [data-provider-terminal-activity-host] [data-testid='activity-dock']",
            ),
          ].map((element) => {
            const rect = element.getBoundingClientRect();
            const style = getComputedStyle(element);
            return {
              target: element.dataset.centerPanelBodyTarget,
              surface: element.dataset.centerSurfaceHost,
              activityHost: element.dataset.providerTerminalActivityHost,
              visible: element.dataset.visible,
              geometry: element.dataset.centerSurfaceGeometry,
              display: style.display,
              visibility: style.visibility,
              width: rect.width,
              height: rect.height,
            };
          }),
        }))
        .catch((diagnosticError: unknown) => ({ error: String(diagnosticError) }));
      throw new Error(
        `The live Codex terminal activity dock did not become visible. Diagnostics: ${JSON.stringify(diagnostics)}`,
        { cause: error },
      );
    }
  } else {
    await expectMissing(terminalDock);
  }
  return terminalId;
}

async function assertProviderTerminalGeometry(terminalId: string): Promise<void> {
  const geometry = await browser.execute((id: string): ProviderTerminalGeometry => {
    const visibleRectangle = (element: HTMLElement | null, label: string): RectSnapshot => {
      if (!element) throw new Error(`Missing ${label}.`);
      const style = getComputedStyle(element);
      if (
        element.hidden ||
        element.closest("[hidden], [inert], [aria-hidden='true']") ||
        style.display === "none" ||
        style.visibility === "hidden" ||
        Number.parseFloat(style.opacity) === 0
      ) {
        throw new Error(`${label} is not visibly rendered.`);
      }
      const { bottom, height, left, right, top, width } = element.getBoundingClientRect();
      return { bottom, height, left, right, top, width };
    };
    const host = document.querySelector<HTMLElement>(
      `[data-provider-terminal-activity-host="${CSS.escape(id)}"]`,
    );
    const owner = host?.closest<HTMLElement>("[data-terminal-owner]") ?? null;
    const toolbar = document.querySelector<HTMLElement>("[data-center-panel-group-header]");
    return {
      dock: visibleRectangle(
        host?.querySelector<HTMLElement>('[data-testid="activity-dock"]') ?? null,
        "provider terminal activity dock",
      ),
      host: visibleRectangle(host, "provider terminal activity host"),
      owner: visibleRectangle(owner, "provider terminal owner"),
      toolbar: visibleRectangle(toolbar, "provider terminal toolbar"),
      viewportHeight: window.innerHeight,
      viewportWidth: window.innerWidth,
    };
  }, terminalId);
  for (const rectangle of [geometry.owner, geometry.host, geometry.toolbar, geometry.dock]) {
    expect(
      isFullyContainedInViewport(rectangle, geometry.viewportWidth, geometry.viewportHeight),
    ).toBe(true);
  }
  expect(geometry.host.left).toBeGreaterThanOrEqual(geometry.owner.left);
  expect(geometry.host.right).toBeLessThanOrEqual(geometry.owner.right);
  expect(geometry.host.top).toBeGreaterThanOrEqual(geometry.toolbar.bottom - 1);
  expect(geometry.dock.left).toBeGreaterThanOrEqual(geometry.host.left);
  expect(geometry.dock.right).toBeLessThanOrEqual(geometry.host.right);
  expect(geometry.dock.top).toBeGreaterThanOrEqual(geometry.host.top);
  expect(rectanglesOverlap(geometry.dock, geometry.toolbar)).toBe(false);
}

async function expectMissing(selector: string): Promise<void> {
  const element = browser.$(selector);
  await element.waitForExist({ reverse: true });
  expect(await element.isExisting()).toBe(false);
}

async function readPersistedProjectDockExpanded(projectId: string): Promise<{
  readonly expanded: boolean;
  readonly projectKey: string;
}> {
  return browser.execute((id: string) => {
    const projectKey = `primary:${id}`;
    const encoded = window.localStorage.getItem("coddy-studio:activity-dock-state:v1");
    const persisted = encoded
      ? (JSON.parse(encoded) as {
          readonly state?: {
            readonly expandedByProject?: Readonly<Record<string, boolean>>;
          };
        })
      : null;
    return {
      expanded: persisted?.state?.expandedByProject?.[projectKey] === true,
      projectKey,
    };
  }, projectId);
}

async function tabTo(
  matches: (element: DesktopUiFocusedElement) => boolean,
  direction: "forward" | "backward" = "forward",
): Promise<DesktopUiFocusedElement> {
  const visited: DesktopUiKeyboardResult[] = [];
  for (let step = 0; step < 20; step += 1) {
    const result = await sendFocusedKeyboardKey("Tab", direction === "backward");
    visited.push(result);
    expect(result.after.tagName).not.toBeNull();
    if (matches(result.after)) return result.after;
  }
  throw new Error(`Tab navigation did not reach the expected control: ${JSON.stringify(visited)}`);
}

async function tabToDockSummary(): Promise<void> {
  const startingFocus = await focusDesktopUiElement('button[aria-label="Toggle right panel"]');
  expect(startingFocus.ariaLabel).toBe("Toggle right panel");
  const arrived = await tabTo(
    (element) => element.ariaLabel === desktopActivityAccessibleNames.collapsedSummary,
  );
  expect(arrived).toEqual(
    expect.objectContaining({
      ariaLabel: desktopActivityAccessibleNames.collapsedSummary,
      tagName: "BUTTON",
    }),
  );
}

async function openSubagentsWithKeyboard(): Promise<{
  readonly enterEvidence: {
    readonly click: number;
    readonly keydown: number;
    readonly keyup: number;
  };
}> {
  await tabToDockSummary();
  const enter = await sendFocusedKeyboardKey("Enter");
  expect(enter.before.ariaLabel).toBe(desktopActivityAccessibleNames.collapsedSummary);
  const enterEvidence = enter.transport === "webdriver" ? enter.driver : enter.synthetic;
  expect(enterEvidence).toEqual(
    expect.objectContaining({
      click: 1,
      keydown: 1,
      keyup: 1,
    }),
  );
  if (enter.transport === "synthetic") {
    expect(enter.driver).toEqual({
      click: 0,
      focusChanged: false,
      keydown: 0,
      keyup: 0,
    });
  }
  await browser
    .$(`button[aria-label="${desktopActivityAccessibleNames.expandedSummary}"]`)
    .waitForExist();
  const subagents = await sendFocusedKeyboardKey("Tab");
  expect(subagents.after).toEqual(
    expect.objectContaining({
      activitySection: "subagents",
      ariaLabel: desktopActivityAccessibleNames.subagents,
      tagName: "BUTTON",
    }),
  );
  const open = await sendFocusedKeyboardKey("Enter");
  expect(open.before.activitySection).toBe("subagents");
  await browser.$("[data-activity-panel]").waitForExist();
  return {
    enterEvidence: {
      click: enterEvidence!.click,
      keydown: enterEvidence!.keydown,
      keyup: enterEvidence!.keyup,
    },
  };
}

async function activityRowVisibility(actorId: string): Promise<{
  readonly hasArea: boolean;
  readonly intersectsViewport: boolean;
}> {
  return browser.execute((id: string) => {
    const row = document.querySelector<HTMLElement>(`[data-activity-row$="${id}"]`);
    if (!row) return { hasArea: false, intersectsViewport: false };
    const rectangle = row.getBoundingClientRect();
    return {
      hasArea: rectangle.width > 0 && rectangle.height > 0,
      intersectsViewport:
        rectangle.bottom > 0 &&
        rectangle.left < window.innerWidth &&
        rectangle.right > 0 &&
        rectangle.top < window.innerHeight,
    };
  }, actorId);
}

function rgbChannels(value: string): [number, number, number] {
  const rgb = value.match(/rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)/i);
  if (rgb) return [Number(rgb[1]), Number(rgb[2]), Number(rgb[3])];
  const color = value.match(/color\(srgb\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)(?:\s*\/\s*[\d.]+)?\)/i);
  if (color) {
    return [Number(color[1]) * 255, Number(color[2]) * 255, Number(color[3]) * 255];
  }
  const oklch = value.match(
    /oklch\(\s*([\d.]+)\s+([\d.]+)\s+(none|[-\d.]+)(?:deg)?(?:\s*\/\s*[\d.]+)?\)/i,
  );
  if (oklch) {
    const lightness = Number(oklch[1]);
    const chroma = Number(oklch[2]);
    const hue = oklch[3] === "none" ? 0 : (Number(oklch[3]) * Math.PI) / 180;
    const a = chroma * Math.cos(hue);
    const b = chroma * Math.sin(hue);
    const l = (lightness + 0.3963377774 * a + 0.2158037573 * b) ** 3;
    const m = (lightness - 0.1055613458 * a - 0.0638541728 * b) ** 3;
    const s = (lightness - 0.0894841775 * a - 1.291485548 * b) ** 3;
    const linear = [
      4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
      -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
      -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
    ];
    return linear.map((channel) => {
      const clamped = Math.max(0, Math.min(1, channel));
      return 255 * (clamped <= 0.0031308 ? 12.92 * clamped : 1.055 * clamped ** (1 / 2.4) - 0.055);
    }) as [number, number, number];
  }
  throw new Error(`Unsupported computed color: ${value}`);
}

function contrastRatio(foreground: string, background: string): number {
  const luminance = (value: string): number => {
    const channels = rgbChannels(value).map((channel) => {
      const normalized = channel / 255;
      return normalized <= 0.04045 ? normalized / 12.92 : ((normalized + 0.055) / 1.055) ** 2.4;
    });
    return channels[0]! * 0.2126 + channels[1]! * 0.7152 + channels[2]! * 0.0722;
  };
  const left = luminance(foreground);
  const right = luminance(background);
  return (Math.max(left, right) + 0.05) / (Math.min(left, right) + 0.05);
}

async function selectActualTheme(
  theme: "Dark" | "Light",
  threadId: string,
): Promise<{ readonly dock: SurfacePresentation; readonly panel: SurfacePresentation }> {
  const origin = await browser.execute(() => window.location.origin);
  await browser.url(`${origin}/#/settings/general`);
  const themePreference = browser.$('[aria-label="Theme preference"]');
  await themePreference.waitForDisplayed();
  await themePreference.click();
  const option = browser.$(`//*[@role="option" and normalize-space()="${theme}"]`);
  await option.waitForDisplayed();
  await option.click();
  await browser.waitUntil(
    async () => {
      const rootClass = (await browser.$("html").getAttribute("class")) ?? "";
      return theme === "Dark" ? rootClass.includes("dark") : !rootClass.includes("dark");
    },
    { timeoutMsg: `The actual ${theme.toLowerCase()} theme did not become authoritative.` },
  );
  await browser.url(`${origin}/#/primary/${encodeURIComponent(threadId)}`);
  await browser.$('[data-testid="activity-dock"]').waitForDisplayed();
  await tabToDockSummary();
  const dock = await browser.execute((): SurfacePresentation => {
    const summary = document.activeElement;
    const card = summary?.closest<HTMLElement>('[data-testid="activity-dock"] > div') ?? null;
    if (!(summary instanceof HTMLElement) || !card) {
      throw new Error("Cannot inspect the focused activity theme surface.");
    }
    const cardStyle = getComputedStyle(card);
    const focusStyle = getComputedStyle(summary);
    return {
      background: cardStyle.backgroundColor,
      foreground: cardStyle.color,
      focusRing: `${focusStyle.outlineColor}|${focusStyle.boxShadow}`,
    };
  });
  await openSubagentsWithKeyboard();
  await browser.$("[data-activity-panel]").waitForDisplayed();
  await focusDesktopUiElement("[data-activity-panel] button:not([disabled])");
  const panel = await browser.execute((): SurfacePresentation => {
    const root = document.querySelector<HTMLElement>("[data-activity-panel]");
    const focused = document.activeElement;
    if (!root || !(focused instanceof HTMLElement) || !root.contains(focused)) {
      throw new Error("Cannot inspect the focused activity panel theme surface.");
    }
    const rootStyle = getComputedStyle(root);
    const focusStyle = getComputedStyle(focused);
    return {
      background: rootStyle.backgroundColor,
      foreground: rootStyle.color,
      focusRing: `${focusStyle.outlineColor}|${focusStyle.boxShadow}`,
    };
  });
  const rightPanelToggle = browser.$('button[aria-label="Toggle right panel"]');
  await rightPanelToggle.click();
  await expectMissing("[data-activity-panel]");
  return { dock, panel };
}

const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

async function installRuntimeMotionEmulation(initialMatches: boolean): Promise<void> {
  await browser.execute(
    (query: string, initial: boolean) => {
      interface RuntimeMotionEmulation {
        readonly nativeMatchMedia: typeof window.matchMedia;
        readonly mediaQuery: MediaQueryList;
        readonly restore: () => void;
        readonly set: (matches: boolean) => void;
      }
      const testWindow = window as Window & {
        __coddyStudioDesktopUiReducedMotionEmulation?: RuntimeMotionEmulation;
      };
      testWindow.__coddyStudioDesktopUiReducedMotionEmulation?.restore();
      const nativeMatchMedia = window.matchMedia;
      const listeners = new Set<EventListenerOrEventListenerObject>();
      let matches = initial;
      let onchange: ((this: MediaQueryList, event: MediaQueryListEvent) => unknown) | null = null;
      const dispatch = (
        listener: EventListenerOrEventListenerObject,
        event: MediaQueryListEvent,
      ) => {
        if (typeof listener === "function") {
          listener.call(mediaQuery, event);
        } else {
          listener.handleEvent(event);
        }
      };
      const mediaQuery = {
        get matches() {
          return matches;
        },
        media: query,
        get onchange() {
          return onchange;
        },
        set onchange(listener) {
          onchange = listener;
        },
        addEventListener: (type: string, listener: EventListenerOrEventListenerObject | null) => {
          if (type === "change" && listener !== null) listeners.add(listener);
        },
        removeEventListener: (
          type: string,
          listener: EventListenerOrEventListenerObject | null,
        ) => {
          if (type === "change" && listener !== null) listeners.delete(listener);
        },
        addListener: (listener: (event: MediaQueryListEvent) => void) => {
          listeners.add(listener as EventListener);
        },
        removeListener: (listener: (event: MediaQueryListEvent) => void) => {
          listeners.delete(listener as EventListener);
        },
        dispatchEvent: (event: Event) => {
          const mediaEvent = event as MediaQueryListEvent;
          for (const listener of listeners) dispatch(listener, mediaEvent);
          onchange?.call(mediaQuery, mediaEvent);
          return !event.defaultPrevented;
        },
      } as MediaQueryList;
      const state: RuntimeMotionEmulation = {
        nativeMatchMedia,
        mediaQuery,
        restore: () => {
          window.matchMedia = nativeMatchMedia;
          listeners.clear();
          onchange = null;
          delete testWindow.__coddyStudioDesktopUiReducedMotionEmulation;
        },
        set: (nextMatches: boolean) => {
          if (matches === nextMatches) return;
          matches = nextMatches;
          mediaQuery.dispatchEvent(
            new MediaQueryListEvent("change", {
              matches: nextMatches,
              media: query,
            }),
          );
        },
      };
      testWindow.__coddyStudioDesktopUiReducedMotionEmulation = state;
      window.matchMedia = (media: string) =>
        media === query ? mediaQuery : nativeMatchMedia.call(window, media);
    },
    REDUCED_MOTION_QUERY,
    initialMatches,
  );
}

async function restoreRuntimeMotionEmulation(): Promise<void> {
  await browser.execute(() => {
    const testWindow = window as Window & {
      __coddyStudioDesktopUiReducedMotionEmulation?: {
        readonly restore: () => void;
      };
    };
    testWindow.__coddyStudioDesktopUiReducedMotionEmulation?.restore();
  });
}

async function remountActivityDockWithRuntimeMotionEmulation(threadId: string): Promise<void> {
  await installRuntimeMotionEmulation(false);
  await browser.execute(() => {
    window.location.hash = "#/settings/general";
  });
  await browser.$('[aria-label="Theme preference"]').waitForDisplayed();
  await expectMissing('[data-testid="activity-dock"]');
  await browser.execute((nextThreadId: string) => {
    window.location.hash = `#/primary/${encodeURIComponent(nextThreadId)}`;
  }, threadId);
  await browser.$('[data-testid="activity-dock"]').waitForDisplayed();
}

async function setRuntimeMotionPreference(matches: boolean): Promise<void> {
  await browser.execute((nextMatches: boolean) => {
    const state = (
      window as Window & {
        __coddyStudioDesktopUiReducedMotionEmulation?: {
          readonly set: (matches: boolean) => void;
        };
      }
    ).__coddyStudioDesktopUiReducedMotionEmulation;
    if (!state) throw new Error("The reduced-motion emulation is not installed.");
    state.set(nextMatches);
  }, matches);
}

async function readRuntimeMotionPresentation(): Promise<RuntimeMotionPresentation> {
  return browser.execute(() => {
    const activeTransitionProperties = (element: Element): ReadonlyArray<string> => {
      const computed = getComputedStyle(element);
      const properties = computed.transitionProperty.split(",").map((value) => value.trim());
      const durations = computed.transitionDuration.split(",").map((value) => {
        const duration = Number.parseFloat(value);
        return value.trim().endsWith("ms") ? duration / 1_000 : duration;
      });
      return properties.filter(
        (_, index) => (durations[index % Math.max(1, durations.length)] ?? 0) > 0,
      );
    };
    const card = document.querySelector<HTMLElement>('[data-testid="activity-dock"] > div');
    if (!card) throw new Error("Activity card is missing for runtime motion inspection.");
    const computed = getComputedStyle(card);
    return {
      activeTransitionProperties: activeTransitionProperties(card),
      countActiveTransitionProperties: [
        ...document.querySelectorAll<HTMLElement>(
          '[data-testid="activity-dock"] [data-activity-count], [data-testid="activity-dock"] .tabular-nums',
        ),
      ].flatMap(activeTransitionProperties),
      runtimeMode: card.dataset.activityMotion,
      transitionDuration: computed.transitionDuration,
      transitionProperty: computed.transitionProperty,
    };
  });
}

async function assertRuntimeMotionState(reduced: boolean): Promise<void> {
  await setRuntimeMotionPreference(reduced);
  let presentation: RuntimeMotionPresentation | null = null;
  await browser.waitUntil(
    async () => {
      presentation = await readRuntimeMotionPresentation();
      return (
        presentation.runtimeMode === (reduced ? "reduced" : "normal") &&
        (reduced
          ? presentation.activeTransitionProperties.length === 0
          : presentation.activeTransitionProperties.includes("width") &&
            presentation.activeTransitionProperties.includes("opacity")) &&
        presentation.countActiveTransitionProperties.length === 0
      );
    },
    {
      timeoutMsg: `Activity motion did not render the ${reduced ? "reduced" : "normal"} computed transition state.`,
    },
  );
  expect(presentation).not.toBeNull();
  expect(presentation!.runtimeMode).toBe(reduced ? "reduced" : "normal");
  expect(presentation!.countActiveTransitionProperties).toEqual([]);
  if (reduced) {
    expect(presentation!.activeTransitionProperties).toEqual([]);
  } else {
    expect(presentation!.transitionProperty).toContain("width");
    expect(presentation!.transitionProperty).toContain("opacity");
    expect(presentation!.transitionDuration).not.toBe("0s");
  }
}

async function assertRuntimeMotionContract(): Promise<void> {
  await setDesktopUiMotionMode("native");
  try {
    await assertRuntimeMotionState(false);
    await assertRuntimeMotionState(true);
    await assertRuntimeMotionState(false);
  } finally {
    await setDesktopUiMotionMode("disabled");
  }
}

describe("packaged responsive activity experience", () => {
  it("keeps activity responsive, keyboard-operable, and focus-stable in the packaged app", async () => {
    clearDesktopActivityMarker(preparedProjectPath);
    await setDesktopUiWindowSize(1_200, 720);
    const materialized = await openMaterializedFixtureChat();

    const liveRegions = await browser.$$(
      '[data-testid="activity-dock"] [role="status"][aria-live="polite"][aria-atomic="true"]',
    );
    expect(liveRegions).toHaveLength(1);
    await expect(liveRegions[0]!).toHaveText(
      "Activity update: 1 active subagent, 0 done subagents, 1 active background task, 0 done background tasks",
    );

    const light = await selectActualTheme("Light", materialized.threadId);
    expect(contrastRatio(light.dock.foreground, light.dock.background)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(light.panel.foreground, light.panel.background)).toBeGreaterThanOrEqual(
      4.5,
    );
    expect(light.dock.focusRing).not.toBe("rgba(0, 0, 0, 0)|none");
    expect(light.panel.focusRing).not.toBe("rgba(0, 0, 0, 0)|none");
    const dark = await selectActualTheme("Dark", materialized.threadId);
    expect(contrastRatio(dark.dock.foreground, dark.dock.background)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(dark.panel.foreground, dark.panel.background)).toBeGreaterThanOrEqual(4.5);
    expect(dark.dock.focusRing).not.toBe("rgba(0, 0, 0, 0)|none");
    expect(dark.panel.focusRing).not.toBe("rgba(0, 0, 0, 0)|none");
    expect(dark.dock.background).not.toBe(light.dock.background);
    expect(dark.dock.foreground).not.toBe(light.dock.foreground);
    expect(dark.panel.background).not.toBe(light.panel.background);
    expect(dark.panel.foreground).not.toBe(light.panel.foreground);
    await remountActivityDockWithRuntimeMotionEmulation(materialized.threadId);
    await assertRuntimeMotionContract();

    for (const width of [960, 980, 981, 1_199, 1_200] as const) {
      await assertActivityPresentation(width);
    }

    const inlineKeyboard = await openSubagentsWithKeyboard();
    expect(inlineKeyboard.enterEvidence).toEqual({
      click: 1,
      keydown: 1,
      keyup: 1,
    });
    await expect(browser.$("[data-activity-panel]")).toBeDisplayed();
    await expectMissing('[role="dialog"] [data-activity-panel]');
    await assertBoundedActivityGeometry(1_200, true);

    const rightPanelToggle = 'button[aria-label="Toggle right panel"]';
    expect(await focusDesktopUiElement(rightPanelToggle)).toEqual(
      expect.objectContaining({ ariaLabel: "Toggle right panel", tagName: "BUTTON" }),
    );
    await sendFocusedKeyboardKey("Enter");
    await expectMissing("[data-activity-panel]");
    await focusDesktopUiElement(rightPanelToggle);
    await sendFocusedKeyboardKey("Enter");
    await expect(browser.$("[data-activity-panel]")).toBeDisplayed();
    await expect(browser.$('section[aria-label="Subagents"]')).toBeDisplayed();

    await focusDesktopUiElement(
      `button[aria-label="${desktopActivityAccessibleNames.collapsedSummary}"]`,
    );
    await sendFocusedKeyboardKey("Enter");
    await browser
      .$(`button[aria-label="${desktopActivityAccessibleNames.expandedSummary}"]`)
      .waitForExist();
    expect(await readPersistedProjectDockExpanded(materialized.projectId)).toEqual({
      expanded: true,
      projectKey: `primary:${materialized.projectId}`,
    });
    await setDesktopUiWindowSize(960, 720);
    await expect(browser.$('[role="dialog"] [data-activity-panel]')).toBeDisplayed();
    expect(
      await focusDesktopUiElement('[role="dialog"] [data-activity-panel] button:not([disabled])'),
    ).toEqual(
      expect.objectContaining({
        tagName: "BUTTON",
      }),
    );
    const collapse = await sendFocusedKeyboardKey("Escape");
    expect(collapse.before.tagName).toBe("BUTTON");
    await expect(browser.$('[role="dialog"] [data-activity-panel]')).toBeDisplayed();
    await browser
      .$(`button[aria-label="${desktopActivityAccessibleNames.collapsedSummary}"]`)
      .waitForExist();
    expect(await readPersistedProjectDockExpanded(materialized.projectId)).toEqual({
      expanded: false,
      projectKey: `primary:${materialized.projectId}`,
    });
    expect(
      await browser.execute(() => document.activeElement?.closest('[role="dialog"]') !== null),
    ).toBe(true);
    await assertBoundedActivityGeometry(960, true);
    await sendFocusedKeyboardKey("Escape");
    await expectMissing('[role="dialog"] [data-activity-panel]');

    expect(await focusDesktopUiElement(rightPanelToggle)).toEqual(
      expect.objectContaining({ ariaLabel: "Toggle right panel", tagName: "BUTTON" }),
    );
    await sendFocusedKeyboardKey("Enter");
    await expect(browser.$('[role="dialog"] [data-activity-panel]')).toBeDisplayed();

    const actor = browser.$(`[data-activity-row$="${desktopActivityFixture.actor.id}"]`);
    await actor.waitForExist();
    await browser.waitUntil(async () => {
      const visibility = await activityRowVisibility(desktopActivityFixture.actor.id);
      return visibility.hasArea && visibility.intersectsViewport;
    });
    expect(await activityRowVisibility(desktopActivityFixture.actor.id)).toEqual({
      hasArea: true,
      intersectsViewport: true,
    });
    await expect(actor).toHaveText(expect.stringContaining(desktopActivityFixture.actor.name));
    await expect(actor).toHaveText(expect.stringContaining("Running"));

    const focusedActor = await tabTo(
      (element) => element.activityRow?.endsWith(desktopActivityFixture.actor.id) === true,
    );
    expect(focusedActor.activityRow).toEqual(
      expect.stringContaining(desktopActivityFixture.actor.id),
    );
    await sendFocusedKeyboardKey("Enter");
    await expect(browser.$("[data-activity-detail-heading]")).toBeDisplayed();
    const back = await tabTo((element) => element.ariaLabel === "Back to Subagents", "backward");
    expect(back.ariaLabel).toBe("Back to Subagents");
    await sendFocusedKeyboardKey("Enter");
    await expectMissing("[data-activity-detail-heading]");
    expect(
      (await sendFocusedKeyboardKey("ArrowDown")).before.activityRow?.endsWith(
        desktopActivityFixture.actor.id,
      ),
    ).toBe(true);

    await sendFocusedKeyboardKey("Escape");
    await expectMissing('[role="dialog"] [data-activity-panel]');

    await setDesktopUiWindowSize(1_200, 720);
    const terminalId = await openCodexProviderTerminal();
    if (supportsCodexTerminalActivity) {
      await assertProviderTerminalGeometry(terminalId);
    }
    const terminalInput =
      '[data-terminal-owner="center-panel"] .xterm-helper-textarea, [data-terminal-owner="right-panel"] .xterm-helper-textarea';
    expect(await focusDesktopUiElement(terminalInput)).toEqual(
      expect.objectContaining({ tagName: "TEXTAREA" }),
    );
    await startDesktopActivityFollowupTurn();
    const followupMessage = browser.$(
      '//*[normalize-space()="publish deterministic live activity update"]',
    );
    await followupMessage.waitForExist({
      timeoutMsg: "The deterministic follow-up turn did not reach the active chat.",
    });
    expect(await describeDesktopUiFocus()).toEqual(
      expect.objectContaining({ tagName: "TEXTAREA" }),
    );
    const updatedSummary = browser.$(
      'button[aria-label*="1 active subagent"][aria-label*="0 done subagents"][aria-label*="0 active background tasks"][aria-label*="1 done background task"]',
    );
    await updatedSummary.waitForExist({
      timeoutMsg: "The provider follow-up did not materially update activity counts.",
    });
    const updatedAnnouncement =
      "Activity update: 1 active subagent, 0 done subagents, 0 active background tasks, 1 done background task";
    await browser.waitUntil(
      async () =>
        browser.execute((announcement: string) => {
          const liveRegions = document.querySelectorAll<HTMLElement>(
            '[data-testid="activity-dock"] [role="status"][aria-live="polite"]',
          );
          return [...liveRegions].some(
            (liveRegion) =>
              liveRegion.closest("[data-provider-terminal-activity-host]") === null &&
              liveRegion.textContent?.trim() === announcement,
          );
        }, updatedAnnouncement),
      { timeoutMsg: "The coalesced live announcement did not publish updated exact status." },
    );
    if (supportsCodexTerminalActivity) {
      await assertProviderTerminalGeometry(terminalId);
    }
    const mainPanel = browser.$(
      '//*[@data-center-panel-tab-list]//button[.//span[normalize-space()="Codex"]]',
    );
    await mainPanel.click();
    await browser.$('[data-testid="composer-editor"]').waitForDisplayed();
    expect(await focusDesktopUiElement('[data-testid="composer-editor"]')).toEqual(
      expect.objectContaining({ testId: "composer-editor" }),
    );
    await browser.execute(() => {
      const composer = document.activeElement;
      if (!(composer instanceof HTMLElement) || composer.dataset.testid !== "composer-editor") {
        throw new Error("The composer must own focus before the second activity update.");
      }
      const state = {
        composer,
        lost: false,
        onFocusIn: () => {
          if (document.activeElement !== composer) state.lost = true;
        },
      };
      (
        window as Window & {
          __coddyStudioActivityComposerFocusGuard?: typeof state;
        }
      ).__coddyStudioActivityComposerFocusGuard = state;
      document.addEventListener("focusin", state.onFocusIn, true);
    });
    await startDesktopActivityComposerFollowupTurn();
    const composerUpdateAnnouncement =
      "Activity update: 2 active subagents, 0 done subagents, 0 active background tasks, 1 done background task";
    await browser.waitUntil(
      async () =>
        browser.execute((announcement: string) => {
          const summary = document.querySelector<HTMLButtonElement>(
            'button[aria-label*="2 active subagents"][aria-label*="0 done subagents"]',
          );
          const liveRegion = document.querySelector<HTMLElement>(
            '[data-testid="activity-dock"] [role="status"][aria-live="polite"]',
          );
          return summary !== null && liveRegion?.textContent?.trim() === announcement;
        }, composerUpdateAnnouncement),
      {
        timeoutMsg:
          "The composer-focused activity revision did not update counts and announcement.",
      },
    );
    const composerFocus = await browser.execute(() => {
      const guardedWindow = window as Window & {
        __coddyStudioActivityComposerFocusGuard?: {
          readonly composer: HTMLElement;
          lost: boolean;
          readonly onFocusIn: () => void;
        };
      };
      const state = guardedWindow.__coddyStudioActivityComposerFocusGuard;
      if (!state) throw new Error("The composer focus guard was lost.");
      document.removeEventListener("focusin", state.onFocusIn, true);
      delete guardedWindow.__coddyStudioActivityComposerFocusGuard;
      return {
        focused: document.activeElement === state.composer,
        lost: state.lost,
      };
    });
    expect(composerFocus).toEqual({ focused: true, lost: false });

    await browser.saveScreenshot(
      NodePath.join(preparedArtifactDirectory, "chat-activity-responsive.png"),
    );
  });
});
