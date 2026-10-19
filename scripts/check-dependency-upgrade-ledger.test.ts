// @effect-diagnostics nodeBuiltinImport:off - Repository ledger tests create isolated manifest fixtures directly.
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { describe, expect, it } from "vite-plus/test";
import { parse as parseYaml } from "yaml";

import {
  DEPENDENCY_AUDIT_IGNORED_DIRECTORIES,
  discoverDependencyInventory,
  type DependencyInventory,
  type DependencyLedger,
  validateDependencyLedger,
} from "./check-dependency-upgrade-ledger.ts";

function writeFixture(root: string, relativePath: string, contents: string): void {
  const filePath = NodePath.join(root, relativePath);
  NodeFS.mkdirSync(NodePath.dirname(filePath), { recursive: true });
  NodeFS.writeFileSync(filePath, contents);
}

function withRepositoryFixture<T>(run: (root: string) => T, temporaryBase = NodeOS.tmpdir()): T {
  const root = NodeFS.mkdtempSync(NodePath.join(temporaryBase, "bibcode-dependency-ledger-"));
  try {
    writeFixture(
      root,
      "package.json",
      JSON.stringify({
        name: "fixture",
        private: true,
        engines: { node: "24.13.1" },
        packageManager: "pnpm@10.24.0",
        devDependencies: {
          effect: "catalog:",
          "vite-plus": "catalog:",
        },
      }),
    );
    writeFixture(
      root,
      "pnpm-workspace.yaml",
      [
        "packages:",
        "  - apps/*",
        "catalog:",
        '  effect: "4.0.0-beta.78"',
        '  vite: "npm:@voidzero-dev/vite-plus-core@0.2.1"',
        '  vite-plus: "0.2.1"',
        "",
      ].join("\n"),
    );
    writeFixture(
      root,
      "apps/web/package.json",
      JSON.stringify({
        name: "@bibcode/web",
        dependencies: {
          "@base-ui/react": "^1.4.1",
          "@bibcode/contracts": "workspace:*",
          effect: "catalog:",
        },
      }),
    );
    writeFixture(
      root,
      "apps/desktop/package.json",
      JSON.stringify({
        name: "@bibcode/desktop",
        scripts: {
          build: "pnpm dlx @tauri-apps/cli@2.11.4 build",
        },
      }),
    );
    writeFixture(
      root,
      "Cargo.toml",
      [
        "[workspace]",
        'members = ["apps/server"]',
        "",
        "[workspace.package]",
        'rust-version = "1.88"',
        "",
        "[workspace.dependencies]",
        'serde = { version = "1", features = ["derive"] }',
        'libc = "0.2"',
        'portable-pty = "0.9.0"',
        'windows = "0.62.2"',
        'windows-sys = "0.61.2"',
        'bibcode-server = { path = "apps/server" }',
        "",
        "[patch.crates-io]",
        'portable-pty = { path = "third_party/portable-pty" }',
        'tao = { git = "https://github.com/tauri-apps/tao", ' +
          'rev = "c704261c519c58cfdd0bc2d58ba24e06a0b71c92" }',
        "",
      ].join("\n"),
    );
    writeFixture(
      root,
      "apps/server/Cargo.toml",
      [
        "[package]",
        'name = "bibcode-server"',
        'version = "0.0.0"',
        "",
        "[dependencies]",
        "serde.workspace = true",
        "",
        "[target.'cfg(windows)'.dependencies]",
        "windows-sys.workspace = true",
        "",
      ].join("\n"),
    );
    writeFixture(
      root,
      "apps/server/tests/fixtures/demo/Cargo.toml",
      [
        "[package]",
        'name = "fixture-crate"',
        'version = "0.0.0"',
        "",
        "[workspace]",
        "",
        "[dependencies]",
        'serde = "1"',
        'xpty = "0.3.6"',
        "",
      ].join("\n"),
    );
    writeFixture(
      root,
      "apps/server/tests/fixtures/task8-harness/Cargo.toml",
      [
        "[package]",
        'name = "task8-harness"',
        'version = "0.0.0"',
        "",
        "[workspace]",
        "",
        "[dependencies]",
        'serde = "1"',
        'libc = "0.2"',
        'portable-pty = { path = "../../../../../third_party/portable-pty" }',
        "",
        "[target.'cfg(windows)'.dependencies]",
        'windows = "0.62.2"',
        'windows-sys = "0.61.2"',
        "",
      ].join("\n"),
    );
    writeFixture(
      root,
      "apps/server/tests/fixtures/task9-harness/Cargo.toml",
      [
        "[package]",
        'name = "task9-harness"',
        'version = "0.0.0"',
        "",
        "[workspace]",
        "",
        "[dependencies]",
        'serde = "1.0"',
        'libc = "0.2"',
        "",
      ].join("\n"),
    );
    writeFixture(
      root,
      "third_party/portable-pty/Cargo.toml",
      [
        "[package]",
        'name = "portable-pty"',
        'version = "0.9.0"',
        "",
        "[dependencies]",
        'serde = "1.0"',
        'libc = "0.2"',
        "",
        '[target."cfg(windows)".dependencies]',
        'bitflags = "1.3"',
        "",
      ].join("\n"),
    );
    writeFixture(
      root,
      ".github/workflows/ci.yml",
      [
        "name: CI",
        "jobs:",
        "  test:",
        "    steps:",
        "      - uses: actions/checkout@v6",
        "      - uses: ./.github/actions/setup",
        "",
      ].join("\n"),
    );
    writeFixture(
      root,
      ".devcontainer/devcontainer.json",
      JSON.stringify({
        features: {
          "ghcr.io/devcontainers-extra/features/bun:1": { version: "1.3.11" },
          "ghcr.io/devcontainers/features/node:1": { version: "24.13.1" },
        },
      }),
    );
    return run(root);
  } finally {
    NodeFS.rmSync(root, { recursive: true, force: true });
  }
}

