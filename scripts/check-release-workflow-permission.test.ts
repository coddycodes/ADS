// @effect-diagnostics nodeBuiltinImport:off - This integration test executes the real release shell adapter against disposable Git repositories.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { afterEach, describe, expect, it } from "vite-plus/test";
import { parse as parseYaml } from "yaml";

const repositoryRoot = NodePath.resolve(import.meta.dirname, "..");
const script = NodePath.join(import.meta.dirname, "check-release-workflow-permission.sh");
const temporaryDirectories: string[] = [];

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    NodeFS.rmSync(directory, { recursive: true, force: true });
  }
});

function fixture() {
  const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "release-workflow-permission-"));
  temporaryDirectories.push(root);
  const remote = NodePath.join(root, "remote");
  const checkout = NodePath.join(root, "checkout");
  const bin = NodePath.join(root, "bin");
  NodeFS.mkdirSync(remote);
  NodeFS.mkdirSync(bin);
  // oxlint-disable-next-line coddy/no-global-process-runtime -- Preserve only host command lookup; isolate every Git and home setting in the subprocess.
  const inheritedPath = process.env.PATH ?? "";
  const env = {
    PATH: `${bin}${NodePath.delimiter}${inheritedPath}`,
    HOME: root,
    USERPROFILE: root,
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_CONFIG_GLOBAL: NodePath.join(root, "gitconfig"),
    GIT_TERMINAL_PROMPT: "0",
    GITHUB_REPOSITORY: "fixture/release",
  };
  const git = (cwd: string, ...args: string[]) =>
    NodeChildProcess.execFileSync("git", args, {
      cwd,
      env,
      encoding: "utf8",
      timeout: 10_000,
    }).trim();
  NodeFS.writeFileSync(
    NodePath.join(bin, "gh"),
    '#!/bin/sh\n[ "$*" = "api repos/fixture/release --jq .default_branch" ] || exit 9\nprintf "release/trunk\\n"\n',
    { mode: 0o755 },
  );
  git(remote, "init", "-q", "--initial-branch=release/trunk");
  git(remote, "config", "user.name", "Release fixture");
  git(remote, "config", "user.email", "release@example.invalid");
  const commit = (path: string, contents: string) => {
    const target = NodePath.join(remote, path);
    NodeFS.mkdirSync(NodePath.dirname(target), { recursive: true });
    NodeFS.writeFileSync(target, contents);
    git(remote, "add", "--", path);
    git(remote, "commit", "-q", "-m", "fixture change");
    return git(remote, "rev-parse", "HEAD");
  };
  const releaseRef = commit(".github/workflows/ci.yml", "name: CI\n");
  git(root, "clone", "-q", remote, checkout);
  const run = (ref = releaseRef) =>
    NodeChildProcess.spawnSync("bash", [script, ref], {
      cwd: checkout,
      env,
      encoding: "utf8",
      timeout: 10_000,
    });
  return { root, remote, checkout, bin, git, commit, run, releaseRef };
}

// The adapter runs only on the Ubuntu release controller; these real shell/Git
// scenarios also run on macOS. Native Windows validates the workflow wiring.
// oxlint-disable-next-line coddy/no-global-process-runtime -- Host capability gate for a Unix shell integration test.
describe.skipIf(process.platform === "win32")("release workflow token permission", () => {
  it("accepts identical workflows after non-workflow changes on the live default branch", () => {
    const repo = fixture();
    repo.commit("README.md", "Product change\n");

    const result = repo.run();

    expect(result.status, result.stderr).toBe(0);
    expect(repo.git(repo.checkout, "rev-parse", "HEAD")).toBe(repo.releaseRef);
    expect(repo.git(repo.checkout, "status", "--porcelain")).toBe("");
  });

  it("rejects a workflow mismatch with actionable recovery before release mutation", () => {
    const repo = fixture();
    repo.commit(".github/workflows/ci.yml", "name: Changed CI\n");

    const result = repo.run();

    expect(result.status).toBe(1);
    expect(result.stderr).toContain("GITHUB_TOKEN cannot authorize workflow changes");
    expect(result.stderr).toContain("authorized release identity");
    expect(result.stderr).toContain("unpublished candidate");
    expect(result.stderr).toContain("Never move a published tag");
    expect(repo.git(repo.checkout, "rev-parse", "HEAD")).toBe(repo.releaseRef);
  });

  it("checks the latest remote state again after preflight already passed", () => {
    const repo = fixture();
    expect(repo.run().status).toBe(0);
    repo.commit(".github/workflows/ci.yml", "name: Changed during build\n");

    const result = repo.run();

    expect(result.status).toBe(1);
    expect(result.stderr).toContain("GITHUB_TOKEN cannot authorize workflow changes");
  });

  it("fails closed when default-branch lookup fails", () => {
    const repo = fixture();
    NodeFS.writeFileSync(NodePath.join(repo.bin, "gh"), "#!/bin/sh\nexit 7\n");
    const result = repo.run();
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain("Could not verify release workflow permissions");
  });

  it("fails closed when the default branch cannot be fetched", () => {
    const repo = fixture();
    repo.git(repo.checkout, "remote", "set-url", "origin", NodePath.join(repo.root, "missing"));
    const result = repo.run();
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain("Could not verify release workflow permissions");
  });

  it("rejects an unresolved release commit instead of comparing a different ref", () => {
    const repo = fixture();
    const result = repo.run("0000000000000000000000000000000000000000");
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain("Could not verify release workflow permissions");
  });
});

it("checks GITHUB_TOKEN permissions before builds and again before any release mutation", () => {
  const workflow = parseYaml(
    NodeFS.readFileSync(NodePath.join(repositoryRoot, ".github/workflows/release.yml"), "utf8"),
  ) as {
    jobs: Record<
      string,
      {
        steps: Array<{
          name?: string;
          id?: string;
          if?: string;
          run?: string;
          env?: Record<string, string>;
        }>;
      }
    >;
  };
  for (const [job, scope, boundary] of [
    ["preflight", "steps.release_meta", "Check"],
    ["release", "needs.preflight", "Require inspected stable draft"],
  ] as const) {
    const steps = workflow.jobs[job]!.steps;
    const index = steps.findIndex((step) =>
      step.run?.includes("check-release-workflow-permission.sh"),
    );
    expect(index).toBeGreaterThanOrEqual(0);
    expect(index).toBeLessThan(steps.findIndex((step) => step.name === boundary));
    expect(steps[index]?.if).toBe(`${scope}.outputs.validate_only != 'true'`);
    expect(steps[index]?.env?.GH_TOKEN).toBe("${{ secrets.GITHUB_TOKEN }}");
    expect(steps[index]?.env?.RELEASE_REF).toBe(`\${{ ${scope}.outputs.ref }}`);
  }
});
