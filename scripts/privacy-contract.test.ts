// @effect-diagnostics nodeBuiltinImport:off - Privacy tests inspect checked-in source.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { describe, expect, it } from "vite-plus/test";

const root = NodePath.resolve(import.meta.dirname, "..");
const ignoredDirectoryNames = new Set([
  ".alchemy",
  ".repos",
  ".vite-plus",
  "coverage",
  "dist",
  "fixtures",
  "node_modules",
  "target",
]);
const sourceExtensions = new Set([
  ".astro",
  ".bat",
  ".cjs",
  ".cmd",
  ".css",
  ".env",
  ".fish",
  ".html",
  ".js",
  ".json",
  ".mjs",
  ".ps1",
  ".rs",
  ".sh",
  ".svg",
  ".toml",
  ".ts",
  ".tsx",
  ".yaml",
  ".yml",
  ".zsh",
]);
const executableSourceNames = new Set(["Dockerfile", "Makefile"]);
const forbiddenTelemetryMarkers = [
  "api.axiom.co",
  "otel.alchemy.run",
  "telemetry.astro.build",
  "sendBeacon",
  "alchemy/Axiom",
  "TelemetryLive",
  "google.com/s2/favicons",
  "BIBCODE_RELAY_CLIENT_OTLP",
  "BIBCODE_MOBILE_OTLP",
  "VITE_RELAY_OTLP",
  "OTEL_EXPORTER_OTLP",
  "/api/observability/v1/traces",
  "otlpTracesUrl",
  "otlpMetricsUrl",
  "@bibcode/shared/relayTracing",
];

function isSourceFile(name: string): boolean {
  return (
    name.startsWith(".env") ||
    executableSourceNames.has(name) ||
    sourceExtensions.has(NodePath.extname(name))
  );
}

function sourceFiles(path: string): string[] {
  return NodeFS.readdirSync(path, { withFileTypes: true }).flatMap((entry) => {
    const child = NodePath.join(path, entry.name);
    if (entry.isDirectory()) {
      return ignoredDirectoryNames.has(entry.name) ? [] : sourceFiles(child);
    }
    if (entry.name.includes(".test.") || entry.name.includes(".spec.")) return [];
    return isSourceFile(entry.name) ? [child] : [];
  });
}

function privacySourceFiles(repoRoot: string): Array<string> {
  const trees = [
    ".devcontainer",
    ".github",
    ".vscode",
    "apps",
    "infra",
    "oxlint-plugin-bibcode",
    "packages",
    "scripts",
    "tools",
  ];
  const rootFiles = NodeFS.readdirSync(repoRoot, { withFileTypes: true }).flatMap((entry) =>
    entry.isFile() && isSourceFile(entry.name) && !entry.name.endsWith(".lock")
      ? [NodePath.join(repoRoot, entry.name)]
      : [],
  );
  return [
    ...trees.flatMap((tree) => {
      const path = NodePath.join(repoRoot, tree);
      return NodeFS.existsSync(path) ? sourceFiles(path) : [];
    }),
    ...rootFiles,
  ];
}

function read(path: string): string {
  return NodeFS.readFileSync(NodePath.join(root, path), "utf8");
}