function completeLedger(inventory: DependencyInventory): DependencyLedger {
  return {
    schemaVersion: 1,
    auditDate: "2026-07-17",
    inventorySummary: { ...inventory.summary },
    baseline: {
      originMainSha: "a".repeat(40),
      implementationHead: "b".repeat(40),
      tools: {
        node: "v24.13.1",
        pnpm: "10.24.0",
        rust: "rustc 1.88.0",
        vitePlus: "0.2.1",
      },
      commands: [
        { command: "vp check", durationSeconds: 1, result: "passed" },
        { command: "vp run typecheck", durationSeconds: 1, result: "passed" },
        { command: "vp test", durationSeconds: 1, result: "passed" },
        { command: "vp run test", durationSeconds: 1, result: "passed" },
        {
          command: "cargo test --workspace --all-targets -j 2",
          durationSeconds: 1,
          result: "passed",
        },
      ],
      warnings: [],
    },
    dependencies: inventory.entries.map((entry) => ({
      key: entry.key,
      name: entry.name,
      current: entry.current,
      target: entry.current,
      channel: "stable",
      source: "https://example.com/dependency",
      cohort: "fixture",
      platforms: entry.platforms ?? ["linux", "macos", "windows"],
      status: "current",
    })),
  };
}

function findUnexpectedSourceReferences(root: string, dependency: string): Array<string> {
  const ignoredFiles = new Set([
    "docs/dependency-upgrades/2026-07-17-ledger.json",
    "docs/superpowers/plans/2026-07-17-direct-dependency-modernization.md",
    "package.json",
    "pnpm-lock.yaml",
    "scripts/check-dependency-upgrade-ledger.test.ts",
  ]);
  const sourceExtensions = new Set([
    ".cjs",
    ".js",
    ".jsx",
    ".json",
    ".mjs",
    ".toml",
    ".ts",
    ".tsx",
    ".yaml",
    ".yml",
  ]);
  const references: Array<string> = [];
  const visit = (directory: string): void => {
    for (const entry of NodeFS.readdirSync(directory, { withFileTypes: true })) {
      if (entry.isDirectory() && DEPENDENCY_AUDIT_IGNORED_DIRECTORIES.has(entry.name)) {
        continue;
      }
      const absolutePath = NodePath.join(directory, entry.name);
      if (entry.isDirectory()) {
        visit(absolutePath);
        continue;
      }
      if (!entry.isFile()) continue;
      const relativePath = NodePath.relative(root, absolutePath).split(NodePath.sep).join("/");
      if (ignoredFiles.has(relativePath)) continue;
      if (!sourceExtensions.has(NodePath.extname(entry.name))) continue;
      if (NodeFS.readFileSync(absolutePath, "utf8").includes(dependency)) {
        references.push(relativePath);
      }
    }
  };
  visit(root);
  return references.toSorted();
}

