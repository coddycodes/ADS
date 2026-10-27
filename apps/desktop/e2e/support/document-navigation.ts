// @effect-diagnostics nodeBuiltinImport:off - Packaged UI tests observe the native page-load log.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

export function readNativePageLoad(log: string): { phase: string; id: string } | undefined {
  const events = [
    ...log.matchAll(/desktop_e2e_page_load_(started|finished) id=([0-9a-f-]{36})(?=\s|$)/g),
  ];
  const last = events.at(-1);
  return last ? { phase: last[1]!, id: last[2]! } : undefined;
}

export async function refreshDesktopUiDocument(
  stateRoot = process.env.CODDY_STUDIO_HOME,
): Promise<void> {
  if (!stateRoot) throw new Error("The desktop UI fixture state root was not configured.");
  const path = NodePath.join(stateRoot, "userdata", "logs", "server.log");
  const read = () => {
    try {
      return readNativePageLoad(NodeFS.readFileSync(path, "utf8"));
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
      throw error;
    }
  };
  let previous: string | undefined;
  await browser.waitUntil(
    () => {
      const current = read();
      if (current?.phase !== "finished") return false;
      previous = current.id;
      return true;
    },
    { timeoutMsg: "Tauri did not report the current desktop document finishing its load." },
  );
  await browser.refresh();
  // WebDriver JavaScript requests can be lost during navigation. Observe the
  // test-only native lifecycle log before sending any script to the new page.
  await browser.waitUntil(
    () => {
      const current = read();
      return current?.phase === "finished" && current.id !== previous;
    },
    { timeoutMsg: "Tauri did not report the replacement desktop document finishing its load." },
  );
  // WebKitGTK can report the native finish while the new DOM is interactive.
  // It is now safe to install a load listener in that replacement document.
  await browser.executeAsync((done: (result: string) => void) => {
    if (document.readyState === "complete") {
      done("ready");
      return;
    }
    window.addEventListener("load", () => done("ready"), { once: true });
  });
}
