import * as NodeCrypto from "node:crypto";
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeFSP from "node:fs/promises";
import * as NodeHttp from "node:http";
import * as NodeModule from "node:module";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import { normalizeWebDriverRequest } from "../../../desktop/e2e/support/webdriver-request.ts";

export async function serveGallery(directory) {
  const server = NodeHttp.createServer(async (request, response) => {
    try {
      const path = new URL(request.url, "http://localhost").pathname;
      const candidate = NodePath.resolve(
        directory,
        `.${decodeURIComponent(path === "/" ? "/index.html" : path)}`,
      );
      if (!candidate.startsWith(`${NodePath.resolve(directory)}${NodePath.sep}`))
        throw new Error("Gallery path refused.");
      const extension = NodePath.extname(candidate);
      response.setHeader(
        "Content-Type",
        {
          ".html": "text/html",
          ".js": "text/javascript",
          ".css": "text/css",
          ".woff": "font/woff",
          ".woff2": "font/woff2",
        }[extension] ?? "application/octet-stream",
      );
      response.end(await NodeFSP.readFile(candidate));
    } catch {
      response.writeHead(404);
      response.end();
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Gallery listener refused.");
  return {
    url: `http://127.0.0.1:${address.port}`,
    close: () =>
      new Promise((resolve, reject) =>
        server.close((error) => (error ? reject(error) : resolve())),
      ),
  };
}

export async function interactWithFixture(browser, actions) {
  for (const action of actions) {
    if (action.action === "escape") {
      await browser.keys("Escape");
      continue;
    }
    const findSelector = () =>
      browser.execute((action) => {
        const candidates = [
          ...document.querySelectorAll(
            action.selector ?? "button, input, textarea, [role], label, h1, h2, h3, p",
          ),
        ].filter((element) => {
          const role =
            element.getAttribute("role") ??
            { BUTTON: "button", INPUT: "textbox", TEXTAREA: "textbox" }[element.tagName];
          const labelId = element.getAttribute("aria-labelledby");
          const label =
            element.getAttribute("aria-label") ??
            (labelId
              ? labelId
                  .split(" ")
                  .map((id) => document.getElementById(id)?.textContent ?? "")
                  .join(" ")
              : null) ??
            (element.id
              ? document.querySelector(`label[for="${CSS.escape(element.id)}"]`)?.textContent
              : null) ??
            element.textContent;
          const name = label?.trim().replace(/\s+/g, " ");
          const rect = element.getBoundingClientRect();
          const expected = action.name ?? action.text;
          return (
            rect.width > 0 &&
            rect.height > 0 &&
            (!action.role || role === action.role) &&
            (action.name_prefix
              ? name?.startsWith(action.name_prefix)
              : expected === undefined || name === expected)
          );
        });
        const element = candidates[action.index ?? 0];
        if (!element) return null;
        element.setAttribute("data-typography-action", "current");
        return "[data-typography-action='current']";
      }, action);
    await browser.waitUntil(async () => (await findSelector()) !== null, { timeout: 5_000 });
    const selector = await findSelector();
    if (!selector) throw new Error("Typography interaction target missing.");
    const element = await browser.$(selector);
    await element.waitForDisplayed({ timeout: 10_000 });
    if (action.action === "click") await element.click();
    else if (action.action === "press") {
      await browser.execute((element) => element.focus(), element);
      await browser.keys(action.key);
    } else if (action.action === "fill") await element.setValue(action.value);
    else if (action.action === "hover") await element.moveTo();
    else if (action.action === "focus")
      await browser.execute((element) => element.focus(), element);
    else if (action.action === "scrollIntoView") await element.scrollIntoView({ block: "center" });
    else throw new Error("Typography interaction refused.");
    await browser.execute(() =>
      document
        .querySelectorAll("[data-typography-action]")
        .forEach((element) => element.removeAttribute("data-typography-action")),
    );
  }
}

export async function captureFixtures({
  browser,
  gallery,
  output,
  source,
  engine,
  selectedIds,
  group,
  interact = interactWithFixture,
  progress = () => undefined,
}) {
  if (group && !["core", "git", "settings"].includes(group))
    throw new Error("Typography group refused.");
  const observations = [];
  const failures = [];
  const scope = {
    kind: group ? "grouped-fixtures" : selectedIds ? "selected-fixtures" : "all-catalog",
    group: group ?? null,
    fixtures: selectedIds ?? null,
  };
  await NodeFSP.mkdir(output, { recursive: true });
  try {
    progress("viewport");
    await browser.setWindowSize(1440, 1200);
    await browser.setTimeout({ pageLoad: 20_000, script: 15_000 });
    const paint = engine === "webkitgtk" ? "&paint=linux-webkit" : "";
    progress("gallery-start");
    await browser.url(`${gallery.url}/?fixture=badge&theme=light${paint}`);
    await browser.waitUntil(
      async () =>
        browser.execute(() => document.getElementById("typography-fixture-metadata") !== null),
      { timeout: 15_000 },
    );
    const catalog = await browser.execute(() =>
      JSON.parse(document.getElementById("typography-fixture-metadata").textContent),
    );
    const belongsToGroup = ({ id }) =>
      group === "git"
        ? /^(git-|pr-)/.test(id)
        : group === "settings"
          ? /^(settings-|status-)/.test(id)
          : !/^(git-|pr-|settings-|status-)/.test(id);
    const fixtures = selectedIds
      ? catalog.filter(({ id }) => selectedIds.includes(id))
      : group
        ? catalog.filter(belongsToGroup)
        : catalog;
    scope.fixtures = fixtures.map(({ id }) => id);
    if (fixtures.length === 0 || (selectedIds && fixtures.length !== selectedIds.length))
      throw new Error("Capture selection refused.");
    for (const fixture of fixtures) {
      for (const theme of ["light", "dark"]) {
        progress(`${fixture.id}-${theme}`);
        await browser.url(`${gallery.url}/?fixture=${fixture.id}&theme=${theme}${paint}`);
        await browser.executeAsync((done) =>
          Promise.all([
            document.fonts.load('12px "DM Sans Variable"'),
            document.fonts.load('12px "JetBrains Mono"'),
          ])
            .then(() => document.fonts.ready)
            .then(() => done(true)),
        );
        for (const state of fixture.captureStates ?? [
          { state: "initial", actions: [], visible_text: fixture.anchors },
        ]) {
          let stage = "interaction";
          let witnessFailure;
          try {
            await interact(browser, state.actions);
            await browser.executeAsync((done) =>
              requestAnimationFrame(() =>
                requestAnimationFrame(() => {
                  const transitions = document
                    .getAnimations()
                    .filter(
                      (animation) => animation.effect?.getComputedTiming().iterations !== Infinity,
                    );
                  Promise.allSettled(transitions.map((animation) => animation.finished)).then(() =>
                    done(true),
                  );
                }),
              ),
            );
            await browser.executeAsync((done) =>
              Promise.all([
                document.fonts.load('12px "DM Sans Variable"'),
                document.fonts.load('12px "JetBrains Mono"'),
              ])
                .then(() => document.fonts.ready)
                .then(() => done(true)),
            );
            stage = "visible-text";
            await browser.waitUntil(
              async () =>
                browser.execute((anchors) => {
                  if (document.documentElement.dataset.fixtureStatus === "failed") return false;
                  const section = document.querySelector("[data-typography-fixture]");
                  const roots = [
                    section,
                    ...document.querySelectorAll(
                      "[data-slot$='popup'], [data-slot='menu-sub-content'], [role='dialog'], [role='listbox'], [role='tooltip']",
                    ),
                  ].filter(Boolean);
                  return anchors.every((anchor) =>
                    roots.some((root) =>
                      [...root.querySelectorAll("*")].some((element) => {
                        const rect = element.getBoundingClientRect();
                        const style = getComputedStyle(element);
                        const ownText = [...element.childNodes]
                          .filter((node) => node.nodeType === Node.TEXT_NODE)
                          .map((node) => node.textContent)
                          .join("")
                          .trim();
                        const controlText =
                          (element instanceof HTMLInputElement ||
                            element instanceof HTMLTextAreaElement) &&
                          element.value === ""
                            ? element.placeholder
                            : "";
                        if (
                          !`${ownText}${controlText}`.includes(anchor) ||
                          rect.width <= 0 ||
                          rect.height <= 0 ||
                          rect.left < 0 ||
                          rect.top < 0 ||
                          rect.right > innerWidth ||
                          rect.bottom > innerHeight ||
                          Number.parseFloat(style.fontSize) < 12
                        )
                          return false;
                        for (let parent = element; parent; parent = parent.parentElement) {
                          const computed = getComputedStyle(parent);
                          if (
                            computed.visibility !== "visible" ||
                            computed.display === "none" ||
                            Number.parseFloat(computed.opacity) === 0
                          )
                            return false;
                          if (
                            /(hidden|clip|auto|scroll)/.test(
                              `${computed.overflowX} ${computed.overflowY}`,
                            )
                          ) {
                            const clip = parent.getBoundingClientRect();
                            if (
                              rect.left < clip.left - 1 ||
                              rect.right > clip.right + 1 ||
                              rect.top < clip.top - 1 ||
                              rect.bottom > clip.bottom + 1
                            )
                              return false;
                          }
                        }
                        return true;
                      }),
                    ),
                  );
                }, state.visible_text),
              {
                timeout: 5_000,
                timeoutMsg: `Visible typography anchors missing: ${fixture.id}/${state.state}`,
              },
            );
            stage = "witness";
            const witness = await browser.execute(() => ({
              fixture: document.documentElement.dataset.fixtureId,
              status: document.documentElement.dataset.fixtureStatus,
              dark: document.documentElement.classList.contains("dark"),
              linuxWebkitGate: document.documentElement.hasAttribute("data-linux-webkit"),
              userAgent: navigator.userAgent,
              viewport: {
                width: window.innerWidth,
                height: window.innerHeight,
                devicePixelRatio: window.devicePixelRatio,
              },
              fonts: {
                dmSans: document.fonts.check('12px "DM Sans Variable"'),
                jetbrains: document.fonts.check('12px "JetBrains Mono"'),
              },
            }));
            witnessFailure = {
              fixtureMatches: witness.fixture === fixture.id,
              renderingFailed: witness.status === "failed",
              themeMatches: witness.dark === (theme === "dark"),
              fonts: witness.fonts,
              linuxWebkitGate: witness.linuxWebkitGate,
            };
            if (
              witness.fixture !== fixture.id ||
              witness.status === "failed" ||
              witness.dark !== (theme === "dark") ||
              !witness.fonts.dmSans ||
              !witness.fonts.jetbrains
            )
              throw new Error("Typography capture witness refused.");
            if (engine === "webkitgtk" && !witness.linuxWebkitGate)
              throw new Error("Native Linux WebKit fixture gate missing.");
            const filename = `${fixture.id}-${state.state}-${theme}.png`;
            stage = "image-save";
            const bytes = await browser.saveScreenshot(NodePath.join(output, filename));
            stage = "source-hash";
            const hashes = Object.fromEntries(
              await Promise.all(
                fixture.modules.map(async (path) => [
                  path,
                  NodeCrypto.createHash("sha256")
                    .update(
                      await NodeFSP.readFile(
                        NodePath.resolve(import.meta.dirname, "../../src", path),
                      ),
                    )
                    .digest("hex"),
                ]),
              ),
            );
            observations.push({
              source,
              engine,
              id: fixture.id,
              state: state.state,
              theme,
              modules: hashes,
              owners: state.owners ?? fixture.owners,
              anchors: state.visible_text,
              witness,
              filename,
              sha256: NodeCrypto.createHash("sha256").update(bytes).digest("hex"),
            });
          } catch {
            failures.push({
              id: fixture.id,
              state: state.state,
              theme,
              status: "capture-refused",
              stage,
              ...(stage === "witness" ? { witnessFailure } : {}),
            });
          }
          await NodeFSP.writeFile(
            NodePath.join(output, "manifest.json"),
            JSON.stringify(
              { source, engine, scope, complete: false, observations, failures },
              null,
              2,
            ),
          );
        }
      }
    }
    await NodeFSP.writeFile(
      NodePath.join(output, "manifest.json"),
      JSON.stringify(
        { source, engine, scope, complete: failures.length === 0, observations, failures },
        null,
        2,
      ),
    );
    if (failures.length > 0)
      throw new Error(
        `Typography capture refused ${failures.length} states; inspect the source-joined manifest.`,
      );
    return observations;
  } finally {
    try {
      await browser.deleteSession();
    } finally {
      await gallery.close();
    }
  }
}

async function main() {
  if (process.argv[4] === "selected" && !process.argv[5])
    throw new Error("Typography selected fixture IDs missing.");
  if (process.env.GITHUB_ACTIONS !== "true") throw new Error("Native browser capture is CI-only.");
  const engine = process.argv[2];
  if (engine !== "chromium" && engine !== "webkitgtk") throw new Error("Capture engine refused.");
  const source = process.env.BIBCODE_TYPOGRAPHY_SOURCE;
  if (!source || !/^[a-f0-9]{40}$/.test(source)) throw new Error("Capture source refused.");
  const output = NodePath.resolve(process.argv[3] ?? `typography-artifacts/${engine}`);
  await NodeFSP.mkdir(output, { recursive: true });
  const bootstrap = async (phase) => {
    process.stdout.write(`TYPOGRAPHY_STAGE ${engine} ${phase}\n`);
    await NodeFSP.writeFile(
      NodePath.join(output, "bootstrap.json"),
      JSON.stringify({ source, engine, phase }),
    );
  };
  await bootstrap("runtime-import");
  if (engine === "chromium") {
    const chrome = "/usr/bin/google-chrome";
    const driver = "/usr/local/share/chromedriver-linux64/chromedriver";
    await NodeFSP.access(chrome, NodeFS.constants.X_OK);
    await NodeFSP.access(driver, NodeFS.constants.X_OK);
    const chromeVersion = NodeChildProcess.execFileSync(chrome, ["--version"], {
      encoding: "utf8",
      timeout: 10_000,
    });
    const driverVersion = NodeChildProcess.execFileSync(driver, ["--version"], {
      encoding: "utf8",
      timeout: 10_000,
    });
    if (chromeVersion.match(/\d+/)?.[0] !== driverVersion.match(/\d+/)?.[0])
      throw new Error("Runner Chrome and driver versions differ.");
  }
  const requireDesktop = NodeModule.createRequire(
    NodePath.resolve(import.meta.dirname, "../../../desktop/package.json"),
  );
  const { remote } = await import(
    NodeURL.pathToFileURL(requireDesktop.resolve("webdriverio")).href
  );
  const gallery = await serveGallery(
    NodePath.resolve(import.meta.dirname, "../../dist/typography"),
  );
  let browser;
  await bootstrap("driver-start");
  try {
    browser = await remote(
      engine === "chromium"
        ? {
            connectionRetryTimeout: 15_000,
            connectionRetryCount: 0,
            transformRequest: normalizeWebDriverRequest,
            capabilities: {
              browserName: "chrome",
              "wdio:enforceWebDriverClassic": true,
              "wdio:chromedriverOptions": {
                binary: "/usr/local/share/chromedriver-linux64/chromedriver",
              },
              "goog:chromeOptions": {
                binary: "/usr/bin/google-chrome",
                args: ["--headless=new", "--disable-dev-shm-usage"],
              },
            },
            logLevel: "error",
          }
        : {
            hostname: "127.0.0.1",
            port: 4444,
            connectionRetryTimeout: 15_000,
            connectionRetryCount: 0,
            transformRequest: normalizeWebDriverRequest,
            capabilities: {
              browserName: "MiniBrowser",
              "wdio:enforceWebDriverClassic": true,
              "webkitgtk:browserOptions": {
                binary: "/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1/MiniBrowser",
                args: ["--automation"],
              },
            },
            logLevel: "error",
          },
    );
  } catch (error) {
    await bootstrap("driver-refused");
    await gallery.close();
    throw error;
  }
  await bootstrap("driver-ready");
  await captureFixtures({
    browser,
    gallery,
    output,
    source,
    engine,
    selectedIds:
      process.argv[4] === "smoke"
        ? ["badge"]
        : process.argv[4] === "selected"
          ? process.argv[5]?.split(",")
          : undefined,
    group: process.argv[4] === "selected" ? undefined : process.argv[5],
    progress: (stage) => process.stdout.write(`TYPOGRAPHY_STAGE ${engine} ${stage}\n`),
  });
}

if (process.argv[1] && import.meta.url === NodeURL.pathToFileURL(process.argv[1]).href)
  await main();
