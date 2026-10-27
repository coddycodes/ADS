import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";
import { parse as parseToml, type TomlTable } from "smol-toml";

const PackageJson = Schema.fromJsonString(
  Schema.Struct({
    version: Schema.optional(Schema.String),
    scripts: Schema.Record(Schema.String, Schema.String),
  }),
);
const decodePackageJson = Schema.decodeUnknownEffect(PackageJson);

const rustPackages = [
  {
    cargoPackage: "coddy-studio-desktop",
    memberPath: "apps/desktop/src-tauri",
    packageJsonPath: "apps/desktop/package.json",
    cargoSelectedScriptNames: ["dev", "test", "typecheck"],
  },
  {
    cargoPackage: "coddy-studio-server",
    memberPath: "apps/server",
    packageJsonPath: "apps/server/package.json",
    cargoSelectedScriptNames: ["build", "start", "test", "typecheck"],
  },
  {
    cargoPackage: "coddy-studio-updater-verifier",
    memberPath: "tools/updater-verifier",
  },
] as const;

const table = (value: unknown): TomlTable => {
  assert.equal(typeof value, "object");
  assert.notEqual(value, null);
  assert.equal(Array.isArray(value), false);
  return value as TomlTable;
};

const assertWorkspaceDependencies = (manifest: TomlTable, section: string): void => {
  const dependencies = manifest[section];
  if (dependencies === undefined) {
    return;
  }

  for (const [name, dependency] of Object.entries(table(dependencies))) {
    assert.equal(table(dependency).workspace, true, `${section}.${name} must inherit workspace`);
  }
};

const assertAllWorkspaceDependencies = (manifest: TomlTable): void => {
  for (const section of ["dependencies", "build-dependencies", "dev-dependencies"]) {
    assertWorkspaceDependencies(manifest, section);
  }

  const targets = manifest.target;
  if (targets === undefined) {
    return;
  }
  for (const targetManifest of Object.values(table(targets))) {
    const target = table(targetManifest);
    for (const section of ["dependencies", "build-dependencies", "dev-dependencies"]) {
      assertWorkspaceDependencies(target, section);
    }
  }
};

it.layer(NodeServices.layer)("canonical Rust workspace", (it) => {
  it.effect("centralizes Rust package configuration and package scripts", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const repoRoot = yield* path.fromFileUrl(new URL("..", import.meta.url));
      const workspaceManifestPath = path.join(repoRoot, "Cargo.toml");

      assert.equal(yield* fs.exists(workspaceManifestPath), true, "root Cargo workspace manifest");
      assert.equal(
        yield* fs.exists(path.join(repoRoot, "Cargo.lock")),
        true,
        "root Cargo lockfile",
      );

      const gitignore = yield* fs.readFileString(path.join(repoRoot, ".gitignore"));
      assert.equal(
        gitignore.split(/\r?\n/).includes("/target/"),
        true,
        "root Cargo target directory must be ignored",
      );

      const ciWorkflow = yield* fs.readFileString(
        path.join(repoRoot, ".github", "workflows", "ci.yml"),
      );
      assert.equal(
        ciWorkflow.match(/uses: dtolnay\/rust-toolchain@[0-9a-f]{40} # 1\.98\.1/g)?.length ?? 0,
        4,
        "Every Rust CI job must exercise the declared Rust 1.98.1 toolchain",
      );

      const rootPackageJson = yield* decodePackageJson(
        yield* fs.readFileString(path.join(repoRoot, "package.json")),
      );
      const cleanTokens = (rootPackageJson.scripts.clean ?? "").split(/\s+/);
      assert.deepEqual(cleanTokens.slice(0, 2), ["rm", "-rf"]);
      assert.equal(cleanTokens.includes("target"), true);

      const workspaceManifest = parseToml(yield* fs.readFileString(workspaceManifestPath));
      const workspace = table(workspaceManifest.workspace);
      assert.deepEqual(
        workspace.members,
        rustPackages.map(({ memberPath }) => memberPath),
      );

      const workspacePackage = table(workspace.package);
      assert.equal(workspacePackage.edition, "2024");
      assert.equal(workspacePackage["rust-version"], "1.98.1");
      assert.equal(table(table(workspace.lints).rust).warnings, "deny");

      const releaseProfile = table(table(workspaceManifest.profile).release);
      assert.equal(releaseProfile.lto, "thin");
      assert.equal(releaseProfile["codegen-units"], 1);
      assert.equal(releaseProfile.strip, "symbols");
      assert.equal(
        releaseProfile.panic,
        "unwind",
        "Wry Objective-C cancellation recovery requires unwinding",
      );

      for (const rustPackage of rustPackages) {
        assert.equal(
          yield* fs.exists(path.join(repoRoot, rustPackage.memberPath, "Cargo.lock")),
          false,
        );

        const childManifest = parseToml(
          yield* fs.readFileString(path.join(repoRoot, rustPackage.memberPath, "Cargo.toml")),
        );
        const childPackage = table(childManifest.package);
        assert.equal(table(childPackage.edition).workspace, true);
        assert.equal(table(childPackage["rust-version"]).workspace, true);
        assert.equal(table(childManifest.lints).workspace, true);
        assertAllWorkspaceDependencies(childManifest);

        if (!("packageJsonPath" in rustPackage)) {
          continue;
        }

        const packageJson = yield* decodePackageJson(
          yield* fs.readFileString(path.join(repoRoot, rustPackage.packageJsonPath)),
        );
        assert.equal(
          childPackage.version,
          packageJson.version,
          `${rustPackage.memberPath}/Cargo.toml version must match ${rustPackage.packageJsonPath}`,
        );

        for (const scriptName of rustPackage.cargoSelectedScriptNames) {
          const script = packageJson.scripts[scriptName] ?? "";
          assert.equal(
            new RegExp(`(?:^|\\s)-p\\s+${rustPackage.cargoPackage}(?:\\s|$)`).test(script),
            true,
            `${rustPackage.packageJsonPath} ${scriptName}`,
          );
        }
      }
    }),
  );
});
