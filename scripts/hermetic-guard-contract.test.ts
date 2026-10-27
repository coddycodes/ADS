// @effect-diagnostics nodeBuiltinImport:off - Guard contracts inspect checked-in Cargo and workflow policy.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import { parse as parseToml, type TomlTable } from "smol-toml";
import { describe, expect, it } from "vite-plus/test";

const root = NodePath.resolve(import.meta.dirname, "..");
const feature = "hermetic-test-guard";
const manifests = [
  "Cargo.toml",
  "apps/server/Cargo.toml",
  "apps/desktop/src-tauri/Cargo.toml",
  "tools/updater-verifier/Cargo.toml",
];

function table(value: unknown): TomlTable {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as TomlTable)
    : {};
}

function strings(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === "string")
    : [];
}

function guardedDependency(value: unknown): boolean {
  return strings(table(value).features).includes(feature);
}

function runtimeViolations(manifest: TomlTable): string[] {
  const violations: string[] = [];
  const sections = [
    table(manifest.dependencies),
    table(table(manifest.workspace).dependencies),
    ...Object.values(table(manifest.target)).map((target) => table(table(target).dependencies)),
  ];
  for (const section of sections) {
    for (const [name, value] of Object.entries(section)) {
      if (guardedDependency(value)) violations.push(`runtime dependency ${name}`);
    }
  }
  const features = table(manifest.features);
  const visited = new Set<string>();
  function enablesGuard(name: string): boolean {
    if (name === feature || name.endsWith(`/${feature}`)) return true;
    if (visited.has(name)) return false;
    visited.add(name);
    return strings(features[name]).some(enablesGuard);
  }
  if (strings(features.default).some(enablesGuard)) violations.push("default feature");
  return violations;
}

function read(relative: string): TomlTable {
  return parseToml(NodeFS.readFileSync(NodePath.join(root, relative), "utf8"));
}

describe("hermetic guard activation", () => {
  it("keeps default abort and Report diagnosis at the shared runtime boundaries", () => {
    const source = (path: string) => NodeFS.readFileSync(NodePath.join(root, path), "utf8");
    const guard = source("apps/server/src/hermetic_guard.rs");
    expect(guard).toContain('const DEFAULT_MODE: &str = "abort";');
    expect(guard).toContain('Some(value) if value == "report" => false');
    for (const path of [
      "apps/server/src/production/provider_runtime.rs",
      "apps/server/src/provider_terminal/supervisor.rs",
    ])
      expect(source(path), path).toContain("crate::hermetic_guard::resolve_guarded_executable");
    expect(source("apps/server/src/git/process.rs")).toContain(
      "crate::hermetic_guard::checked_launch_executable",
    );
    for (const path of [
      "apps/server/src/production/provider_runtime.rs",
      "apps/server/src/provider_terminal/codex.rs",
      "apps/server/src/provider_terminal/claude.rs",
      "apps/server/src/provider_terminal/opencode.rs",
    ])
      expect(source(path), path).toContain("crate::hermetic_guard::checked_provider_executable");
    for (const path of [
      "apps/server/src/provider_usage/mod.rs",
      "apps/server/src/provider_usage/codex_backend.rs",
    ])
      expect(source(path), path).toContain("crate::hermetic_guard::credential_path_allowed");
    expect(source("apps/server/src/provider_usage/mod.rs")).toContain('"macOS keychain"');
  });

  it("enables the guard for server and desktop dev units while production dependencies stay feature off", () => {
    const server = read("apps/server/Cargo.toml");
    expect(table(server.features)[feature]).toEqual([]);
    for (const path of ["apps/server/Cargo.toml", "apps/desktop/src-tauri/Cargo.toml"]) {
      const dependency = table(table(read(path))["dev-dependencies"])["coddy-studio-server"];
      expect(table(dependency).workspace).toBe(true);
      expect(strings(table(dependency).features)).toContain(feature);
    }
    for (const path of manifests) expect(runtimeViolations(read(path)), path).toEqual([]);
    const library = NodeFS.readFileSync(NodePath.join(root, "apps/server/src/lib.rs"), "utf8");
    expect(library).toContain('#[cfg(feature = "hermetic-test-guard")]\nmod hermetic_guard;');
  });

  it("rejects accidental normal, target-specific and indirect-default activation", () => {
    expect(
      runtimeViolations(
        parseToml('[dependencies]\ncoddy-studio-server = { features = ["hermetic-test-guard"] }'),
      ),
    ).toEqual(["runtime dependency coddy-studio-server"]);
    expect(
      runtimeViolations(
        parseToml(
          '[target."cfg(windows)".dependencies]\ncoddy-studio-server = { features = ["hermetic-test-guard"] }',
        ),
      ),
    ).toEqual(["runtime dependency coddy-studio-server"]);
    expect(
      runtimeViolations(
        parseToml('[features]\ndefault = ["full"]\nfull = ["coddy-studio-server/hermetic-test-guard"]'),
      ),
    ).toEqual(["default feature"]);
  });
});
