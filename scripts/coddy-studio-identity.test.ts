// @effect-diagnostics nodeBuiltinImport:off - Repository identity guard scans Git-owned files directly.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import { describe, expect, it } from "vite-plus/test";

const REPOSITORY_ROOT = NodePath.resolve(import.meta.dirname, "..");
const SELF = "scripts/coddy-studio-identity.test.ts";
const TEXT_EXTENSIONS = new Set([
  "",
  ".css",
  ".csv",
  ".example",
  ".html",
  ".json",
  ".md",
  ".mjs",
  ".ps1",
  ".rs",
  ".sh",
  ".sql",
  ".toml",
  ".ts",
  ".tsx",
  ".txt",
  ".yaml",
  ".yml",
]);

const removedIdentityPatterns = [
  new RegExp(["t", "3", "code"].join(""), "i"),
  new RegExp(["t", "3", "\\s+code"].join(""), "i"),
  new RegExp(["@t", "3", "tools"].join(""), "i"),
  new RegExp(["t", "3", "tools"].join(""), "i"),
  new RegExp(["(?<![A-Za-z0-9])t", "3", "(?![A-Za-z0-9])"].join(""), "i"),
  new RegExp(["t", "3", "_"].join(""), "i"),
  new RegExp(["t", "3", "env"].join(""), "i"),
  new RegExp(["urn:t", "3"].join(""), "i"),
  new RegExp(["%3At", "3", "%3A"].join(""), "i"),
  new RegExp(["(?<![A-Za-z0-9_])T", "4", "(?:\\s*Code)?(?![A-Za-z0-9_])"].join("")),
  new RegExp(["t", "4", "\\s*code"].join(""), "i"),
];

const compatibilityFiles = new Set([
  ".gitignore",
  "apps/desktop/src-tauri/src/backend.rs",
  "apps/desktop/src-tauri/src/config.rs",
  "apps/desktop/src-tauri/src/ssh.rs",
  "apps/server/src/auth/model.rs",
  "apps/server/src/auth/service.rs",
  "apps/server/src/config.rs",
  "apps/server/src/environment_identity.rs",
  "apps/server/src/http.rs",
  "apps/server/src/logging.rs",
  "apps/server/src/production/jwt.rs",
  "apps/server/src/production/relay.rs",
  "apps/server/src/provider_terminal/claude.rs",
  "apps/server/src/provider_terminal/supervisor.rs",
  "apps/server/src/provider_usage/mod.rs",
  "apps/server/src/source_control/pull_request.rs",
  "apps/server/src/terminal/model.rs",
  "apps/server/src/terminal/osc.rs",
  "apps/server/src/terminal/pty.rs",
  "apps/server/tests/auth_http.rs",
  "apps/server/tests/identity_paths.rs",
  "apps/server/tests/server_runtime.rs",
  "apps/web/index.html",
  "apps/web/src/cloud/dpop.test.ts",
  "apps/web/src/cloud/dpop.ts",
  "apps/web/src/components/terminalTheme.test.ts",
  "apps/web/src/connection/storage.test.ts",
  "apps/web/src/connection/storage.ts",
  "apps/web/src/hooks/useLocalStorage.test.ts",
  "apps/web/src/lib/storage.test.ts",
  "apps/web/src/lib/storage.ts",
  "apps/web/src/tauriDesktopBridge.test.ts",
  "apps/web/src/tauriDesktopBridge.ts",
  "apps/web/src/uiStateStore.ts",
  "apps/web/vite.config.app.mjs",
  "infra/relay/src/auth/RelayTokens.test.ts",
  "packages/contracts/src/relay.test.ts",
  "packages/contracts/src/relay.ts",
  "packages/shared/src/environmentIdentity.test.ts",
  "packages/shared/src/environmentIdentity.ts",
  "packages/shared/src/relayJwt.test.ts",
  "packages/shared/src/relayJwt.ts",
  "README.md",
  "scripts/build-desktop-artifact.test.ts",
  "scripts/lib/public-config.test.ts",
  "scripts/lib/public-config.ts",
  "docs/superpowers/specs/2026-08-01-provider-maintenance-rust-design.md",
]);

const explicitPredecessorDocumentationFiles = new Set([
  "docs/architecture/overview.md",
  "docs/guides/project-data-recovery.md",
  "docs/superpowers/plans/2026-08-09-project-data-safety.md",
]);
const documentedPredecessorPattern = new RegExp(
  ["(?<![A-Za-z0-9_])T", "4", "(?:\\s*Code)?(?![A-Za-z0-9_])"].join(""),
  "gi",
);

function projectFiles(): string[] {
  return NodeChildProcess.execFileSync(
    "git",
    [
      "ls-files",
      "--cached",
      "--others",
      "--exclude-standard",
      "--",
      ".",
      ":(exclude).repos/**",
      ":(exclude).tmp/**",
    ],
    { cwd: REPOSITORY_ROOT, encoding: "utf8" },
  )
    .split(/\r?\n/u)
    .filter(Boolean)
    .filter((path) => path !== SELF)
    .filter((path) => !path.startsWith(".repos/"))
    .filter((path) => !path.startsWith(".tmp/"))
    .filter((path) => {
      const absolutePath = NodePath.join(REPOSITORY_ROOT, path);
      return NodeFS.existsSync(absolutePath) && NodeFS.statSync(absolutePath).isFile();
    });
}

function firstMatch(value: string): string | null {
  return removedIdentityPatterns.find((pattern) => pattern.test(value))?.source ?? null;
}

describe("Coddy Studio identity", () => {
  it("keeps tracked source files free of literal NUL bytes", () => {
    const findings: string[] = [];
    for (const path of projectFiles()) {
      const extension = NodePath.extname(path).toLowerCase();
      if (!TEXT_EXTENSIONS.has(extension)) continue;
      const content = NodeFS.readFileSync(NodePath.join(REPOSITORY_ROOT, path));
      const nulOffset = content.indexOf(0);
      if (nulOffset >= 0) {
        findings.push(`${path}: byte ${String(nulOffset)}`);
      }
    }

    expect(findings, findings.join("\n")).toEqual([]);
  });

  it("contains no removed predecessor identity outside compatibility files", () => {
    const findings: string[] = [];
    for (const path of projectFiles()) {
      const normalizedPath = path.replaceAll("\\", "/");
      if (compatibilityFiles.has(normalizedPath)) continue;
      const pathMatch = firstMatch(normalizedPath);
      if (pathMatch) {
        findings.push(`${normalizedPath}: path matches /${pathMatch}/i`);
      }

      const extension = NodePath.extname(path).toLowerCase();
      if (!TEXT_EXTENSIONS.has(extension)) continue;
      const absolutePath = NodePath.join(REPOSITORY_ROOT, path);
      const content = NodeFS.readFileSync(absolutePath, "utf8");
      for (const [index, line] of content.split(/\r?\n/u).entries()) {
        const contentMatch = firstMatch(
          explicitPredecessorDocumentationFiles.has(normalizedPath)
            ? line.replaceAll(documentedPredecessorPattern, "")
            : line,
        );
        if (contentMatch) {
          findings.push(`${normalizedPath}:${String(index + 1)} matches /${contentMatch}/i`);
        }
      }
    }

    expect(findings, findings.slice(0, 200).join("\n")).toEqual([]);
  });
});