describe("dependency upgrade ledger discovery", () => {
  it("cleans a repository fixture when its callback fails", () => {
    const temporaryBase = NodeFS.mkdtempSync(
      NodePath.join(NodeOS.tmpdir(), "bibcode-dependency-ledger-owner-"),
    );
    try {
      expect(() =>
        withRepositoryFixture(() => {
          throw new Error("forced fixture failure");
        }, temporaryBase),
      ).toThrow("forced fixture failure");
      expect(NodeFS.readdirSync(temporaryBase)).toEqual([]);
    } finally {
      NodeFS.rmSync(temporaryBase, { recursive: true, force: true });
    }
  });

  it("keeps the direct Babel JSX transform plugin unused outside dependency metadata", () => {
    const repositoryRoot = NodePath.resolve(import.meta.dirname, "..");

    expect(
      findUnexpectedSourceReferences(repositoryRoot, "@babel/plugin-transform-react-jsx"),
    ).toEqual([]);
  });

  it("discovers external JavaScript dependencies and excludes workspace links", () => {
    const inventory = withRepositoryFixture(discoverDependencyInventory);

    expect(inventory.entries.map((entry) => entry.key)).toContain("js:apps/web:@base-ui/react");
    expect(inventory.entries.map((entry) => entry.key)).toContain("js:catalog:effect");
    expect(inventory.entries.some((entry) => entry.name === "@bibcode/contracts")).toBe(false);
    expect(inventory.entries.find((entry) => entry.key === "js:catalog:effect")?.locations).toEqual(
      ["apps/web/package.json", "package.json", "pnpm-workspace.yaml"],
    );
  });

  it("ignores local Superpowers snapshot dependencies", () => {
    const inventory = withRepositoryFixture((root) => {
      const snapshotDirectory = NodePath.join(root, ".superpowers/sdd/snapshots/example");
      NodeFS.mkdirSync(snapshotDirectory, { recursive: true });
      NodeFS.writeFileSync(
        NodePath.join(snapshotDirectory, "package.json"),
        JSON.stringify({
          dependencies: {
            "snapshot-only-dependency": "1.0.0",
          },
        }),
      );
      return discoverDependencyInventory(root);
    });

    expect(inventory.entries.some((entry) => entry.name === "snapshot-only-dependency")).toBe(
      false,
    );
  });

  it("ignores dependencies from local Git worktrees", () => {
    const inventory = withRepositoryFixture((root) => {
      const worktreeDirectory = NodePath.join(root, ".worktrees/example");
      NodeFS.mkdirSync(worktreeDirectory, { recursive: true });
      NodeFS.writeFileSync(
        NodePath.join(worktreeDirectory, "package.json"),
        JSON.stringify({ dependencies: { "worktree-only-dependency": "1.0.0" } }),
      );
      return discoverDependencyInventory(root);
    });

    expect(inventory.entries.some((entry) => entry.name === "worktree-only-dependency")).toBe(
      false,
    );
  });

  it("keeps explicit standalone declarations separate from workspace inheritance", () => {
    const inventory = withRepositoryFixture(discoverDependencyInventory);
    const keys = inventory.entries.map((entry) => entry.key);

    expect(keys).toContain("rust:workspace:serde");
    expect(keys).toContain("rust:workspace:bibcode-server");
    expect(keys).toContain("rust:apps/server/tests/fixtures/demo:xpty");
    expect(keys).toEqual(
      expect.arrayContaining([
        "rust:apps/server/tests/fixtures/demo:serde",
        "rust:apps/server/tests/fixtures/task8-harness:serde",
        "rust:apps/server/tests/fixtures/task8-harness:libc",
        "rust:apps/server/tests/fixtures/task8-harness:portable-pty",
        "rust:apps/server/tests/fixtures/task9-harness:serde",
        "rust:apps/server/tests/fixtures/task9-harness:libc",
        "rust:third_party/portable-pty:serde",
        "rust:third_party/portable-pty:libc",
      ]),
    );
    expect(keys).not.toContain("rust:apps/server:serde");
    expect(inventory.summary.rustRegistry).toBe(16);
    expect(inventory.summary.rustPath).toBe(3);
    expect(
      inventory.entries.find(
        (entry) => entry.key === "rust:apps/server/tests/fixtures/task8-harness:serde",
      )?.platforms,
    ).toEqual(["linux", "macos", "windows"]);
    expect(
      inventory.entries.find(
        (entry) => entry.key === "rust:apps/server/tests/fixtures/task8-harness:windows-sys",
      )?.platforms,
    ).toEqual(["windows"]);
    expect(
      inventory.entries.find((entry) => entry.key === "rust:third_party/portable-pty:bitflags")
        ?.platforms,
    ).toEqual(["windows"]);
  });

  it("classifies crates.io Git and path patches as first-class inventory entries", () => {
    const inventory = withRepositoryFixture(discoverDependencyInventory);

    expect(inventory.entries).toEqual(
      expect.arrayContaining([
        {
          key: "rust:patch:portable-pty",
          category: "rust",
          name: "portable-pty",
          current: "path:third_party/portable-pty",
          locations: ["Cargo.toml"],
          dependencyKind: "path",
          platforms: ["linux", "macos", "windows"],
        },
        {
          key: "rust:patch:tao",
          category: "rust",
          name: "tao",
          current: "git:https://github.com/tauri-apps/tao#c704261c519c58cfdd0bc2d58ba24e06a0b71c92",
          locations: ["Cargo.toml"],
          dependencyKind: "git",
          platforms: ["linux", "macos", "windows"],
        },
      ]),
    );
    expect(inventory.summary.rustGit).toBe(1);
    expect(inventory.summary.rustPath).toBe(3);
  });

  it("discovers external workflow actions but ignores local actions", () => {
    const inventory = withRepositoryFixture(discoverDependencyInventory);

    expect(inventory.entries.map((entry) => entry.key)).toContain("action:actions/checkout");
    expect(inventory.entries.some((entry) => entry.name === "./.github/actions/setup")).toBe(false);
  });

  it("rejects reserved-scope inventory key collisions instead of overwriting them", () => {
    expect(() =>
      withRepositoryFixture((root) => {
        writeFixture(
          root,
          "workspace/Cargo.toml",
          [
            "[package]",
            'name = "reserved-workspace-scope"',
            'version = "0.0.0"',
            "",
            "[dependencies]",
            'serde = "1"',
            "",
          ].join("\n"),
        );
        return discoverDependencyInventory(root);
      }),
    ).toThrow("duplicate dependency inventory key rust:workspace:serde");
  });

  it("discovers Node, pnpm, Rust, Vite+, Tauri CLI, and devcontainer pins", () => {
    const inventory = withRepositoryFixture(discoverDependencyInventory);
    const keys = inventory.entries.map((entry) => entry.key);

    expect(keys).toEqual(
      expect.arrayContaining([
        "toolchain:node",
        "toolchain:pnpm",
        "toolchain:rust",
        "toolchain:vite-core",
        "toolchain:vite-plus",
        "toolchain:tauri-cli",
        "toolchain:devcontainer:bun",
        "toolchain:devcontainer:node",
      ]),
    );
  });
});

