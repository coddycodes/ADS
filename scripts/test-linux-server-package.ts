#!/usr/bin/env node
// @effect-diagnostics nodeBuiltinImport:off - This standalone validation adapter owns Docker process execution.
// @effect-diagnostics globalConsole:off - The CLI reports bounded container failures.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import * as NodeUtil from "node:util";

import type { ReleaseArch } from "./lib/release-targets.ts";

export interface LinuxServerPackageSmokeTarget {
  readonly format: "deb" | "rpm";
  readonly image: string;
}

export const LINUX_SERVER_PACKAGE_SMOKE_TARGETS = [
  { format: "deb", image: "ubuntu:22.04" },
  { format: "deb", image: "ubuntu:24.04" },
  { format: "deb", image: "debian:12" },
  { format: "rpm", image: "rockylinux:9" },
  { format: "rpm", image: "fedora:44" },
] as const satisfies ReadonlyArray<LinuxServerPackageSmokeTarget>;

export interface LinuxServerPackageSmokeInput {
  readonly arch: ReleaseArch;
  readonly expectedVersion: string;
  readonly packagePath: string;
  readonly runId: string;
}

export interface LinuxServerPackageSmokePlan {
  readonly command: "docker";
  readonly args: ReadonlyArray<string>;
  readonly containerName: string;
  readonly script: string;
}

export class LinuxServerPackageSmokeError extends Error {
  override readonly name = "LinuxServerPackageSmokeError";
}

function safeName(value: string): string {
  const name = value
    .toLowerCase()
    .replace(/[^a-z0-9_.-]+/g, "-")
    .replace(/^-+|-+$/g, "");
  if (name.length === 0) throw new LinuxServerPackageSmokeError("Container name is empty.");
  return name;
}

function packageScript(
  target: LinuxServerPackageSmokeTarget,
  expectedArch: string,
  expectedVersion: string,
): string {
  const install =
    target.format === "deb"
      ? "apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y curl ca-certificates procps && apt-get install -y /artifacts/package.deb"
      : "dnf install -y curl-minimal ca-certificates procps-ng /artifacts/package.rpm";
  const inspect =
    target.format === "deb"
      ? "dpkg-deb --field /artifacts/package.deb Package Version Architecture"
      : "rpm -qp --queryformat '%{NAME} %{VERSION}-%{RELEASE} %{ARCH}\\n' /artifacts/package.rpm";
  const remove =
    target.format === "deb" ? "apt-get remove -y coddy-studio-server" : "dnf remove -y coddy-studio-server";
  return [
    "set -euo pipefail",
    `test "$(uname -m)" = "${expectedArch}"`,
    inspect,
    install,
    `coddy-studio --version | grep -F "${expectedVersion}"`,
    'state_root="$(mktemp -d /tmp/coddy-studio-package-smoke.XXXXXX)"',
    'printf preserved > "$state_root/preserved-sentinel"',
    'coddy-studio serve --host 127.0.0.1 --port 3773 --base-dir "$state_root" > /tmp/coddy-studio-ready.json 2> /tmp/coddy-studio-server.log &',
    'server_pid="$!"',
    'cleanup() { if kill -0 "$server_pid" 2>/dev/null; then kill -TERM "$server_pid"; wait "$server_pid" || true; fi; }',
    "trap cleanup EXIT",
    "ready=0",
    "for attempt in $(seq 1 60); do if curl -fsS http://127.0.0.1:3773/ | grep -F Coddy Studio >/dev/null; then ready=1; break; fi; sleep 0.5; done",
    'test "$ready" = "1"',
    'kill -TERM "$server_pid"',
    'wait "$server_pid"',
    "trap - EXIT",
    remove,
    "test ! -e /usr/bin/coddy-studio",
    'test -f "$state_root/preserved-sentinel"',
  ].join("\n");
}

export function buildLinuxPackageSmokePlan(
  target: LinuxServerPackageSmokeTarget,
  input: LinuxServerPackageSmokeInput,
): LinuxServerPackageSmokePlan {
  const packagePath = NodePath.resolve(input.packagePath);
  const imageName = target.image.replace(/[:/]+/g, "-");
  const containerName = safeName(
    `coddy-studio-server-package-${input.runId}-${imageName}-${input.arch}`,
  );
  const dockerArch = input.arch === "arm64" ? "arm64" : "amd64";
  const unameArch = input.arch === "arm64" ? "aarch64" : "x86_64";
  const extension = target.format;
  const script = packageScript(target, unameArch, input.expectedVersion);
  return {
    command: "docker",
    containerName,
    script,
    args: [
      "run",
      "--rm",
      "--name",
      containerName,
      "--platform",
      `linux/${dockerArch}`,
      "--mount",
      `type=bind,source=${packagePath},target=/artifacts/package.${extension},readonly`,
      target.image,
      "bash",
      "-lc",
      script,
    ],
  };
}

export function runLinuxPackageSmokePlan(plan: LinuxServerPackageSmokePlan): void {
  const result = NodeChildProcess.spawnSync(plan.command, [...plan.args], {
    shell: false,
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    NodeChildProcess.spawnSync("docker", ["rm", "-f", plan.containerName], {
      shell: false,
      stdio: "ignore",
    });
    throw new LinuxServerPackageSmokeError(
      `${plan.containerName} exited with ${result.status ?? 1}.`,
    );
  }
}

function parseArguments(argv: ReadonlyArray<string>): LinuxServerPackageSmokeInput {
  const { values } = NodeUtil.parseArgs({
    args: [...argv],
    allowPositionals: false,
    strict: true,
    options: {
      arch: { type: "string" },
      "expected-version": { type: "string" },
      package: { type: "string" },
      "run-id": { type: "string" },
    },
  });
  if (values.arch !== "arm64" && values.arch !== "x64") {
    throw new LinuxServerPackageSmokeError("--arch must be arm64 or x64.");
  }
  if (
    typeof values["expected-version"] !== "string" ||
    typeof values.package !== "string" ||
    typeof values["run-id"] !== "string"
  ) {
    throw new LinuxServerPackageSmokeError(
      "--expected-version, --package, and --run-id are required.",
    );
  }
  return {
    arch: values.arch,
    expectedVersion: values["expected-version"],
    packagePath: values.package,
    runId: values["run-id"],
  };
}

if (
  process.argv[1] !== undefined &&
  import.meta.url === NodeURL.pathToFileURL(process.argv[1]).href
) {
  try {
    const input = parseArguments(process.argv.slice(2));
    if (!NodeFS.existsSync(input.packagePath)) {
      throw new LinuxServerPackageSmokeError(`Package is missing: ${input.packagePath}`);
    }
    const extension = NodePath.extname(input.packagePath).slice(1);
    const targets = LINUX_SERVER_PACKAGE_SMOKE_TARGETS.filter(
      (target) => target.format === extension,
    );
    if (targets.length === 0) {
      throw new LinuxServerPackageSmokeError("Package must end in .deb or .rpm.");
    }
    for (const target of targets)
      runLinuxPackageSmokePlan(buildLinuxPackageSmokePlan(target, input));
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