function cargoDependencyNames(source: string): Array<string> {
  const dependencies = source.match(/^\[dependencies\]\r?\n(?<body>[\s\S]*?)(?=^\[)/m)?.groups
    ?.body;
  if (dependencies === undefined) return [];

  return dependencies
    .split(/\r?\n/)
    .flatMap((line) => {
      const match = /^([A-Za-z0-9_-]+)(?:\.[A-Za-z0-9_-]+)?\s*=/.exec(line);
      return match?.[1] === undefined ? [] : [match[1]];
    })
    .sort();
}

function telemetryViolations(
  files: ReadonlyArray<string>,
  relativeTo: string = root,
  forbiddenMarkers: ReadonlyArray<string> = forbiddenTelemetryMarkers,
  transformSource: (source: string) => string = (source) => source,
): Array<string> {
  return files.flatMap((file) => {
    const source = transformSource(NodeFS.readFileSync(file, "utf8"));
    return forbiddenMarkers
      .filter((marker) => source.includes(marker))
      .map((marker) => `${NodePath.relative(relativeTo, file)}: ${marker}`);
  });
}

function workflowJob(path: string, name: string): string {
  const source = read(path).replaceAll("\r\n", "\n");
  const header = `  ${name}:\n`;
  const start = source.indexOf(header);
  if (start < 0) return "";
  const bodyStart = start + header.length;
  const nextJob = /^  [A-Za-z0-9_-]+:$/m.exec(source.slice(bodyStart));
  return source.slice(start, nextJob ? bodyStart + nextJob.index : undefined);
}

describe("zero-telemetry privacy contract", () => {
  it("adds no dependency for the Git Manager", () => {
    const webPackage = JSON.parse(read("apps/web/package.json")) as {
      dependencies: Record<string, string>;
      devDependencies: Record<string, string>;
    };
    const webDependencyNames = [
      ...Object.keys(webPackage.dependencies),
      ...Object.keys(webPackage.devDependencies),
    ].sort();
    const serverDependencyNames = cargoDependencyNames(read("apps/server/Cargo.toml"));

    expect(webDependencyNames).toEqual([
      "@base-ui/react",
      "@bibcode/client-runtime",
      "@bibcode/contracts",
      "@bibcode/shared",
      "@clerk/react",
      "@dnd-kit/core",
      "@dnd-kit/modifiers",
      "@dnd-kit/sortable",
      "@dnd-kit/utilities",
      "@effect/atom-react",
      "@effect/platform-node",
      "@effect/vitest",
      "@fontsource-variable/dm-sans",
      "@fontsource/jetbrains-mono",
      "@formkit/auto-animate",
      "@legendapp/list",
      "@lexical/react",
      "@pierre/diffs",
      "@pierre/trees",
      "@rolldown/plugin-babel",
      "@tailwindcss/vite",
      "@tanstack/react-pacer",
      "@tanstack/react-router",
      "@tanstack/router-plugin",
      "@tauri-apps/api",
      "@types/babel__core",
      "@types/react",
      "@types/react-dom",
      "@vercel/config",
      "@vitejs/plugin-react",
      "@wdio/tauri-plugin",
      "@xterm/addon-fit",
      "@xterm/addon-webgl",
      "@xterm/xterm",
      "babel-plugin-react-compiler",
      "class-variance-authority",
      "effect",
      "fake-indexeddb",
      "fontkitten",
      "happy-dom",
      "jose",
      "lexical",
      "lucide-react",
      "msw",
      "react",
      "react-dom",
      "react-markdown",
      "rehype-raw",
      "rehype-sanitize",
      "remark-breaks",
      "remark-gfm",
      "tailwind-merge",
      "tailwindcss",
      "vite",
      "vite-plus",
      "zustand",
    ]);
    expect(serverDependencyNames).toEqual([
      "axum",
      "backtrace",
      "base64",
      "breakpad-symbols",
      "clap",
      "crash-handler",
      "dirs",
      "ed25519-dalek",
      "futures-util",
      "getrandom",
      "hmac",
      "http-body-util",
      "httpdate",
      "hyper",
      "hyper-util",
      "mime_guess",
      "minidump",
      "minidump-processor",
      "minidumper",
      "notify",
      "object",
      "open",
      "p256",
      "percent-encoding",
      "portable-pty",
      "process-wrap",
      "reqwest",
      "rusqlite",
      "semver",
      "serde",
      "serde_json",
      "sha2",
      "snow",
      "subtle",
      "sysinfo",
      "thiserror",
      "time",
      "tokio",
      "tokio-tungstenite",
      "tokio-util",
      "tower-http",
      "tracing",
      "tracing-subscriber",
      "url",
      "uuid",
      "zip",
    ]);
  });

  it("contacts no third-party host from Git Manager code", () => {
    const gitManagerFiles = [
      ...sourceFiles(NodePath.join(root, "apps/web/src/components/gitManager")),
      NodePath.join(root, "apps/web/src/gitManagerStore.ts"),
      NodePath.join(root, "packages/client-runtime/src/state/gitManager.ts"),
      ...sourceFiles(NodePath.join(root, "apps/server/src/git/manager")),
      NodePath.join(root, "apps/server/src/source_control/checks.rs"),
    ];
    const productionSource = (source: string): string =>
      source.split("\n#[cfg(test)]\nmod tests {")[0] ?? source;
    const violations = telemetryViolations(
      gitManagerFiles,
      root,
      [
        "http://",
        "https://",
        "sendBeacon",
        "navigator.connection",
        "gravatar",
        "avatars.githubusercontent.com",
      ],
      productionSource,
    );
    const networkClientImport =
      /(?:\bfrom\s*|\bimport\s*(?:\(\s*)?|\brequire\s*\()\s*["'](?:axios|cross-fetch|got|ky|node-fetch|ofetch|superagent|undici|wretch)(?:[/"'])|\b(?:extern\s+crate\s+|use\s+)reqwest(?:::|;|\b)/u;
    const networkClientImportViolations = gitManagerFiles.flatMap((file) => {
      const source = productionSource(NodeFS.readFileSync(file, "utf8"));
      return networkClientImport.test(source)
        ? [`${NodePath.relative(root, file)}: network client import`]
        : [];
    });

    expect([...violations, ...networkClientImportViolations]).toEqual([]);
  });

  it("scans executable web, manifest, shell, and environment files", () => {
    const directory = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "bibcode-privacy-"));
    const fixtures = [
      "page.astro",
      "index.html",
      "package.json",
      "deploy.sh",
      "deploy.ps1",
      "deploy.cmd",
      ".env.production",
    ];
    try {
      for (const fixture of fixtures) {
        NodeFS.writeFileSync(NodePath.join(directory, fixture), "navigator.sendBeacon('/stats')");
      }

      expect(telemetryViolations(sourceFiles(directory), directory).sort()).toEqual(
        fixtures.map((fixture) => `${fixture}: sendBeacon`).sort(),
      );
    } finally {
      NodeFS.rmSync(directory, { recursive: true, force: true });
    }
  });

  it("contains no first-party remote telemetry path", () => {
    const violations = telemetryViolations(privacySourceFiles(root));

    expect(violations).toEqual([]);
  });

  it("removes dedicated telemetry modules", () => {
    for (const path of [
      "apps/server/src/telemetry/mod.rs",
      "apps/web/src/observability/clientTracing.ts",
      "infra/relay/src/observability.ts",
      "packages/shared/src/relayTracing.ts",
    ]) {
      expect(NodeFS.existsSync(NodePath.join(root, path)), path).toBe(false);
    }
  });

  it("forces third-party telemetry off at repository entry points", () => {
    const marketing = JSON.parse(read("apps/marketing/package.json")) as {
      scripts: Record<string, string>;
    };
    const relay = JSON.parse(read("infra/relay/package.json")) as {
      scripts: Record<string, string>;
    };
    const preload = "node --require ../../scripts/disable-telemetry.cjs";

    expect(Object.values(marketing.scripts).every((script) => script.startsWith(preload))).toBe(
      true,
    );
    expect(relay.scripts.deploy?.startsWith(preload)).toBe(true);
    expect(relay.scripts.destroy?.startsWith(preload)).toBe(true);

    const child = NodeChildProcess.spawnSync(
      process.execPath,
      [
        "--require",
        NodePath.join(root, "scripts/disable-telemetry.cjs"),
        "-e",
        `
        const attempt = (change) => { try { change(); } catch {} };
        attempt(() => { process.env.ASTRO_TELEMETRY_DISABLED = "0"; });
        attempt(() => { delete process.env.ALCHEMY_TELEMETRY_DISABLED; });
        attempt(() => Object.defineProperty(process.env, "DO_NOT_TRACK", { value: "0" }));
        attempt(() => { process.env.astro_telemetry_disabled = "0"; });
        attempt(() => { delete process.env.Alchemy_Telemetry_Disabled; });
        attempt(() => Object.defineProperty(process.env, "do_not_track", { value: "0" }));
        attempt(() => { process.env = {}; });
        process.stdout.write(JSON.stringify({
          astro: process.env.ASTRO_TELEMETRY_DISABLED,
          alchemy: process.env.ALCHEMY_TELEMETRY_DISABLED,
          doNotTrack: process.env.DO_NOT_TRACK,
        }));
      `,
      ],
      {
        encoding: "utf8",
        env: {
          ...process.env,
          ASTRO_TELEMETRY_DISABLED: "0",
          ALCHEMY_TELEMETRY_DISABLED: "0",
          DO_NOT_TRACK: "0",
        },
      },
    );
    expect(child.status, child.stderr).toBe(0);
    expect(JSON.parse(child.stdout)).toEqual({
      astro: "1",
      alchemy: "1",
      doNotTrack: "1",
    });

    const deployRelayJob = workflowJob(".github/workflows/deploy-relay.yml", "deploy_relay");
    expect(deployRelayJob).toContain("vp run --filter bibcode-relay deploy");
    expect(deployRelayJob).toContain(
      '      ALCHEMY_TELEMETRY_DISABLED: "1"\n      DO_NOT_TRACK: "1"',
    );

    const releaseRelayJob = workflowJob(".github/workflows/release.yml", "relay_public_config");
    expect(releaseRelayJob).toContain("vp run --filter bibcode-relay deploy");
    expect(releaseRelayJob).toContain(
      '      ALCHEMY_TELEMETRY_DISABLED: "1"\n      DO_NOT_TRACK: "1"',
    );
  });

  it("preserves local diagnostics and stable automatic updates", () => {
    expect(NodeFS.existsSync(NodePath.join(root, "apps/server/src/diagnostics/trace.rs"))).toBe(
      true,
    );
    const updates = read("apps/desktop/src-tauri/src/updates.rs");
    const desktop = read("apps/desktop/src-tauri/src/lib.rs");
    expect(updates).toContain(
      "BACKGROUND_UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60)",
    );
    expect(desktop).toContain("run_background_update_checks");
    expect(read("apps/desktop/src-tauri/tauri.release.conf.json")).toContain(
      "releases/latest/download/latest.json",
    );
  });
});
