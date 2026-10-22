// @effect-diagnostics nodeBuiltinImport:off - Workflow contract tests inspect checked-in YAML directly.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import { describe, expect, it } from "vite-plus/test";
import { parse as parseYaml } from "yaml";

const REPOSITORY_ROOT = NodePath.resolve(import.meta.dirname, "..");
const CI_WORKFLOW_PATH = NodePath.join(REPOSITORY_ROOT, ".github/workflows/ci.yml");
const RELEASE_WORKFLOW_PATH = NodePath.join(REPOSITORY_ROOT, ".github/workflows/release.yml");
const DESKTOP_UI_WORKFLOW_PATH = NodePath.join(
  REPOSITORY_ROOT,
  ".github/workflows/desktop-ui-smoke.yml",
);
const DESKTOP_UPGRADE_WORKFLOW_PATH = NodePath.join(
  REPOSITORY_ROOT,
  ".github/workflows/desktop-upgrade-smoke.yml",
);
const PACKAGE_JSON_PATH = NodePath.join(REPOSITORY_ROOT, "package.json");
const SERVER_PACKAGE_JSON_PATH = NodePath.join(REPOSITORY_ROOT, "apps/server/package.json");
const DESKTOP_PACKAGE_JSON_PATH = NodePath.join(REPOSITORY_ROOT, "apps/desktop/package.json");
const RELEASE_GUIDE_PATH = NodePath.join(REPOSITORY_ROOT, "docs/operations/release.md");

interface WorkflowStep {
  readonly env?: Record<string, string>;
  readonly if?: string;
  readonly name?: string;
  readonly run?: string;
  readonly shell?: string;
  readonly uses?: string;
  readonly with?: Record<string, unknown>;
}

interface WorkflowJob {
  readonly env?: Record<string, string>;
  readonly needs?: string | ReadonlyArray<string>;
  readonly outputs?: Record<string, string>;
  readonly "runs-on"?: string;
  readonly "timeout-minutes"?: number | string;
  readonly strategy?: {
    readonly "fail-fast"?: boolean;
    readonly matrix?: {
      readonly include?: ReadonlyArray<Record<string, string | number>>;
    };
  };
  readonly steps?: ReadonlyArray<WorkflowStep>;
}

interface Workflow {
  readonly concurrency?: {
    readonly group?: string;
    readonly "cancel-in-progress"?: boolean;
  };
  readonly on?: Record<string, unknown>;
  readonly jobs?: Record<string, WorkflowJob>;
}

function readWorkflow(path: string): { readonly raw: string; readonly workflow: Workflow } {
  const raw = NodeFS.readFileSync(path, "utf8");
  return { raw, workflow: parseYaml(raw) as Workflow };
}

function requireJob(workflow: Workflow, name: string): WorkflowJob {
  const job = workflow.jobs?.[name];
  if (job === undefined) throw new Error(`Missing workflow job ${name}`);
  return job;
}

function allStepCommands(job: WorkflowJob): string {
  return (job.steps ?? [])
    .map((step) => [step.name, step.if, step.run, step.uses, JSON.stringify(step.with)].join("\n"))
    .join("\n");
}