describe("dependency upgrade ledger validation", () => {
  it("records the approved convergence targets and patch set", () => {
    const repositoryRoot = NodePath.resolve(import.meta.dirname, "..");
    const ledger = JSON.parse(
      NodeFS.readFileSync(
        NodePath.join(repositoryRoot, "docs/dependency-upgrades/2026-07-17-ledger.json"),
        "utf8",
      ),
    ) as DependencyLedger;
    const entries = new Map(ledger.dependencies.map((dependency) => [dependency.key, dependency]));
    const entry = (key: string) => {
      const dependency = entries.get(key);
      expect(dependency, key).toBeDefined();
      return dependency!;
    };
    const workspace = parseYaml(
      NodeFS.readFileSync(NodePath.join(repositoryRoot, "pnpm-workspace.yaml"), "utf8"),
    ) as { patchedDependencies: Record<string, string> };

    expect(entry("toolchain:node").target).toBe("26.8.1");
    expect(entry("toolchain:pnpm").target).toBe("11.25.0");
    expect(entry("toolchain:rust").target).toBe("1.98.0");
    expect(entry("toolchain:vite-core").target).toBe("npm:@voidzero-dev/vite-plus-core@0.3.0");
    expect(entry("toolchain:vite-plus").target).toBe("0.3.0");
    expect(entry("js:catalog:effect").target).toBe("4.0.0-beta.107");
    expect(entry("js:catalog:@effect/platform-node-shared").target).toBe("4.0.0-beta.107");
    expect(entry("js:catalog:@effect/sql-d1").target).toBe("4.0.0-beta.107");
    expect(entry("js:catalog:@effect/sql-sqlite-do").target).toBe("4.0.0-beta.107");
    expect(entry("js:infra/relay:alchemy").target).toBe("2.0.0-beta.72");
    expect(entry("js:infra/relay:drizzle-kit").target).toBe("1.0.0-rc.5-ab785fc");
    expect(entry("js:infra/relay:drizzle-orm").target).toBe("1.0.0-rc.5-ab785fc");
    expect(entry("js:apps/web:react").target).toBe("19.2.8");
    expect(entry("rust:workspace:process-wrap").target).toBe("9.1.0");
    expect(entry("rust:workspace:base64").target).toBe("0.22.1");

    const exactMigratedTargets = [
      ["js:apps/web:@fontsource-variable/dm-sans", "^5.3.0", "5.3.0", "react-ui"],
      ["js:apps/web:@fontsource/jetbrains-mono", "^5.3.0", "5.3.0", "react-ui"],
      ["js:apps/web:@types/react-dom", "~19.2.6", "19.2.6", "react-ui"],
      ["js:apps/web:happy-dom", "^20.13.2", "20.13.2", "stable-javascript-tooling"],
      ["js:apps/web:zustand", "^5.0.15", "5.0.15", "react-ui"],
    ] as const;
    for (const [key, current, target, cohort] of exactMigratedTargets) {
      const dependency = entry(key);
      expect(dependency.current, key).toBe(current);
      expect(dependency.target, key).toBe(target);
      expect(dependency.cohort, key).toBe(cohort);
      expect(dependency.status, key).toBe("green");
    }

    const blockedWdioEntries = [
      ["js:apps/desktop:@wdio/cli", "9.29.1", "9.31.5"],
      ["js:apps/desktop:@wdio/globals", "9.29.1", "9.31.3"],
      ["js:apps/desktop:@wdio/local-runner", "9.29.1", "9.31.5"],
      ["js:apps/desktop:@wdio/mocha-framework", "9.29.1", "9.31.5"],
      ["js:apps/desktop:@wdio/native-utils", "2.5.0", "2.6.0"],
      ["js:apps/desktop:@wdio/spec-reporter", "9.29.1", "9.31.2"],
      ["js:apps/desktop:@wdio/tauri-service", "1.2.0", "1.3.0"],
      ["js:apps/desktop:webdriverio", "9.29.1", "9.31.5"],
      ["js:apps/web:@wdio/tauri-plugin", "1.2.0", "1.3.0"],
      ["rust:workspace:tauri-plugin-wdio", "=1.2.0", "1.3.0"],
      ["rust:workspace:tauri-plugin-wdio-webdriver", "=1.2.0", "1.3.0"],
    ] as const;
    for (const [key, current, target] of blockedWdioEntries) {
      const dependency = entry(key);
      expect(dependency.current, key).toBe(current);
      expect(dependency.target, key).toBe(target);
      expect(dependency.status, key).toBe("blocked");
      expect(dependency.notes, key).toContain("afterSession teardown ordering");
      expect(dependency.notes, key).toContain("aligns @wdio/globals/expect-webdriverio");
    }

    const retainedEntries = [
      {
        key: "js:catalog:typescript",
        target: "7.0.2",
        releaseCondition: "official compatible checker exists",
      },
      {
        key: "rust:workspace:process-wrap",
        target: "9.1.0",
        releaseCondition: "dedicated process-supervision migration",
      },
      {
        key: "rust:workspace:base64",
        target: "0.22.1",
        releaseCondition: "protocol/serialization review",
      },
      {
        key: "rust:workspace:cairo-rs",
        target: "0.18",
        releaseCondition: "WebKit and Linux system-library support as one platform project",
      },
      {
        key: "rust:workspace:gtk",
        target: "0.18",
        releaseCondition: "WebKit and Linux system-library support as one platform project",
      },
      {
        key: "rust:workspace:webkit2gtk",
        target: "2.0",
        releaseCondition: "WebKit and Linux system-library support as one platform project",
      },
      {
        key: "rust:workspace:webview2-com",
        target: "0.38",
        releaseCondition: "pre-1.0 Windows FFI migration",
      },
      {
        key: "rust:workspace:windows",
        target: "0.61",
        releaseCondition: "Tauri/WebView compatibility and native Windows validation",
      },
      {
        key: "rust:workspace:windows-sys",
        target: "0.61.2",
        releaseCondition: "Tauri/WebView compatibility and native Windows validation",
      },
      {
        key: "rust:workspace:portable-pty",
        target: "0.9.0",
        releaseCondition:
          "at-creation Job Object, termination-result, and Unix spawn-handshake fixes",
      },
      {
        key: "rust:workspace:minisign-verify",
        target: "0.2.5",
        releaseCondition: "trust boundary is already current and exact",
      },
      {
        key: "rust:patch:tao",
        target: "c704261c519c58cfdd0bc2d58ba24e06a0b71c92",
        releaseCondition:
          "Tauri consumes an upstream equivalent to the Windows reentrant keyboard/IME fix",
      },
    ];
    for (const retained of retainedEntries) {
      const dependency = entry(retained.key);
      expect(dependency.target, retained.key).toBe(retained.target);
      expect(dependency.status, retained.key).toBe("blocked");
      expect(dependency.notes, retained.key).toContain(retained.releaseCondition);
    }

    expect(Object.keys(workspace.patchedDependencies).sort()).toEqual([
      "@effect/vitest@4.0.0-beta.107",
      "@pierre/trees@1.0.0-beta.6",
      "@wdio/tauri-plugin@1.2.0",
    ]);
  });

  it("rejects every stale authoritative inventory summary field", () => {
    const inventory = withRepositoryFixture(discoverDependencyInventory);
    const summaryFields = Object.keys(inventory.summary) as Array<keyof typeof inventory.summary>;

    for (const field of summaryFields) {
      const ledger = Object.assign(completeLedger(inventory), {
        inventorySummary: {
          ...inventory.summary,
          [field]: inventory.summary[field] + 1,
        },
      });

      expect(validateDependencyLedger(inventory, ledger)).toContain(
        `inventory summary ${field} does not match discovered count: ${inventory.summary[field] + 1} != ${inventory.summary[field]}`,
      );
    }
  });

  it("rejects duplicate keys and missing required fields", () => {
    const inventory = withRepositoryFixture(discoverDependencyInventory);
    const ledger = completeLedger(inventory);
    const firstDependency = ledger.dependencies[0];
    if (firstDependency === undefined) throw new Error("fixture inventory must not be empty");
    const duplicate: Partial<typeof firstDependency> = structuredClone(firstDependency);
    delete duplicate.target;
    ledger.dependencies.push(duplicate as typeof firstDependency);

    const errors = validateDependencyLedger(inventory, ledger);

    expect(errors.some((error) => error.includes("duplicate ledger key"))).toBe(true);
    expect(errors.some((error) => error.includes("missing target"))).toBe(true);
  });

  it("rejects missing, stale, and invalid-status entries", () => {
    const inventory = withRepositoryFixture(discoverDependencyInventory);
    const ledger = completeLedger(inventory);
    const removed = ledger.dependencies.shift();
    if (removed === undefined) throw new Error("fixture inventory must not be empty");
    const firstDependency = ledger.dependencies[0];
    if (firstDependency === undefined) throw new Error("fixture inventory must contain two rows");
    firstDependency.status = "finished";
    ledger.dependencies.push({
      key: "js:apps/removed:stale",
      name: "stale",
      current: "1.0.0",
      target: "2.0.0",
      channel: "stable",
      source: "https://example.com/stale",
      cohort: "fixture",
      platforms: ["linux"],
      status: "current",
    });

    const errors = validateDependencyLedger(inventory, ledger);

    expect(errors.some((error) => error.includes(`missing ledger entry ${removed.key}`))).toBe(
      true,
    );
    expect(errors.some((error) => error.includes("invalid status"))).toBe(true);
    expect(errors.some((error) => error.includes("no longer declared"))).toBe(true);
  });

  it("rejects a missing duplicate standalone declaration and a misclassified path patch", () => {
    const inventory = withRepositoryFixture(discoverDependencyInventory);
    const ledger = completeLedger(inventory);
    const duplicateIndex = ledger.dependencies.findIndex(
      (entry) => entry.key === "rust:apps/server/tests/fixtures/task9-harness:serde",
    );
    if (duplicateIndex < 0) throw new Error("fixture duplicate declaration must be discovered");
    ledger.dependencies.splice(duplicateIndex, 1);
    const pathPatch = ledger.dependencies.find((entry) => entry.key === "rust:patch:portable-pty");
    if (pathPatch === undefined) throw new Error("fixture path patch must be discovered");
    pathPatch.current = "git:https://example.invalid/portable-pty#" + "a".repeat(40);
    Object.assign(ledger, {
      inventorySummary: {
        ...ledger.inventorySummary,
        rustPath: ledger.inventorySummary.rustPath - 1,
      },
    });

    const errors = validateDependencyLedger(inventory, ledger);

    expect(errors).toContain(
      "missing ledger entry rust:apps/server/tests/fixtures/task9-harness:serde",
    );
    expect(errors).toContain(
      "rust:patch:portable-pty current value does not match declarations: " +
        "git:https://example.invalid/portable-pty#" +
        "a".repeat(40) +
        " != path:third_party/portable-pty",
    );
    expect(errors).toContain("inventory summary rustPath does not match discovered count: 2 != 3");
  });

  it("rejects incorrect authoritative target platforms", () => {
    const inventory = withRepositoryFixture(discoverDependencyInventory);
    const ledger = completeLedger(inventory);
    const windowsOnly = ledger.dependencies.find(
      (entry) => entry.key === "rust:apps/server/tests/fixtures/task8-harness:windows-sys",
    );
    if (windowsOnly === undefined) throw new Error("fixture Windows dependency must be discovered");
    Object.assign(windowsOnly, { platforms: ["linux", "macos", "windows"] });

    expect(validateDependencyLedger(inventory, ledger)).toContain(
      "rust:apps/server/tests/fixtures/task8-harness:windows-sys platforms do not match declarations: linux,macos,windows != windows",
    );
  });

  it("reports malformed platform shapes without throwing during authoritative comparison", () => {
    const inventory = withRepositoryFixture(discoverDependencyInventory);
    const ledger = completeLedger(inventory);
    const windowsOnly = ledger.dependencies.find(
      (entry) => entry.key === "rust:apps/server/tests/fixtures/task8-harness:windows-sys",
    );
    if (windowsOnly === undefined) throw new Error("fixture Windows dependency must be discovered");
    Object.assign(windowsOnly, { platforms: { windows: true } });
    let errors: Array<string> = [];

    expect(() => {
      errors = validateDependencyLedger(inventory, ledger);
    }).not.toThrow();
    expect(errors).toContain(
      "rust:apps/server/tests/fixtures/task8-harness:windows-sys is missing platforms",
    );
  });

  it("detects declaration deletion and path-patch source reclassification", () => {
    withRepositoryFixture((root) => {
      const ledger = completeLedger(discoverDependencyInventory(root));
      const task9Path = NodePath.join(root, "apps/server/tests/fixtures/task9-harness/Cargo.toml");
      NodeFS.writeFileSync(
        task9Path,
        NodeFS.readFileSync(task9Path, "utf8").replace('serde = "1.0"\n', ""),
      );
      const cargoPath = NodePath.join(root, "Cargo.toml");
      NodeFS.writeFileSync(
        cargoPath,
        NodeFS.readFileSync(cargoPath, "utf8").replace(
          'portable-pty = { path = "third_party/portable-pty" }',
          'portable-pty = { git = "https://example.invalid/portable-pty", rev = "' +
            "b".repeat(40) +
            '" }',
        ),
      );

      const errors = validateDependencyLedger(discoverDependencyInventory(root), ledger);

      expect(errors).toEqual(
        expect.arrayContaining([
          "inventory summary rustRegistry does not match discovered count: 16 != 15",
          "inventory summary rustPath does not match discovered count: 3 != 2",
          "inventory summary rustGit does not match discovered count: 1 != 2",
          "rust:patch:portable-pty current value does not match declarations: " +
            "path:third_party/portable-pty != " +
            "git:https://example.invalid/portable-pty#" +
            "b".repeat(40),
          "rust:apps/server/tests/fixtures/task9-harness:serde is no longer declared and must be marked removed",
        ]),
      );
    });
  });

  it("accepts omitted command durations and rejects malformed baseline command metadata", () => {
    const inventory = withRepositoryFixture(discoverDependencyInventory);
    const withoutDurations = completeLedger(inventory);
    Object.assign(withoutDurations.baseline, {
      commands: withoutDurations.baseline.commands.map((command) => ({
        command: command.command,
        result: command.result,
        ...(command.note === undefined ? {} : { note: command.note }),
      })),
    });
    expect(validateDependencyLedger(inventory, withoutDurations)).toEqual([]);

    const malformed = completeLedger(inventory);
    Object.assign(malformed.baseline.commands[0]!, {
      durationSeconds: 86_401,
      note: " ",
    });
    Object.assign(malformed.baseline.commands[1]!, { command: "" });
    Object.assign(malformed.baseline.commands[2]!, { result: "" });
    malformed.baseline.commands.push(null as never);

    expect(validateDependencyLedger(inventory, malformed)).toEqual(
      expect.arrayContaining([
        "baseline command at index 0 has invalid durationSeconds",
        "baseline command at index 0 has invalid note",
        "baseline command at index 1 has invalid command",
        "baseline command at index 2 has invalid result",
        "baseline command at index 5 is invalid",
      ]),
    );
  });

  it("requires a completely green synchronized baseline before progress", () => {
    const inventory = withRepositoryFixture(discoverDependencyInventory);
    const ledger = completeLedger(inventory);
    const firstCommand = ledger.baseline.commands[0];
    const firstDependency = ledger.dependencies[0];
    if (firstCommand === undefined || firstDependency === undefined) {
      throw new Error("fixture ledger must contain baseline evidence and dependencies");
    }
    firstCommand.result = "failed";
    firstDependency.status = "green";

    expect(validateDependencyLedger(inventory, ledger)).toContain(
      "baseline command vp check did not pass before dependency progress",
    );
  });

  it("accounts for every dependency in the synchronized repository", () => {
    const repositoryRoot = NodePath.resolve(import.meta.dirname, "..");
    const inventory = discoverDependencyInventory(repositoryRoot);
    const ledger = JSON.parse(
      NodeFS.readFileSync(
        NodePath.join(repositoryRoot, "docs/dependency-upgrades/2026-07-17-ledger.json"),
        "utf8",
      ),
    ) as DependencyLedger;

    expect(validateDependencyLedger(inventory, ledger)).toEqual([]);
    expect(ledger.inventorySummary).toEqual({
      javascriptDirect: 81,
      javascriptLedger: 83,
      rustRegistry: 138,
      rustPath: 3,
      rustGit: 1,
      actions: 9,
      toolchains: 9,
    });
    expect({
      auditDate: ledger.auditDate,
      pendingKeys: ledger.dependencies
        .filter((dependency) => dependency.status === "pending")
        .map((dependency) => dependency.key),
    }).toEqual({
      auditDate: "2026-09-03",
      pendingKeys: [],
    });
    expect(
      ledger.dependencies.filter((dependency) =>
        dependency.key.startsWith("rust:apps/server/tests/fixtures/task8-harness:"),
      ),
    ).toHaveLength(19);
    expect(
      ledger.dependencies.filter((dependency) =>
        dependency.key.startsWith("rust:apps/server/tests/fixtures/task9-harness:"),
      ),
    ).toHaveLength(10);
    expect(
      ledger.dependencies.filter((dependency) =>
        dependency.key.startsWith("rust:third_party/portable-pty:"),
      ),
    ).toHaveLength(17);
    expect(ledger.baseline.tools).toEqual({
      node: "v26.8.1",
      pnpm: "11.15.0",
      rust: "rustc 1.98.0",
      vitePlus: "0.2.5",
    });
    expect(ledger.baseline.warnings).toEqual([]);
    const validation = (
      ledger as DependencyLedger & {
        readonly validationFoundation?: {
          readonly supportedConvergence?: {
            readonly tools?: Readonly<Record<string, string>>;
            readonly warnings?: ReadonlyArray<string>;
            readonly ledgerClosure?: {
              readonly rows: number;
              readonly statuses: Readonly<Record<string, number>>;
            };
          };
        };
      }
    ).validationFoundation?.supportedConvergence;
    expect(validation?.tools).toEqual({
      node: "v26.8.1",
      pnpm: "11.25.0",
      rust: "rustc 1.98.0",
      vitePlus: "0.3.0",
    });
    expect(validation?.warnings).toEqual(
      expect.arrayContaining([
        expect.stringContaining("xn--a.clerk.accounts.dev"),
        expect.stringContaining("442 non-fatal"),
      ]),
    );
    const derivedStatuses = ledger.dependencies.reduce<Record<string, number>>(
      (counts, dependency) => {
        counts[dependency.status] = (counts[dependency.status] ?? 0) + 1;
        return counts;
      },
      {},
    );
    expect(validation?.ledgerClosure).toEqual({
      rows: ledger.dependencies.length,
      statuses: derivedStatuses,
    });
    const tauriCli = ledger.dependencies.find(
      (dependency) => dependency.key === "toolchain:tauri-cli",
    );
    expect(tauriCli).toMatchObject({
      current: "2.11.4",
      target: "2.11.4",
      status: "current",
    });
    expect(tauriCli?.notes).toContain("locked workspace dependency");
    const representativeStates = [
      {
        key: "rust:apps/server/tests/fixtures/task8-harness:base64",
        status: "current",
        current: "0.22.1",
        target: "0.22.1",
      },
      {
        key: "rust:apps/server/tests/fixtures/task8-harness:serde",
        status: "green",
        current: "1",
        target: "1.0.229",
      },
      {
        key: "rust:apps/server/tests/fixtures/task8-harness:rusqlite",
        status: "current",
        current: "0.40.1",
        target: "0.40.2",
      },
      {
        key: "rust:apps/server/tests/fixtures/task8-harness:portable-pty",
        status: "blocked",
        current: "path:../../../../../third_party/portable-pty",
        target: "path:../../../../../third_party/portable-pty",
      },
    ];
    for (const expected of representativeStates) {
      const dependency = ledger.dependencies.find((entry) => entry.key === expected.key);
      expect(dependency, expected.key).toMatchObject(expected);
      if (expected.status === "blocked") {
        expect(dependency?.notes, expected.key).toContain("UPSTREAM.md");
      }
    }
    expect(
      ledger.dependencies.find(
        (dependency) =>
          dependency.key === "rust:apps/server/tests/fixtures/task8-harness:windows-sys",
      )?.platforms,
    ).toEqual(["windows"]);
    expect(
      ledger.dependencies.find(
        (dependency) => dependency.key === "rust:third_party/portable-pty:bitflags",
      )?.platforms,
    ).toEqual(["windows"]);
    for (const key of [
      "js:catalog:typescript",
      "rust:workspace:process-wrap",
      "rust:workspace:base64",
      "rust:workspace:portable-pty",
      "rust:patch:portable-pty",
      "rust:patch:tao",
    ]) {
      const boundary = ledger.dependencies.find((dependency) => dependency.key === key);
      expect(boundary?.status, key).toBe("blocked");
      expect(boundary?.notes?.trim().length, key).toBeGreaterThan(0);
    }
  });
});
