// @effect-diagnostics nodeBuiltinImport:off - This Docker planner test uses temporary package paths.
import * as NodePath from "node:path";

import { describe, expect, it } from "vite-plus/test";

import {
  LINUX_SERVER_PACKAGE_SMOKE_TARGETS,
  buildLinuxPackageSmokePlan,
} from "./test-linux-server-package.ts";

describe("Linux server package smoke", () => {
  it("covers the approved DEB and RPM distributions", () => {
    expect(LINUX_SERVER_PACKAGE_SMOKE_TARGETS).toEqual([
      { format: "deb", image: "ubuntu:22.04" },
      { format: "deb", image: "ubuntu:24.04" },
      { format: "deb", image: "debian:12" },
      { format: "rpm", image: "rockylinux:9" },
      { format: "rpm", image: "fedora:44" },
    ]);
  });

  it("builds a native ARM64 install, startup, removal, and data-preservation plan", () => {
    const packagePath = NodePath.resolve("/tmp/coddy-studio-server_0.4.3_arm64.deb");
    const plan = buildLinuxPackageSmokePlan(
      { format: "deb", image: "debian:12" },
      {
        arch: "arm64",
        expectedVersion: "0.4.3",
        packagePath,
        runId: "run-17",
      },
    );

    expect(plan.command).toBe("docker");
    expect(plan.containerName).toBe("coddy-studio-server-package-run-17-debian-12-arm64");
    expect(plan.args).toEqual(
      expect.arrayContaining([
        "--rm",
        "--name",
        plan.containerName,
        "--platform",
        "linux/arm64",
        "debian:12",
      ]),
    );
    expect(plan.script).toContain('test "$(uname -m)" = "aarch64"');
    expect(plan.script).toContain("apt-get install -y /artifacts/package.deb");
    expect(plan.script).toContain("curl -fsS http://127.0.0.1:3773/");
    expect(plan.script).toContain("apt-get remove -y coddy-studio-server");
    expect(plan.script).toContain('test -f "$state_root/preserved-sentinel"');
  });

  it("keeps RPM smoke compatible with minimal Rocky and Fedora images", () => {
    const plan = buildLinuxPackageSmokePlan(
      { format: "rpm", image: "rockylinux:9" },
      {
        arch: "x64",
        expectedVersion: "0.4.3",
        packagePath: NodePath.resolve("/tmp/coddy-studio-server-0.4.3-1.x86_64.rpm"),
        runId: "run-18",
      },
    );

    expect(plan.script).toContain("dnf install -y curl-minimal ca-certificates procps-ng");
    expect(plan.script).not.toContain("dnf install -y curl ");
  });
});