describe("cross-platform CI contract", () => {
  it("cancels superseded CI and seeded-upgrade runs on the same ref", () => {
    for (const path of [CI_WORKFLOW_PATH, DESKTOP_UPGRADE_WORKFLOW_PATH]) {
      const { workflow } = readWorkflow(path);
      expect(workflow.concurrency).toEqual({
        group: "${{ github.workflow }}-${{ github.ref }}",
        "cancel-in-progress": true,
      });
    }
  });

  it("serializes package tests directly in CI and release preflight", () => {
    for (const [path, job] of [
      [CI_WORKFLOW_PATH, "test"],
      [RELEASE_WORKFLOW_PATH, "preflight"],
    ] as const) {
      const { workflow } = readWorkflow(path);
      const testStep = requireJob(workflow, job).steps?.find((step) => step.name === "Test");

      expect(testStep?.run).toBe("vp run -r --concurrency-limit 1 test");
    }
  });

  it("uses default Rust harness threads in standard package and CI commands", () => {
    const serverPackage = JSON.parse(NodeFS.readFileSync(SERVER_PACKAGE_JSON_PATH, "utf8")) as {
      readonly scripts: Record<string, string>;
    };
    const desktopPackage = JSON.parse(NodeFS.readFileSync(DESKTOP_PACKAGE_JSON_PATH, "utf8")) as {
      readonly scripts: Record<string, string>;
    };
    const { raw: ciWorkflow, workflow } = readWorkflow(CI_WORKFLOW_PATH);
    const rustWorkspaceTestStep = requireJob(workflow, "test").steps?.find(
      (step) => step.name === "Rust workspace tests",
    );
    const releaseGuide = NodeFS.readFileSync(RELEASE_GUIDE_PATH, "utf8");

    expect(serverPackage.scripts.test).toBe(
      "node ../../scripts/run-msvc.mjs cargo test -p bibcode-server -j 2",
    );
    expect(desktopPackage.scripts.test).toBe(
      "node ../../scripts/run-msvc.mjs cargo test -p bibcode-desktop -j 2",
    );
    expect(rustWorkspaceTestStep?.run).toBe("cargo test --workspace -j 2");
    expect(ciWorkflow).not.toContain("--test-threads=1");
    expect(releaseGuide).toContain("node scripts/run-msvc.mjs cargo test -p bibcode-desktop -j 2");
    expect(releaseGuide).not.toContain("cargo test -p bibcode-desktop -j 2 -- --test-threads=1");
  });

  it("keeps portable validation on Ubuntu 24.04", () => {
    const { workflow } = readWorkflow(CI_WORKFLOW_PATH);

    for (const name of ["check", "test", "release_smoke"]) {
      expect(requireJob(workflow, name)["runs-on"]).toBe("ubuntu-24.04");
    }
  });

  it("runs the ignored SSH environment integration after building bibcode", () => {
    const { workflow } = readWorkflow(CI_WORKFLOW_PATH);
    const steps = requireJob(workflow, "test").steps ?? [];
    const names = steps.map((step) => step.name);
    const sshStep = steps.find((step) => step.name === "SSH environment integration");

    expect(names.indexOf("SSH environment integration")).toBeGreaterThan(
      names.indexOf("Rust workspace tests"),
    );
    expect(sshStep?.run?.trim().split("\n")).toEqual([
      "cargo test -p bibcode-server --test cli_smoke --no-run -j 2",
      "cargo test -p bibcode-desktop --test ssh_environment -j 2 -- --ignored",
    ]);
  });

  it("budgets Check and Test for a cold Rust cache plus a stalled apt install", () => {
    const { workflow } = readWorkflow(CI_WORKFLOW_PATH);

    // Cold Check projected at 33 minutes, plus 7 minutes of margin.
    expect(requireJob(workflow, "check")["timeout-minutes"]).toBe(40);
    // Cold Test finished in 44 minutes. A 21-minute apt stall and 10 minutes
    // for the unfinished workspace suite plus SSH is 75.
    expect(requireJob(workflow, "test")["timeout-minutes"]).toBe(75);
  });

  it("saves CI Rust build caches from main so pull requests restore them", () => {
    const { workflow } = readWorkflow(CI_WORKFLOW_PATH);

    for (const name of ["check", "test", "native_desktop", "crash-smoke"] as const) {
      const step = requireJob(workflow, name).steps?.find(
        (candidate) => candidate.name === "Cache Rust build output",
      );
      expect(step?.uses).toContain("Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6");
      expect(step?.with?.["save-if"]).toBe("${{ github.ref == 'refs/heads/main' }}");
      expect(step?.with?.["cache-on-failure"]).toBe("true");
      expect(String(step?.with?.workspaces)).toContain(". -> target");
    }
  });

  it("allows native CI to finish host tests, optimized recovery probes, and bundles", () => {
    const { workflow } = readWorkflow(CI_WORKFLOW_PATH);
    const job = requireJob(workflow, "native_desktop");
    expect(job["timeout-minutes"]).toBe("${{ matrix.jobTimeoutMinutes }}");
    const matrix = job.strategy?.matrix?.include ?? [];
    expect(matrix).toHaveLength(6);
    for (const row of matrix) {
      // Intel's cold optimized probe and native package use distinct dependency
      // feature sets, in addition to the unoptimized host test graph.
      const minimum = row.platform === "mac" && row.arch === "x64" ? 240 : 120;
      expect(row.jobTimeoutMinutes, String(row.label)).toBeGreaterThanOrEqual(minimum);
    }
  });

  it("builds native desktop bundles on every supported runner and architecture", () => {
    const { workflow } = readWorkflow(CI_WORKFLOW_PATH);
    const nativeJob = requireJob(workflow, "native_desktop");
    const matrix = nativeJob.strategy?.matrix?.include ?? [];

    expect(nativeJob.strategy?.["fail-fast"]).toBe(false);
    expect(matrix).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          runner: "ubuntu-22.04-arm",
          platform: "linux",
          target: "appimage",
          arch: "arm64",
        }),
        expect.objectContaining({
          runner: "ubuntu-22.04",
          platform: "linux",
          target: "appimage",
          arch: "x64",
        }),
        expect.objectContaining({
          runner: "windows-11-vs2026-arm",
          platform: "win",
          target: "nsis",
          arch: "arm64",
        }),
        expect.objectContaining({
          runner: "windows-2025",
          platform: "win",
          target: "nsis",
          arch: "x64",
        }),
        expect.objectContaining({
          runner: "macos-26",
          platform: "mac",
          target: "dmg",
          arch: "arm64",
        }),
        expect.objectContaining({
          runner: "macos-26-intel",
          platform: "mac",
          target: "dmg",
          arch: "x64",
        }),
      ]),
    );
    expect(matrix).toHaveLength(6);
  });

  it("installs dependencies once in every CI job", () => {
    const { workflow } = readWorkflow(CI_WORKFLOW_PATH);

    for (const name of ["check", "test", "release_smoke"]) {
      const setupStep = requireJob(workflow, name).steps?.find(
        (step) => step.name === "Setup Vite+",
      );
      expect(setupStep?.with?.["run-install"]).toBe(true);
    }

    const nativeSetupStep = requireJob(workflow, "native_desktop").steps?.find(
      (step) => step.name === "Setup Vite+",
    );
    expect(nativeSetupStep?.with?.["run-install"]).toBe(false);
  });

  it("performs frozen install, version assertions, web build, Rust tests, and bundle build", () => {
    const { workflow } = readWorkflow(CI_WORKFLOW_PATH);
    const commands = allStepCommands(requireJob(workflow, "native_desktop"));

    expect(commands).toMatch(/vp install --frozen-lockfile/);
    expect(commands).toMatch(/node --version/);
    expect(commands).toMatch(/vp --version/);
    expect(commands).toMatch(/rustc --version/);
    expect(commands).toMatch(/vp run --filter @bibcode\/web build/);
    expect(commands).toMatch(/vp run --filter @bibcode\/desktop test/);
    expect(commands).toMatch(/node scripts\/build-desktop-artifact\.ts/);
    expect(commands).not.toMatch(/gh release|softprops\/action-gh-release/);
  });

  it("runs the native desktop E2E support contracts on Windows", () => {
    const { workflow } = readWorkflow(CI_WORKFLOW_PATH);
    const nativeSteps = requireJob(workflow, "native_desktop").steps ?? [];
    const windowsE2eStep = nativeSteps.find(
      (step) => step.name === "Test Windows desktop E2E support contracts",
    );

    expect(windowsE2eStep?.if).toBe("matrix.platform == 'win'");
    expect(windowsE2eStep?.run).toBe(
      "vp test run apps/desktop/e2e/support/test-project.test.ts apps/web/src/components/gitManager/gitManagerRepositoryAvailability.test.ts",
    );
  });

  it("compiles every bibcode-server test target on the Windows native rows", () => {
    // Clippy with --all-targets runs on Linux only, so Unix-only test helpers
    // and imports can leave the Windows server test binaries uncompilable
    // under -D warnings without this gate.
    const { workflow } = readWorkflow(CI_WORKFLOW_PATH);
    const nativeSteps = requireJob(workflow, "native_desktop").steps ?? [];
    const windowsCompileStep = nativeSteps.find(
      (step) => step.name === "Check Windows server test targets compile",
    );

    expect(windowsCompileStep?.if).toBe("matrix.platform == 'win'");
    expect(windowsCompileStep?.run).toBe(
      "node scripts/run-msvc.mjs cargo check -p bibcode-server --all-targets",
    );
  });

  it("installs the full official Linux Tauri prerequisite set", () => {
    const { workflow } = readWorkflow(CI_WORKFLOW_PATH);
    const commands = allStepCommands(requireJob(workflow, "native_desktop"));

    for (const dependency of [
      "build-essential",
      "curl",
      "wget",
      "file",
      "libxdo-dev",
      "libssl-dev",
      "libgtk-3-dev",
      "libwebkit2gtk-4.1-dev",
      "libayatana-appindicator3-dev",
      "librsvg2-dev",
      "patchelf",
    ]) {
      expect(commands).toContain(dependency);
    }
    expect(commands).not.toContain("libappindicator3-dev");
  });

  it("installs xdg-utils in every Linux AppImage build workflow", () => {
    const jobs = [
      requireJob(readWorkflow(CI_WORKFLOW_PATH).workflow, "native_desktop"),
      requireJob(readWorkflow(RELEASE_WORKFLOW_PATH).workflow, "build_desktop"),
      requireJob(readWorkflow(DESKTOP_UI_WORKFLOW_PATH).workflow, "desktop_ui_smoke"),
      requireJob(readWorkflow(DESKTOP_UPGRADE_WORKFLOW_PATH).workflow, "seeded_upgrade_smoke"),
    ];

    for (const job of jobs) {
      expect(allStepCommands(job)).toContain("xdg-utils");
    }
  });
});

