// @effect-diagnostics nodeBuiltinImport:off - WDIO configuration manages host test artifacts.
// @effect-diagnostics globalConsole:off - WDIO configuration reports the retained artifact path.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import { copyAppLogEvidence } from "./support/app-log-evidence.ts";
import { installDesktopUiMotionGuard } from "./support/motion-guard.ts";
import { refreshDesktopUiDocument } from "./support/document-navigation.ts";

import { resolveDesktopAppPath, type DesktopUiPlatform } from "./support/app-path.ts";
import { isFinalDesktopUiSpec, requestDesktopUiApplicationExit } from "./support/app-lifecycle.ts";
import {
  deferDesktopUiTestContextCleanupUntilExit,
  prepareDesktopUiTestContext,
} from "./support/test-project.ts";
import { normalizeWebDriverRequest } from "./support/webdriver-request.ts";

// oxlint-disable-next-line bibcode/no-global-process-runtime -- The standalone WDIO config detects the host once and passes it to pure adapters.
const hostPlatform = process.platform;
const platform: DesktopUiPlatform =
  process.env.BIBCODE_E2E_PLATFORM === "mac" ||
  process.env.BIBCODE_E2E_PLATFORM === "linux" ||
  process.env.BIBCODE_E2E_PLATFORM === "win"
    ? process.env.BIBCODE_E2E_PLATFORM
    : hostPlatform === "darwin"
      ? "mac"
      : hostPlatform === "win32"
        ? "win"
        : "linux";
const testContext = prepareDesktopUiTestContext();
const appBinaryPath = resolveDesktopAppPath({
  platform,
  environment: process.env,
});
const requestedSpec = process.env.BIBCODE_E2E_SPEC?.trim();
const desktopUiSpecFiles =
  requestedSpec && requestedSpec.length > 0
    ? [requestedSpec]
    : [
        "./specs/main-window.e2e.ts",
        "./specs/document-navigation.e2e.ts",
        "./specs/project-session-terminal.e2e.ts",
        "./specs/platform-capabilities.e2e.ts",
        "./specs/terminal-font.e2e.ts",
        "./specs/composer-native-triggers.e2e.ts",
        "./specs/composer-message-queue.e2e.ts",
        "./specs/chat-activity-panel.e2e.ts",
        "./specs/pierre-diffs.e2e.ts",
      ];
const desktopUiSpecs = requestedSpec ? desktopUiSpecFiles : [desktopUiSpecFiles];

if (!NodeFS.existsSync(appBinaryPath)) {
  throw new Error(`Packaged BiBCode application does not exist: ${appBinaryPath}`);
}

const screenshotPath = (title: string): string =>
  NodePath.join(
    testContext.artifactDirectory,
    `${title
      .replaceAll(/[^a-z0-9]+/gi, "-")
      .replaceAll(/(^-|-$)/g, "")
      .toLowerCase()}.png`,
  );

async function resetDesktopUiConnectionCache(): Promise<void> {
  const resetResult = await browser.executeAsync(
    (done: (result: { readonly error: string | null }) => void) => {
      window.localStorage.clear();
      window.sessionStorage.clear();
      let settled = false;
      const finish = (error: string | null) => {
        if (settled) return;
        settled = true;
        done({ error });
      };
      const openRequest = indexedDB.open("bibcode:connection-runtime", 2);
      openRequest.addEventListener("error", () => {
        finish(String(openRequest.error ?? "Could not open the E2E connection catalog."));
      });
      openRequest.addEventListener("upgradeneeded", () => {
        finish("The E2E connection catalog schema was unexpectedly missing.");
      });
      openRequest.addEventListener("success", () => {
        const database = openRequest.result;
        const storeNames = ["catalog", "shell", "thread"];
        const transaction = database.transaction(storeNames, "readwrite");
        transaction.addEventListener("error", () => {
          database.close();
          finish(String(transaction.error ?? "Could not reset the E2E connection catalog."));
        });
        transaction.addEventListener("complete", () => {
          database.close();
          finish(null);
        });
        for (const storeName of storeNames) {
          transaction.objectStore(storeName).clear();
        }
      });
    },
  );
  if (resetResult.error !== null) {
    throw new Error(resetResult.error);
  }
  await refreshDesktopUiDocument();
}

export const config = {
  runner: "local",
  specs: desktopUiSpecs,
  maxInstances: 1,
  services: [
    [
      "@wdio/tauri-service",
      {
        appBinaryPath,
        driverProvider: "embedded",
        captureBackendLogs: true,
        embeddedPort: Number(process.env.BIBCODE_E2E_WEBDRIVER_PORT ?? 4_445),
        startTimeout: 90_000,
        statusPollTimeout: 10_000,
        commandTimeout: 60_000,
        logDir: testContext.artifactDirectory,
      },
    ],
  ],
  capabilities: [
    {
      browserName: "tauri",
      "tauri:options": {
        application: appBinaryPath,
      },
    },
  ],
  logLevel: "info",
  outputDir: testContext.artifactDirectory,
  bail: 0,
  waitforTimeout: 20_000,
  connectionRetryTimeout: 120_000,
  connectionRetryCount: 0,
  transformRequest: normalizeWebDriverRequest,
  framework: "mocha",
  reporters: ["spec"],
  mochaOpts: {
    ui: "bdd",
    timeout: 120_000,
  },
  before: async () => {
    const pluginReady = await browser.execute(() => {
      const host = window as Window & {
        readonly wdioTauri?: {
          readonly execute?: unknown;
          readonly waitForInit?: unknown;
        };
      };
      return (
        typeof host.wdioTauri?.execute === "function" &&
        typeof host.wdioTauri.waitForInit === "function"
      );
    });
    if (!pluginReady) {
      throw new Error("The packaged Tauri WebDriver plugin did not initialize.");
    }
  },
  beforeTest: async () => {
    let setupStage = "connection reset and navigation";
    try {
      await resetDesktopUiConnectionCache();
      setupStage = "motion stylesheet";
      await installDesktopUiMotionGuard();
    } catch (error) {
      throw new Error(`Desktop UI setup failed during ${setupStage}: ${String(error)}`, {
        cause: error,
      });
    }
  },
  afterTest: async (
    test: { readonly title: string },
    _context: unknown,
    result: { readonly passed: boolean },
  ) => {
    if (!result.passed) {
      await browser.saveScreenshot(screenshotPath(`failure-${test.title}`));
      NodeFS.writeFileSync(
        screenshotPath(`failure-source-${test.title}`).replace(/\.png$/, ".html"),
        await browser.getPageSource(),
      );
    }
  },
  after: async (_result: unknown, _capabilities: unknown, currentSpecs: ReadonlyArray<string>) => {
    if (!isFinalDesktopUiSpec(currentSpecs, desktopUiSpecFiles)) return;
    // Ask Tauri to run its normal ExitRequested path so the in-process backend and provider
    // children release Windows filesystem handles before the launcher service stops the driver.
    await requestDesktopUiApplicationExit();
  },
  onComplete: () => {
    // Keep the app's own logs (native page loads, webview process failures, server errors)
    // with the screenshots before the fixture state root is removed.
    copyAppLogEvidence(testContext.stateRoot, testContext.artifactDirectory);
    // WDIO invokes configuration hooks before launcher service hooks. Defer shared fixture cleanup
    // until process exit so the Tauri service has released Windows filesystem handles first.
    deferDesktopUiTestContextCleanupUntilExit(testContext, process);
    console.log(`Desktop UI artifacts: ${testContext.artifactDirectory}`);
  },
};
