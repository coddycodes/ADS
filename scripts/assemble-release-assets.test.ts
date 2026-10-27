// @effect-diagnostics nodeBuiltinImport:off - This assembler test uses real temporary release fixtures.
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { afterEach, describe, expect, it } from "vite-plus/test";

import {
  ReleaseAssetAssemblyError,
  assembleReleaseAssets,
  expectedServerAssetNames,
  expectedSymbolAssetNames,
  serverSigningPlan,
} from "./assemble-release-assets.ts";

const temporaryRoots: string[] = [];

function temporaryRoot(): string {
  const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-release-assets-"));
  temporaryRoots.push(root);
  return root;
}

afterEach(() => {
  for (const root of temporaryRoots.splice(0)) {
    NodeFS.rmSync(root, { recursive: true, force: true });
  }
});

function writeCompleteFixture(root: string, version: string, updater: boolean): void {
  const desktop = [
    `CoddyStudio_${version}_aarch64.dmg`,
    `CoddyStudio_${version}_x64.dmg`,
    `CoddyStudio_${version}_aarch64.AppImage`,
    `CoddyStudio_${version}_amd64.AppImage`,
    `CoddyStudio_${version}_arm64-setup.exe`,
    `CoddyStudio_${version}_x64-setup.exe`,
  ];
  for (const name of [
    ...desktop,
    ...expectedServerAssetNames(version),
    ...expectedSymbolAssetNames(version),
  ]) {
    NodeFS.writeFileSync(NodePath.join(root, name), name);
  }
  if (updater) {
    NodeFS.writeFileSync(NodePath.join(root, "latest.json"), "{}\n");
    for (const target of [
      "darwin-aarch64",
      "darwin-x86_64",
      "linux-aarch64",
      "linux-x86_64",
      "windows-aarch64",
      "windows-x86_64",
    ]) {
      const artifact = target.startsWith("darwin-")
        ? `coddy-studio-update-${target}.app.tar.gz`
        : target === "linux-aarch64"
          ? `CoddyStudio_${version}_aarch64.AppImage`
          : target === "linux-x86_64"
            ? `CoddyStudio_${version}_amd64.AppImage`
            : target === "windows-aarch64"
              ? `CoddyStudio_${version}_arm64-setup.exe`
              : `CoddyStudio_${version}_x64-setup.exe`;
      NodeFS.writeFileSync(
        NodePath.join(root, `updater-${target}.json`),
        `${JSON.stringify({ target, artifact, signature: `${artifact}.sig` })}\n`,
      );
      NodeFS.writeFileSync(NodePath.join(root, artifact), artifact);
      NodeFS.writeFileSync(NodePath.join(root, `${artifact}.sig`), "signature");
    }
  }
}