describe("cross-platform release contract", () => {
  it("gives the publication preflight enough time for a cold full test graph", () => {
    const { workflow } = readWorkflow(RELEASE_WORKFLOW_PATH);

    expect(requireJob(workflow, "preflight")["timeout-minutes"]).toBeGreaterThanOrEqual(60);
  });

  it("installs the pinned Rust toolchain before parallel preflight typechecks", () => {
    const { workflow } = readWorkflow(RELEASE_WORKFLOW_PATH);
    const steps = requireJob(workflow, "preflight").steps ?? [];
    const setupRustIndex = steps.findIndex((step) => step.name === "Setup Rust");
    const typecheckIndex = steps.findIndex((step) => step.name === "Typecheck");

    expect(setupRustIndex).toBeGreaterThan(-1);
    expect(typecheckIndex).toBeGreaterThan(setupRustIndex);
    expect(steps[setupRustIndex]?.uses).toBe(
      "dtolnay/rust-toolchain@18f0487477426dce244608110aa2660c2893deb7",
    );
  });

  it("builds AppImage on Ubuntu 22.04 with the complete Linux prerequisites", () => {
    const { workflow } = readWorkflow(RELEASE_WORKFLOW_PATH);
    const build = requireJob(workflow, "build_desktop");
    const linux = (build.strategy?.matrix?.include ?? []).find(
      (entry) => entry.platform === "linux" && entry.arch === "x64",
    );
    const commands = allStepCommands(build);

    expect(linux?.runner).toBe("ubuntu-22.04");
    expect(commands).toContain("libayatana-appindicator3-dev");
    expect(commands).not.toContain("libappindicator3-dev");
    expect(commands).toContain("patchelf");
  });

  it("verifies complete ad-hoc signatures before publishing macOS DMGs", () => {
    const { workflow } = readWorkflow(RELEASE_WORKFLOW_PATH);
    const steps = requireJob(workflow, "build_desktop").steps ?? [];
    const buildIndex = steps.findIndex((step) => step.name === "Build desktop artifact");
    const verifyIndex = steps.findIndex(
      (step) => step.name === "Verify macOS ad-hoc application signature",
    );
    const collectIndex = steps.findIndex((step) => step.name === "Collect release assets");
    const verify = steps[verifyIndex];

    expect(verifyIndex).toBeGreaterThan(buildIndex);
    expect(collectIndex).toBeGreaterThan(verifyIndex);
    expect(verify?.if).toBe("matrix.platform == 'mac'");
    expect(verify?.run).toContain("set -euo pipefail");
    expect(verify?.run).toContain("shopt -s nullglob");
    expect(verify?.run).toContain("if (( ${#dmg_paths[@]} != 1 )); then");
    expect(verify?.run).toContain("hdiutil attach -readonly -nobrowse -noautoopen");
    expect(verify?.run).toContain("if (( ${#app_paths[@]} != 1 )); then");
    expect(verify?.run).toContain("codesign --verify --deep --strict --verbose=4");
    expect(verify?.run).toContain("Signature=adhoc");
    expect(verify?.run).toContain("TeamIdentifier=not set");
    expect(verify?.run).toContain("trap cleanup EXIT");
    expect(verify?.run).toContain(
      'hdiutil detach "$mount_dir" >/dev/null 2>&1 || hdiutil detach -force "$mount_dir"',
    );
    expect(verify?.run).not.toContain("attached=");
  });

  it("classifies only numeric stable releases as updater candidates", () => {
    const { workflow } = readWorkflow(RELEASE_WORKFLOW_PATH);
    const preflight = requireJob(workflow, "preflight");
    const releaseMeta = preflight.steps?.find((step) => step.name === "Resolve release version");
    const run = releaseMeta?.run ?? "";

    expect(preflight.outputs?.is_update_candidate).toBe(
      "${{ steps.release_meta.outputs.is_update_candidate }}",
    );
    expect(run).toMatch(
      /release_channel=nightly[\s\S]*is_prerelease=true[\s\S]*is_update_candidate=false/,
    );
    expect(run).toMatch(
      /if \[\[ "\$version" =~ \^\[0-9\][\s\S]*is_prerelease=false[\s\S]*is_update_candidate=true[\s\S]*else[\s\S]*is_prerelease=true[\s\S]*is_update_candidate=false/,
    );
  });

  it("signs and collects updater artifacts only for updater candidates", () => {
    const { workflow } = readWorkflow(RELEASE_WORKFLOW_PATH);
    const build = requireJob(workflow, "build_desktop");
    const commands = allStepCommands(build);
    const releaseAssets = build.steps?.find((step) => step.name === "Collect release assets");
    const signingCheck = build.steps?.find(
      (step) => step.name === "Verify stable updater signing configuration",
    );

    expect(build.env?.TAURI_SIGNING_PRIVATE_KEY).toBe(
      "${{ needs.preflight.outputs.is_update_candidate == 'true' && secrets.TAURI_SIGNING_PRIVATE_KEY || '' }}",
    );
    expect(build.env?.TAURI_SIGNING_PRIVATE_KEY_PASSWORD).toBe(
      "${{ needs.preflight.outputs.is_update_candidate == 'true' && secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD || '' }}",
    );
    expect(signingCheck?.if).toBe("needs.preflight.outputs.is_update_candidate == 'true'");
    expect(commands).toMatch(/TAURI_SIGNING_PRIVATE_KEY/);
    expect(commands).toMatch(/TAURI_SIGNING_PRIVATE_KEY_PASSWORD/);
    expect(commands).toMatch(
      /needs\.preflight\.outputs\.is_update_candidate[^]*args\+=\(--updater\)/,
    );
    expect(releaseAssets?.run).toMatch(/release\/\*\.app\.tar\.gz/);
    expect(releaseAssets?.run).toMatch(/release\/\*\.AppImage/);
    expect(releaseAssets?.run).toMatch(/release\/\*\.exe/);
    expect(releaseAssets?.run).toMatch(/release\/\*\.sig/);
    expect(releaseAssets?.run).toMatch(/release\/updater-\*\.json/);
    expect(releaseAssets?.run).toMatch(/is_update_candidate/);
  });

  it("builds independent six-target desktop and server release matrices", () => {
    const { workflow } = readWorkflow(RELEASE_WORKFLOW_PATH);
    const desktop = requireJob(workflow, "build_desktop");
    const server = requireJob(workflow, "build_server");
    const release = requireJob(workflow, "release");

    // Intel x64 needs more than 90 minutes when its Rust cache is cold.
    expect(desktop["timeout-minutes"]).toBe("${{ matrix.jobTimeoutMinutes }}");
    expect(desktop.strategy?.matrix?.include).toEqual([
      {
        label: "macOS arm64",
        runner: "macos-26",
        platform: "mac",
        target: "dmg",
        arch: "arm64",
        jobTimeoutMinutes: 90,
      },
      {
        label: "macOS x64",
        runner: "macos-15-intel",
        platform: "mac",
        target: "dmg",
        arch: "x64",
        jobTimeoutMinutes: 150,
      },
      {
        label: "Linux arm64",
        runner: "ubuntu-22.04-arm",
        platform: "linux",
        target: "appimage",
        arch: "arm64",
        jobTimeoutMinutes: 90,
      },
      {
        label: "Linux x64",
        runner: "ubuntu-22.04",
        platform: "linux",
        target: "appimage",
        arch: "x64",
        jobTimeoutMinutes: 90,
      },
      {
        label: "Windows arm64",
        runner: "windows-11-vs2026-arm",
        platform: "win",
        target: "nsis",
        arch: "arm64",
        jobTimeoutMinutes: 90,
      },
      {
        label: "Windows x64",
        runner: "windows-2025",
        platform: "win",
        target: "nsis",
        arch: "x64",
        jobTimeoutMinutes: 90,
      },
    ]);
    expect(server.strategy?.matrix?.include).toEqual([
      {
        label: "macOS arm64",
        runner: "macos-26",
        platform: "mac",
        arch: "arm64",
        serverOs: "darwin",
        serverArch: "aarch64",
      },
      {
        label: "macOS x64",
        runner: "macos-15-intel",
        platform: "mac",
        arch: "x64",
        serverOs: "darwin",
        serverArch: "x86_64",
      },
      {
        label: "Linux arm64",
        runner: "ubuntu-22.04-arm",
        platform: "linux",
        arch: "arm64",
        serverOs: "linux",
        serverArch: "aarch64",
      },
      {
        label: "Linux x64",
        runner: "ubuntu-22.04",
        platform: "linux",
        arch: "x64",
        serverOs: "linux",
        serverArch: "x86_64",
      },
      {
        label: "Windows arm64",
        runner: "windows-11-vs2026-arm",
        platform: "win",
        arch: "arm64",
        serverOs: "windows",
        serverArch: "aarch64",
      },
      {
        label: "Windows x64",
        runner: "windows-2025",
        platform: "win",
        arch: "x64",
        serverOs: "windows",
        serverArch: "x86_64",
      },
    ]);
    expect(allStepCommands(server)).toContain("scripts/build-server-artifact.ts");
    expect(allStepCommands(server)).toContain('chown -R "$HOST_UID:$HOST_GID" /workspace/target');
    expect(allStepCommands(server)).toContain("scripts/smoke-server-distribution.ts");
    expect(release.needs).toEqual(["preflight", "build_desktop", "build_server"]);
  });

  it("cryptographically verifies every updater payload before creating a stable draft", () => {
    const { workflow } = readWorkflow(RELEASE_WORKFLOW_PATH);
    const steps = requireJob(workflow, "release").steps ?? [];
    const setupRustIndex = steps.findIndex((step) => step.name === "Setup Rust");
    const manifestIndex = steps.findIndex((step) => step.name === "Build Tauri update manifest");
    const verifyIndex = steps.findIndex((step) => step.name === "Verify Tauri update signatures");
    const draftIndex = steps.findIndex((step) => step.name === "Publish stable release");
    const verify = steps[verifyIndex];

    expect(setupRustIndex).toBeGreaterThan(-1);
    expect(manifestIndex).toBeGreaterThan(setupRustIndex);
    expect(verifyIndex).toBeGreaterThan(manifestIndex);
    expect(draftIndex).toBeGreaterThan(verifyIndex);
    expect(verify?.if).toBe("needs.preflight.outputs.is_update_candidate == 'true'");
    expect(verify?.run).toContain(
      "cargo run --locked -p bibcode-updater-verifier -- apps/desktop/src-tauri/tauri.release.conf.json release-assets/latest.json release-assets",
    );
  });

  it("requires a second manually approved stable run before publication", () => {
    const { raw, workflow } = readWorkflow(RELEASE_WORKFLOW_PATH);
    const preflight = requireJob(workflow, "preflight");
    const release = requireJob(workflow, "release");
    const requireDraft = release.steps?.find(
      (step) => step.name === "Require inspected stable draft",
    );
    const prepareDraft = release.steps?.find(
      (step) => step.name === "Prepare stable release assets",
    );
    const createDraft = release.steps?.find((step) => step.name === "Publish stable release");
    const createFirstDraft = release.steps?.find(
      (step) => step.name === "Publish first stable release",
    );
    const verifyDraft = release.steps?.find(
      (step) => step.name === "Verify stable draft release assets",
    );
    const publish = release.steps?.find((step) => step.name === "Publish approved stable release");

    expect(raw).toMatch(
      /publish:\s*\n\s+description:[^\n]+\n\s+required: false\n\s+default: false\n\s+type: boolean/,
    );
    expect(preflight.outputs?.publish_requested).toBe(
      "${{ steps.release_meta.outputs.publish_requested }}",
    );
    expect(requireDraft?.if).toContain("needs.preflight.outputs.validate_only != 'true'");
    expect(requireDraft?.if).toContain(
      "needs.preflight.outputs.release_channel == 'stable' && needs.preflight.outputs.publish_requested == 'true'",
    );
    expect(requireDraft?.run).toMatch(/gh release view[^]*isDraft/);
    for (const draftStep of [prepareDraft, createDraft, createFirstDraft, verifyDraft]) {
      expect(draftStep?.if).toContain("needs.preflight.outputs.validate_only != 'true'");
      expect(draftStep?.if).toContain("needs.preflight.outputs.publish_requested != 'true'");
    }
    expect(publish?.if).toContain("needs.preflight.outputs.validate_only != 'true'");
    expect(publish?.if).toContain(
      "needs.preflight.outputs.release_channel == 'stable' && needs.preflight.outputs.publish_requested == 'true'",
    );
    expect(publish?.run).toMatch(/gh release edit[^]*--draft=false/);
    expect(publish?.run).toMatch(/IS_UPDATE_CANDIDATE[^]*--latest/);
  });

  it("does not reintroduce scheduled nightly releases", () => {
    const ci = readWorkflow(CI_WORKFLOW_PATH);
    const release = readWorkflow(RELEASE_WORKFLOW_PATH);

    expect(ci.workflow.on?.schedule).toBeUndefined();
    expect(release.workflow.on?.schedule).toBeUndefined();
    expect(release.raw).toMatch(/workflow_dispatch:/);
    expect(release.raw).toMatch(/- nightly/);
  });
});

describe("packaged desktop UI smoke contract", () => {
  it("is manual and reusable without scheduled or release publishing triggers", () => {
    const { raw, workflow } = readWorkflow(DESKTOP_UI_WORKFLOW_PATH);

    expect(workflow.on?.workflow_dispatch).toBeDefined();
    expect(workflow.on?.workflow_call).toBeDefined();
    expect(workflow.on?.schedule).toBeUndefined();
    expect(raw).not.toMatch(/softprops\/action-gh-release|gh release|npm publish/);
  });

  it("builds and tests packaged applications on all supported native runners", () => {
    const { workflow } = readWorkflow(DESKTOP_UI_WORKFLOW_PATH);
    const smoke = requireJob(workflow, "desktop_ui_smoke");
    const matrix = smoke.strategy?.matrix?.include ?? [];
    const commands = allStepCommands(smoke);

    expect(smoke.strategy?.["fail-fast"]).toBe(false);
    expect(matrix).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          runner: "ubuntu-22.04-arm",
          platform: "linux",
          arch: "arm64",
        }),
        expect.objectContaining({ runner: "ubuntu-22.04", platform: "linux", arch: "x64" }),
        expect.objectContaining({
          runner: "windows-11-vs2026-arm",
          platform: "win",
          arch: "arm64",
        }),
        expect.objectContaining({ runner: "windows-2025", platform: "win", arch: "x64" }),
        expect.objectContaining({ runner: "macos-26", platform: "mac", arch: "arm64" }),
        expect.objectContaining({
          runner: "macos-26-intel",
          platform: "mac",
          arch: "x64",
        }),
      ]),
    );
    expect(matrix).toHaveLength(6);
    expect(commands).toMatch(/vp install --frozen-lockfile/);
    expect(commands).toMatch(/test:ui:build/);
    expect(commands).toMatch(/test:ui:desktop/);
    expect(commands).toMatch(/xvfb-run/);
    expect(commands).toMatch(/hdiutil attach/);
    expect(commands).toMatch(/hdiutil detach/);
    expect(commands).toContain("-name 'BiBCode.app'");
    expect(commands).toContain('"BiBCode.exe", "bibcode-desktop.exe"');
    expect(commands).not.toMatch(/bundle\/macos.*\.app/);
    expect(commands).toMatch(/always\(\)/);
    expect(commands).toMatch(/actions\/upload-artifact/);
  });

  it("resolves runner temporary paths only where the runner context is available", () => {
    const { workflow } = readWorkflow(DESKTOP_UI_WORKFLOW_PATH);
    const smoke = requireJob(workflow, "desktop_ui_smoke");
    const runStep = smoke.steps?.find((step) => step.name === "Run packaged desktop UI smoke");

    expect(JSON.stringify(smoke.env ?? {})).not.toContain("runner.temp");
    expect(runStep?.env?.BIBCODE_E2E_ARTIFACT_DIR).toBe(
      "${{ runner.temp }}/bibcode-desktop-ui-artifacts",
    );
  });

  it("exports an absolute Linux AppImage path to the packaged UI harness", () => {
    const { workflow } = readWorkflow(DESKTOP_UI_WORKFLOW_PATH);
    const smoke = requireJob(workflow, "desktop_ui_smoke");
    const resolveStep = smoke.steps?.find((step) => step.name === "Resolve Linux AppImage");

    expect(resolveStep?.run).toContain(
      'find "$GITHUB_WORKSPACE/target/${{ matrix.rustTarget }}/release/bundle/appimage"',
    );
    expect(resolveStep?.run).toContain('echo "BIBCODE_E2E_APP_PATH=$app_path"');
  });

  it("extracts the completed Linux AppImage and rejects a bundled Wayland client", () => {
    const { workflow } = readWorkflow(DESKTOP_UI_WORKFLOW_PATH);
    const steps = requireJob(workflow, "desktop_ui_smoke").steps ?? [];
    const resolveIndex = steps.findIndex((step) => step.name === "Resolve Linux AppImage");
    const verifyIndex = steps.findIndex(
      (step) => step.name === "Verify Linux AppImage portability",
    );
    const runIndex = steps.findIndex((step) => step.name === "Run packaged desktop UI smoke");
    const verify = steps[verifyIndex];

    expect(resolveIndex).toBeGreaterThan(-1);
    expect(verifyIndex).toBeGreaterThan(resolveIndex);
    expect(runIndex).toBeGreaterThan(verifyIndex);
    expect(verify?.if).toBe("matrix.platform == 'linux'");
    expect(verify?.shell).toBe("bash");
    expect(verify?.run).toContain('"$BIBCODE_E2E_APP_PATH" --appimage-extract');
    expect(verify?.run).toContain("-name 'libwayland-client.so*'");
    expect(verify?.run).toContain("trap cleanup EXIT");
    expect(verify?.run).toContain("exit 1");
  });

  it("uploads native packages and only bounded UI evidence", () => {
    const { workflow } = readWorkflow(DESKTOP_UI_WORKFLOW_PATH);
    const smoke = requireJob(workflow, "desktop_ui_smoke");
    const packageStep = smoke.steps?.find(
      (step) => step.name === "Upload packaged desktop application",
    );
    const evidenceStep = smoke.steps?.find(
      (step) => step.name === "Upload desktop UI screenshots and logs",
    );
    const packagePaths = String(packageStep?.with?.path);
    const evidencePaths = String(evidenceStep?.with?.path);

    expect(packageStep?.if).toBe("always()");
    expect(packagePaths).toContain(
      "target/${{ matrix.rustTarget }}/release/bundle/appimage/*.AppImage",
    );
    expect(packagePaths).toContain("target/${{ matrix.rustTarget }}/release/bundle/nsis/*.exe");
    expect(packagePaths).toContain("target/${{ matrix.rustTarget }}/release/bundle/dmg/*.dmg");
    expect(evidencePaths).toContain("/*.png");
    expect(evidencePaths).toContain("/*.log");
    expect(evidencePaths).not.toContain("/state/");
  });
});

