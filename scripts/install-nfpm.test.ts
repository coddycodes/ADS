// @effect-diagnostics nodeBuiltinImport:off - This installer test uses real temporary filesystem fixtures.
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import * as NodeHttp from "node:http";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import { NfpmInstallationError, installNfpm, planNfpmInstall } from "./install-nfpm.ts";

const temporaryRoots: string[] = [];

function temporaryRoot(): string {
  const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-nfpm-"));
  temporaryRoots.push(root);
  return root;
}

afterEach(() => {
  for (const root of temporaryRoots.splice(0)) {
    NodeFS.rmSync(root, { recursive: true, force: true });
  }
});

describe("nFPM installer", () => {
  it("pins verified Linux archives for x64 and ARM64", () => {
    expect(planNfpmInstall("linux", "x64", "/repo")).toEqual(
      expect.objectContaining({
        version: "2.47.0",
        asset: "nfpm_2.47.0_Linux_x86_64.tar.gz",
        sha256: "0660ca602b2d2d2ae4781a06c692b3eeb9d437ffea05b831d76e41f4a3188783",
        executablePath: NodePath.join("/repo", "target", "tools", "nfpm", "2.47.0", "x64", "nfpm"),
      }),
    );
    expect(planNfpmInstall("linux", "arm64", "/repo")).toEqual(
      expect.objectContaining({
        version: "2.47.0",
        asset: "nfpm_2.47.0_Linux_arm64.tar.gz",
        sha256: "1c0f5f2999b9a974bfb04fdb0cc3306096de530ac5dbb25d739cc5f5219c919c",
        executablePath: NodePath.join(
          "/repo",
          "target",
          "tools",
          "nfpm",
          "2.47.0",
          "arm64",
          "nfpm",
        ),
      }),
    );
  });

  it("verifies downloaded bytes before extraction", async () => {
    const root = temporaryRoot();
    const archive = new TextEncoder().encode("verified archive");
    const sha256 = NodeCrypto.createHash("sha256").update(archive).digest("hex");
    const extract = vi.fn(async (_archivePath: string, directory: string) => {
      NodeFS.mkdirSync(directory, { recursive: true });
      NodeFS.writeFileSync(NodePath.join(directory, "nfpm"), "binary");
    });
    const plan = {
      ...planNfpmInstall("linux", "arm64", root),
      sha256,
    };

    await expect(
      installNfpm(plan, {
        download: async () => archive,
        extract,
      }),
    ).resolves.toBe(plan.executablePath);
    expect(extract).toHaveBeenCalledOnce();

    const rejectExtract = vi.fn();
    await expect(
      installNfpm(
        { ...plan, sha256: "0".repeat(64), executablePath: `${plan.executablePath}-bad` },
        { download: async () => archive, extract: rejectExtract },
      ),
    ).rejects.toBeInstanceOf(NfpmInstallationError);
    expect(rejectExtract).not.toHaveBeenCalled();
  });

  it("follows a bounded release-asset redirect before verifying bytes", async () => {
    const root = temporaryRoot();
    const archive = new TextEncoder().encode("redirected archive");
    const sha256 = NodeCrypto.createHash("sha256").update(archive).digest("hex");
    const server = NodeHttp.createServer((request, response) => {
      if (request.url === "/asset") {
        response.writeHead(302, { location: "/final" });
        response.end();
        return;
      }
      response.writeHead(200, { "content-type": "application/gzip" });
      response.end(archive);
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    try {
      const address = server.address();
      if (address === null || typeof address === "string") throw new Error("HTTP fixture address");
      const plan = {
        ...planNfpmInstall("linux", "arm64", root),
        url: `http://127.0.0.1:${address.port}/asset`,
        sha256,
      };

      await expect(
        installNfpm(plan, {
          extract: async (_archivePath, directory) => {
            NodeFS.writeFileSync(NodePath.join(directory, "nfpm"), "binary");
          },
        }),
      ).resolves.toBe(plan.executablePath);
    } finally {
      await new Promise<void>((resolve, reject) =>
        server.close((error) => (error ? reject(error) : resolve())),
      );
    }
  });
});