describe("release asset assembly", () => {
  it("requires six archives and four Linux packages", () => {
    expect(expectedServerAssetNames("0.4.3")).toEqual([
      "coddy-studio-server-v0.4.3-darwin-aarch64.tar.gz",
      "coddy-studio-server-v0.4.3-darwin-x86_64.tar.gz",
      "coddy-studio-server-v0.4.3-linux-aarch64.tar.gz",
      "coddy-studio-server-v0.4.3-linux-x86_64.tar.gz",
      "coddy-studio-server-v0.4.3-windows-aarch64.zip",
      "coddy-studio-server-v0.4.3-windows-x86_64.zip",
      "coddy-studio-server_0.4.3_amd64.deb",
      "coddy-studio-server_0.4.3_arm64.deb",
      "coddy-studio-server-0.4.3-1.aarch64.rpm",
      "coddy-studio-server-0.4.3-1.x86_64.rpm",
    ]);
  });

  it("requires a desktop and a server symbol archive for every release target", () => {
    const names = expectedSymbolAssetNames("0.4.3");
    expect(names).toHaveLength(12);
    for (const family of ["desktop", "server"]) {
      for (const key of [
        "darwin-aarch64",
        "darwin-x86_64",
        "linux-aarch64",
        "linux-x86_64",
        "windows-aarch64",
        "windows-x86_64",
      ]) {
        expect(names).toContain(`coddy-studio-${family}-symbols-0.4.3-${key}.zip`);
      }
    }
  });

  it("writes sorted checksums and removes only internal updater descriptors", async () => {
    const root = temporaryRoot();
    writeCompleteFixture(root, "0.4.3", true);

    const result = await assembleReleaseAssets({
      assetsDir: root,
      version: "0.4.3",
      updater: true,
    });

    const checksumPath = NodePath.join(root, "coddy-studio-server-SHA256SUMS");
    const checksumLines = NodeFS.readFileSync(checksumPath, "utf8").trim().split("\n");
    expect(checksumLines).toHaveLength(10);
    const checksumNames = checksumLines.map((line) => line.slice(66));
    expect(checksumNames).toEqual(checksumNames.toSorted());
    const firstName = "coddy-studio-server-0.4.3-1.aarch64.rpm";
    const expectedHash = NodeCrypto.createHash("sha256").update(firstName).digest("hex");
    expect(checksumLines[0]).toBe(`${expectedHash}  ${firstName}`);
    expect(result.signed).toBe(false);
    expect(result.publishedAssets).toContain("coddy-studio-desktop-symbols-0.4.3-linux-x86_64.zip");
    expect(result.publishedAssets).toContain("coddy-studio-server-symbols-0.4.3-windows-aarch64.zip");
    expect(NodeFS.existsSync(NodePath.join(root, "latest.json"))).toBe(true);
    expect(NodeFS.readdirSync(root).some((name) => name.startsWith("updater-"))).toBe(false);
  });

  it("rejects missing server targets, unexpected files, and partial signing config", async () => {
    const root = temporaryRoot();
    writeCompleteFixture(root, "0.4.3", false);
    NodeFS.rmSync(NodePath.join(root, "coddy-studio-server-v0.4.3-linux-aarch64.tar.gz"));
    await expect(
      assembleReleaseAssets({ assetsDir: root, version: "0.4.3", updater: false }),
    ).rejects.toBeInstanceOf(ReleaseAssetAssemblyError);

    writeCompleteFixture(root, "0.4.3", false);
    NodeFS.writeFileSync(NodePath.join(root, "stale-debug.txt"), "unexpected");
    await expect(
      assembleReleaseAssets({ assetsDir: root, version: "0.4.3", updater: false }),
    ).rejects.toThrow("Unexpected release asset stale-debug.txt");

    NodeFS.rmSync(NodePath.join(root, "stale-debug.txt"));
    NodeFS.writeFileSync(NodePath.join(root, "stale.sig"), "unexpected signature");
    await expect(
      assembleReleaseAssets({ assetsDir: root, version: "0.4.3", updater: false }),
    ).rejects.toThrow("Unexpected release asset stale.sig");

    NodeFS.rmSync(NodePath.join(root, "stale.sig"));
    NodeFS.writeFileSync(
      NodePath.join(root, "coddy-studio-desktop-symbols-0.4.3-linux-riscv64.zip"),
      "unknown target",
    );
    await expect(
      assembleReleaseAssets({ assetsDir: root, version: "0.4.3", updater: false }),
    ).rejects.toThrow("Unexpected release asset coddy-studio-desktop-symbols-0.4.3-linux-riscv64.zip");

    NodeFS.rmSync(NodePath.join(root, "coddy-studio-desktop-symbols-0.4.3-linux-riscv64.zip"));
    NodeFS.rmSync(NodePath.join(root, "coddy-studio-server-symbols-0.4.3-darwin-x86_64.zip"));
    await expect(
      assembleReleaseAssets({ assetsDir: root, version: "0.4.3", updater: false }),
    ).rejects.toThrow(
      "Missing symbol release asset coddy-studio-server-symbols-0.4.3-darwin-x86_64.zip",
    );

    expect(() => serverSigningPlan({ privateKey: "/tmp/private", publicKey: undefined })).toThrow(
      "both private and public keys",
    );
  });
});