describe("seeded packaged desktop upgrade contract", () => {
  it("exposes the seeded upgrade harness as a repository command", () => {
    const packageJson = JSON.parse(NodeFS.readFileSync(PACKAGE_JSON_PATH, "utf8")) as {
      readonly scripts?: Record<string, string>;
    };

    expect(packageJson.scripts?.["test:desktop:upgrade"]).toBe(
      "node scripts/seeded-desktop-upgrade-smoke.ts",
    );
  });

  it("is release-blocking on the exact supported updater matrix", () => {
    const { workflow } = readWorkflow(DESKTOP_UPGRADE_WORKFLOW_PATH);
    const smoke = requireJob(workflow, "seeded_upgrade_smoke");

    expect(workflow.on?.pull_request).toBeDefined();
    expect(workflow.on?.workflow_dispatch).toBeDefined();
    expect(smoke.strategy?.["fail-fast"]).toBe(false);
    expect(smoke.strategy?.matrix?.include).toEqual([
      {
        label: "Linux arm64 AppImage",
        runner: "ubuntu-22.04-arm",
        platform: "linux",
        arch: "arm64",
        bundle: "appimage",
        jobTimeoutMinutes: 240,
      },
      {
        label: "Linux x64 AppImage",
        runner: "ubuntu-22.04",
        platform: "linux",
        arch: "x64",
        bundle: "appimage",
        jobTimeoutMinutes: 240,
      },
      {
        label: "Windows arm64 NSIS",
        runner: "windows-11-vs2026-arm",
        platform: "win",
        arch: "arm64",
        bundle: "nsis",
        jobTimeoutMinutes: 240,
      },
      {
        label: "Windows x64 NSIS",
        runner: "windows-2025",
        platform: "win",
        arch: "x64",
        bundle: "nsis",
        jobTimeoutMinutes: 240,
      },
      {
        label: "macOS arm64",
        runner: "macos-26",
        platform: "mac",
        arch: "arm64",
        bundle: "dmg",
        jobTimeoutMinutes: 240,
      },
      {
        label: "macOS x64",
        runner: "macos-26-intel",
        platform: "mac",
        arch: "x64",
        bundle: "dmg",
        jobTimeoutMinutes: 360,
      },
    ]);
  });

  it("budgets three cold Intel packages and retains setup, every upgrade lane, and evidence", () => {
    const { workflow } = readWorkflow(DESKTOP_UPGRADE_WORKFLOW_PATH);
    const smoke = requireJob(workflow, "seeded_upgrade_smoke");
    expect(smoke["timeout-minutes"]).toBe("${{ matrix.jobTimeoutMinutes }}");
    const intel = smoke.strategy?.matrix?.include?.find(
      (row) => row.platform === "mac" && row.arch === "x64",
    );
    // Three separate Cargo outputs, three bounded frozen installs, plus an hour
    // for setup, runtime verification, evidence retention, and cleanup.
    expect(intel?.jobTimeoutMinutes).toBe(3 * 90 + 3 * 10 + 60);
    expect(requireJob(workflow, "windows_wsl_upgrade_smoke")["timeout-minutes"]).toBe(240);
    expect(smoke.steps?.map((step) => step.name)).toEqual([
      "Checkout complete release history",
      "Setup Vite+",
      "Setup Rust",
      "Cache Rust build output",
      "Install Linux packaged-updater prerequisites",
      "Install frozen dependencies",
      "Resolve previous stable and isolated candidate versions",
      "Generate ephemeral updater signing key",
      "Audit Windows processes and keep crash dumps",
      "Run previous-stable, protected-baseline, and remote-install upgrades",
      "Upload bounded seeded-upgrade evidence",
      "Upload Windows crash dumps",
    ]);
    // Only Windows lanes keep dumps, and only for a week: they hold process memory.
    const dumps = smoke.steps?.find((step) => step.name === "Upload Windows crash dumps");
    expect(dumps?.if).toBe("always() && matrix.platform == 'win'");
    expect(dumps?.with?.["retention-days"]).toBe(7);
    const commands = allStepCommands(smoke);
    expect(commands).toContain("--restart-timeout-ms 180000");
    expect(commands).toContain(". -> target");
  });

  it("runs both seeded lanes with ephemeral signing and bounded redacted evidence", () => {
    const { raw, workflow } = readWorkflow(DESKTOP_UPGRADE_WORKFLOW_PATH);
    const smoke = requireJob(workflow, "seeded_upgrade_smoke");
    const commands = allStepCommands(smoke);

    expect(commands).toContain("scripts/resolve-previous-release-tag.ts");
    expect(commands).toContain("scripts/seeded-desktop-upgrade-smoke.ts");
    expect(commands).toContain("--previous-tag");
    expect(commands).toContain("--candidate-version");
    expect(commands).toContain('--current-tag "v2147483647.0.0"');
    expect(commands).not.toContain("apps/desktop/package.json");
    expect(commands).toContain("TAURI_SIGNING_PRIVATE_KEY");
    expect(commands).toContain("TAURI_SIGNING_PRIVATE_KEY_PASSWORD");
    expect(raw).toContain("previous-stable");
    expect(raw).toContain("protected-baseline");
    expect(raw).not.toMatch(
      /cat\s+.*(?:private|signing).*key|echo\s+["']?\$(?:TAURI_SIGNING_PRIVATE_KEY|key_password)["']?\s*(?:$|[|>])/m,
    );

    const evidence = smoke.steps?.find(
      (step) => step.name === "Upload bounded seeded-upgrade evidence",
    );
    expect(evidence?.if).toBe("always()");
    expect(String(evidence?.with?.path)).toContain("/evidence/");
    expect(String(evidence?.with?.path)).not.toMatch(/state\.sqlite|\/data\//);
  });

  it("runs WSL identity coverage or records the unavailable capability reason", () => {
    const { workflow } = readWorkflow(DESKTOP_UPGRADE_WORKFLOW_PATH);
    const wsl = requireJob(workflow, "windows_wsl_upgrade_smoke");
    const commands = allStepCommands(wsl);

    expect(wsl["runs-on"]).toBe("windows-2025");
    expect(commands).toContain("wsl --status");
    expect(commands).toContain("capability_reason");
    expect(commands).toContain("GITHUB_STEP_SUMMARY");
    expect(commands).toContain("--wsl");
    expect(commands).toContain("scripts/seeded-desktop-upgrade-smoke.ts");
    expect(commands).toContain("cargo build -p bibcode-server --bin bibcode --release");
    expect(commands).toContain("BIBCODE_WSL_SERVER_BINARY");
    expect(commands).toContain('--current-tag "v2147483647.0.0"');
  });
});

describe("release symbols and crash smoke contract", () => {
  const symbolProfile = [
    "CARGO_PROFILE_RELEASE_STRIP=none",
    "CARGO_PROFILE_RELEASE_DEBUG=line-tables-only",
  ];

  function stepNamed(job: WorkflowJob, name: string): WorkflowStep {
    const step = job.steps?.find((candidate) => candidate.name === name);
    if (step === undefined) throw new Error(`Missing workflow step ${name}`);
    return step;
  }

  function stepIndex(job: WorkflowJob, name: string): number {
    return (job.steps ?? []).findIndex((step) => step.name === name);
  }

  it("installs dump_syms and minidump-stackwalk at pinned versions only", () => {
    for (const path of [CI_WORKFLOW_PATH, RELEASE_WORKFLOW_PATH]) {
      const { raw } = readWorkflow(path);
      const installs = raw.match(/cargo install (?:dump_syms|minidump-stackwalk)[^\n]*/g) ?? [];
      expect(installs.length).toBeGreaterThan(0);
      for (const install of installs) {
        expect(install).toMatch(
          /^cargo install (?:dump_syms --locked --version 2\.3\.9|minidump-stackwalk --locked --version 0\.27\.0)$/,
        );
      }
    }
  });

  it("crashes, dumps, and symbolicates the smoke binary on every OS", () => {
    const { workflow } = readWorkflow(CI_WORKFLOW_PATH);
    const job = requireJob(workflow, "crash-smoke");
    const crash = stepNamed(job, "Crash the smoke binary");
    const symbolicate = stepNamed(job, "Symbolicate the dump");
    const upload = stepNamed(job, "Upload crash smoke evidence");

    expect(job.strategy?.["fail-fast"]).toBe(false);
    expect((job.strategy?.matrix as { os?: unknown } | undefined)?.os).toEqual([
      "ubuntu-24.04",
      "windows-2025",
      "macos-26",
    ]);
    expect(stepIndex(job, "Install pinned symbol tools")).toBeLessThan(
      stepIndex(job, "Crash the smoke binary"),
    );
    // Every multi-command step runs in bash, which stops at the first failure on every OS.
    for (const step of job.steps ?? []) {
      if ((step.run ?? "").trim().includes("\n")) expect(step.shell, step.name).toBe("bash");
    }
    expect(crash.env?.CRASH_SMOKE_OUT).toBe("${{ runner.temp }}/crash-smoke");
    for (const setting of symbolProfile) expect(crash.run).toContain(setting);
    expect(crash.run).toContain('RUSTFLAGS="-C link-arg=-Wl,--build-id=sha1"');
    expect(crash.run).toContain("CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO=packed");
    expect(crash.run).toContain(
      "node scripts/run-msvc.mjs cargo test -p bibcode-server --test crash_smoke --release",
    );
    expect(symbolicate.env?.CRASH_SMOKE_OUT).toBe(crash.env?.CRASH_SMOKE_OUT);
    expect(symbolicate.run).toContain("node scripts/dump-symbols.ts");
    expect(symbolicate.run).toContain("--dsym target/release/crash-smoke.dSYM");
    expect(symbolicate.run).toContain("--pdb target/release/crash_smoke.pdb");
    expect(symbolicate.run).toContain("minidump-stackwalk --json");
    expect(symbolicate.run).toContain("grep -q crash_here_in_named_function");
    // BiBCode's own symbolication, from the release build the crash step made, names it too.
    expect(symbolicate.run).toContain(
      '"$bibcode" profiler symbolicate "$(basename "${dumps[0]}" .dmp)"',
    );
    expect(symbolicate.run).toContain('--symbols "$out/symbols" --json --base-dir "$out"');
    expect(symbolicate.run).toContain(
      'grep -q crash_here_in_named_function "$out/symbolicate.json"',
    );
    // No dump fails with its count, Linux proves a sha1 build-id, and the caller is unwound by CFI.
    expect(symbolicate.run).toContain("shopt -s nullglob");
    expect(symbolicate.run).toContain("Expected one minidump in");
    expect(symbolicate.run).toContain("Build ID: [0-9a-f]{40}$");
    expect(symbolicate.run).toContain('name(caller) !== "crash_smoke::main"');
    expect(symbolicate.run).toContain('caller?.trust !== "cfi"');
    expect(upload.if).toBe("failure()");
    expect(upload.with?.path).toBe("${{ runner.temp }}/crash-smoke");
  });

  it("dumps desktop symbols during the artifact build and publishes their archive", () => {
    const { workflow } = readWorkflow(RELEASE_WORKFLOW_PATH);
    const build = requireJob(workflow, "build_desktop");

    expect(stepIndex(build, "Install dump_syms")).toBeLessThan(
      stepIndex(build, "Build desktop artifact"),
    );
    expect(stepNamed(build, "Build desktop artifact").run).toContain("--symbols");
    expect(stepNamed(build, "Collect release assets").run).toContain(
      '"release/bibcode-desktop-symbols-*.zip"',
    );
  });

  it("builds the server with symbols, then dumps and strips it on the host before packaging", () => {
    const { workflow } = readWorkflow(RELEASE_WORKFLOW_PATH);
    const server = requireJob(workflow, "build_server");
    const linux = stepNamed(server, "Build Linux server on the compatibility baseline");
    const native = stepNamed(server, "Build macOS or Windows server");
    const dump = stepNamed(server, "Dump and strip server symbols");

    for (const setting of symbolProfile) expect(linux.run).toContain(`--env ${setting}`);
    expect(linux.run).toContain('--env "RUSTFLAGS=-C link-arg=-Wl,--build-id=sha1"');
    expect(native.env).toEqual({
      CARGO_PROFILE_RELEASE_STRIP: "none",
      CARGO_PROFILE_RELEASE_DEBUG: "line-tables-only",
      CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO: "packed",
    });
    expect(stepIndex(server, "Dump and strip server symbols")).toBeGreaterThan(
      stepIndex(server, "Build macOS or Windows server"),
    );
    expect(stepIndex(server, "Dump and strip server symbols")).toBeLessThan(
      stepIndex(server, "Build standalone server artifacts"),
    );
    expect(dump.run).toContain(
      '--zip "server-release/bibcode-server-symbols-${{ needs.preflight.outputs.version }}-${{ matrix.serverOs }}-${{ matrix.serverArch }}.zip"',
    );
    expect(dump.run).toContain('strip --strip-all -o "$stripped" "$release_dir/bibcode"');
    // Plain strip: rustc's level for macOS executables (`-x` is for dynamic libraries).
    expect(dump.run).toContain('mac) strip -o "$stripped" "$release_dir/bibcode"');
    expect(dump.run).not.toContain("strip -x");
    expect(dump.run).toContain('mv "$stripped" "$release_dir/bibcode"');
    expect(dump.run).toContain('codesign --verify --verbose=2 "$release_dir/bibcode"');
    expect(stepNamed(server, "Upload server artifacts").with?.path).toContain(
      "server-release/*.zip",
    );
  });

  it("gives the release job time to move the symbol archives", () => {
    const { workflow } = readWorkflow(RELEASE_WORKFLOW_PATH);

    expect(requireJob(workflow, "release")["timeout-minutes"]).toBeGreaterThanOrEqual(25);
  });
});
