// @effect-diagnostics nodeBuiltinImport:off - This release-script test uses real temporary filesystem fixtures.
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { afterEach, describe, expect, it } from "vite-plus/test";

import {
  ServerArtifactConfigurationError,
  parseServerArtifactArguments,
  planServerArtifact,
  stageServerDistribution,
  validateServerArchiveEntries,
  validateServerArchiveSignature,
} from "./build-server-artifact.ts";

const temporaryRoots: string[] = [];

function temporaryRoot(): string {
  const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-server-artifact-"));
  temporaryRoots.push(root);
  return root;
}

afterEach(() => {
  for (const root of temporaryRoots.splice(0)) {
    NodeFS.rmSync(root, { recursive: true, force: true });
  }
});

describe("server artifact builder", () => {
  it("loads both Windows PowerShell ZIP assemblies in dependency order", () => {
    const script = NodeFS.readFileSync(
      NodePath.resolve(import.meta.dirname, "create-portable-zip.ps1"),
      "utf8",
    );
    expect(script.indexOf("System.IO.Compression\n")).toBeGreaterThanOrEqual(0);
    expect(script.indexOf("System.IO.Compression.FileSystem")).toBeGreaterThan(
      script.indexOf("System.IO.Compression\n"),
    );
  });

  it("expands every nFPM content source from the staged package root", () => {
    const config = NodeFS.readFileSync(
      NodePath.resolve(import.meta.dirname, "../apps/server/package/nfpm.yaml"),
      "utf8",
    );
    expect(
      config.match(/- src: \$\{CODDY_STUDIO_SERVER_PACKAGE_ROOT\}[^\n]*\n\s+expand: true/g),
    ).toHaveLength(4);
  });

  it("defaults CLI version and output from the repository server package", () => {
    const root = temporaryRoot();
    NodeFS.mkdirSync(NodePath.join(root, "apps/server"), { recursive: true });
    NodeFS.writeFileSync(
      NodePath.join(root, "apps/server/package.json"),
      `${JSON.stringify({ version: "1.2.3" })}\n`,
    );

    expect(parseServerArtifactArguments(["--platform", "linux", "--arch", "arm64"], root)).toEqual({
      platform: "linux",
      arch: "arm64",
      version: "1.2.3",
      outputDir: "release/server/linux-aarch64",
    });
  });

  it("plans the Linux ARM64 archive from the shared release target", () => {
    const root = temporaryRoot();
    const plan = planServerArtifact(
      {
        platform: "linux",
        arch: "arm64",
        version: "0.4.3",
        outputDir: NodePath.join(root, "out"),
        skipBuild: true,
        binaryPath: NodePath.join(root, "coddy-studio"),
        webDir: NodePath.join(root, "web"),
      },
      { platform: "linux", arch: "arm64" },
      root,
    );

    expect(plan.target.rustTarget).toBe("aarch64-unknown-linux-gnu");
    expect(plan.archiveName).toBe("coddy-studio-server-v0.4.3-linux-aarch64.tar.gz");
    expect(plan.distributionRootName).toBe("coddy-studio-server-v0.4.3-linux-aarch64");
    expect(plan.archiveCommand).toEqual({
      command: "tar",
      args: [
        "-czf",
        "coddy-studio-server-v0.4.3-linux-aarch64.tar.gz",
        "-C",
        "staging",
        "coddy-studio-server-v0.4.3-linux-aarch64",
      ],
    });
    expect(plan.packageArtifacts).toEqual([
      NodePath.join(root, "out", "coddy-studio-server_0.4.3_arm64.deb"),
      NodePath.join(root, "out", "coddy-studio-server-0.4.3-1.aarch64.rpm"),
    ]);
  });

  it("uses native Windows ZIP creation with relative artifact paths", () => {
    const root = temporaryRoot();
    const plan = planServerArtifact(
      {
        platform: "win",
        arch: "x64",
        version: "0.4.3",
        outputDir: NodePath.join(root, "out"),
        skipBuild: true,
        binaryPath: NodePath.join(root, "coddy-studio.exe"),
        webDir: NodePath.join(root, "web"),
      },
      { platform: "win32", arch: "x64" },
      root,
    );

    expect(plan.archiveCommand).toEqual({
      command: "powershell.exe",
      args: [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-File",
        NodePath.join(root, "scripts/create-portable-zip.ps1"),
        "-Source",
        "staging\\coddy-studio-server-v0.4.3-windows-x86_64",
        "-Destination",
        "coddy-studio-server-v0.4.3-windows-x86_64.zip",
      ],
    });
    expect(plan.archiveListCommand.command).toBe("powershell.exe");
    expect(plan.archiveListCommand.env).toEqual({
      CODDY_STUDIO_SERVER_ARCHIVE_DESTINATION: "coddy-studio-server-v0.4.3-windows-x86_64.zip",
    });
  });

  it("rejects tar bytes renamed with a zip extension", () => {
    const root = temporaryRoot();
    const archive = NodePath.join(root, "server.zip");
    NodeFS.writeFileSync(archive, "ustar");
    expect(() => validateServerArchiveSignature(archive, "zip")).toThrow("ZIP signature");

    NodeFS.writeFileSync(archive, Buffer.from([0x50, 0x4b, 0x03, 0x04]));
    expect(() => validateServerArchiveSignature(archive, "zip")).not.toThrow();
  });

  it("rejects non-portable Windows separators inside ZIP entries", () => {
    const root = "coddy-studio-server-v0.4.3-windows-x86_64";
    expect(() => validateServerArchiveEntries([`${root}\\coddy-studio.exe`], root)).toThrow(
      "Unsafe archive entry",
    );
    expect(() =>
      validateServerArchiveEntries([`${root}/coddy-studio.exe`, `${root}/web/index.html`], root),
    ).not.toThrow();
  });

  it("stages the executable, web client, guide, and license under one versioned root", async () => {
    const root = temporaryRoot();
    const binary = NodePath.join(root, "coddy-studio");
    const web = NodePath.join(root, "web");
    const guide = NodePath.join(root, "server-installation.md");
    const license = NodePath.join(root, "LICENSE");
    NodeFS.mkdirSync(web);
    NodeFS.writeFileSync(binary, "binary");
    NodeFS.writeFileSync(NodePath.join(web, "index.html"), "<main>Coddy Studio</main>");
    NodeFS.writeFileSync(NodePath.join(web, "app.js"), "console.log('Coddy Studio')");
    NodeFS.writeFileSync(guide, "# Server installation\n");
    NodeFS.writeFileSync(license, "MIT\n");
    const plan = planServerArtifact(
      {
        platform: "linux",
        arch: "arm64",
        version: "0.4.3",
        outputDir: NodePath.join(root, "out"),
        skipBuild: true,
        binaryPath: binary,
        webDir: web,
      },
      { platform: "linux", arch: "arm64" },
      root,
      { guidePath: guide, licensePath: license },
    );

    const staged = await stageServerDistribution(plan);

    expect(staged).toEqual(["LICENSE", "README.md", "coddy-studio", "web/app.js", "web/index.html"]);
    expect(NodeFS.readFileSync(NodePath.join(plan.stagingDir, "README.md"), "utf8")).toBe(
      "# Server installation\n",
    );
  });

  it("rejects incomplete web assets and overlapping source/output trees", async () => {
    const root = temporaryRoot();
    const binary = NodePath.join(root, "coddy-studio");
    const web = NodePath.join(root, "web");
    const output = NodePath.join(root, "out");
    NodeFS.mkdirSync(web);
    NodeFS.writeFileSync(binary, "binary");
    const base = {
      platform: "linux" as const,
      arch: "arm64" as const,
      version: "0.4.3",
      outputDir: output,
      skipBuild: true,
      binaryPath: binary,
      webDir: web,
    };
    const plan = planServerArtifact(base, { platform: "linux", arch: "arm64" }, root, {
      guidePath: binary,
      licensePath: binary,
    });

    await expect(stageServerDistribution(plan)).rejects.toThrow("web/index.html");
    expect(() =>
      planServerArtifact(
        { ...base, binaryPath: NodePath.join(output, "coddy-studio") },
        { platform: "linux", arch: "arm64" },
        root,
      ),
    ).toThrow(ServerArtifactConfigurationError);
  });
});
