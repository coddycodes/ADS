// @effect-diagnostics nodeBuiltinImport:off - The packaged-upgrade harness owns host paths.
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeChildProcess from "node:child_process";
import * as NodeVM from "node:vm";
import * as NodeURL from "node:url";
import * as NodeModule from "node:module";

import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import * as Schema from "effect/Schema";
import * as Effect from "effect/Effect";
import { it as effectIt } from "@effect/vitest";
import { HostProcessPlatform } from "@coddy/shared/hostProcess";
import { AuthPairingLink } from "@coddy/contracts";
import { parse as parseToml, type TomlTable } from "smol-toml";
import {
  releasePackageFiles,
  releaseRustPackageFiles,
  releaseVersionFiles,
} from "./update-release-package-versions.ts";

import {
  assertBaselineVersionIsOlder,
  assertSeededUpgradeBuildVersion,
  assertWebDriverPhaseExit,
  remoteEvidenceReadFailure,
  windowsCandidateIsInstalled,
  productVersionFromExecutableBytes,
  waitForWindowsRelaunchedApplication,
  windowedApplicationRunning,
  windowsInstallerProcesses,
  observeWindowsInstalledCandidate,
  waitForWindowsInstalledCandidate,
  copyBoundedEvidence,
  readWindowsCandidateProbe,
  ownedChildTreeKillCommand,
  reportSeededUpgradeSmokeFailure,
  terminateSeededUpgradeChild,
  SeededUpgradeCommandTimeoutError,
  buildLocalUpdaterManifest,
  buildSeededUpgradeOverlay,
  canonicalizeSeededUpgradeWorkRoot,
  createSeededUpgradeDriverSpec,
  createSeededUpgradeRunLayout,
  createSeededUpgradeWdioConfig,
  ManagedProcessRegistry,
  parseSeededDesktopUpgradeSmokeArgs,
  prepareSeededUpgradeBuild,
  redactAndBoundUpgradeEvidence,
  writeRemoteUploadCapabilityReceipt,
  readRemoteFixtureSecrets,
  stopRemoteUploadFixture,
  removeSeededUpgradeDependencyTree,
  runBoundedCommand,
  seededUpgradeVitePlusExecutable,
  seededUpgradeBundleRoot,
  seededUpgradeRustTarget,
  updaterTargetFor,
  verifySeededUpgradeOutcome,
  waitForUpgradeCondition,
  restartedApplicationCleanupPlan,
  assertRemoteInstallPort,
  countAppImageMounts,
  scopedCleanupPids,
  verifyRemoteInstallOutcome,
  SeededDesktopUpgradeSmokeError,
  type RemoteInstallEvidence,
  runSeededDesktopUpgradeSmoke,
  type SeededDesktopUpgradeSmokeInput,
} from "./seeded-desktop-upgrade-smoke.ts";

const absolute = (...parts: ReadonlyArray<string>): string =>
  NodePath.resolve("/tmp/coddy-studio-upgrade-smoke", ...parts);

const decodePairingLink = Schema.decodeUnknownSync(Schema.toCodecJson(AuthPairingLink));
const repositoryRoot = NodeURL.fileURLToPath(new URL("..", import.meta.url));

describe("private held upload receipt and cleanup", () => {
  const directory = "coddy-studio-update-witness-00000000-0000-4000-8000-000000000001";
  const receipt = {
    version: 1 as const,
    relativeDirectory: directory,
    relativeUrl: "/api/transfers/private-capability",
  };
  async function owned(
    run: (root: string, workspace: string, upload: string, credentials: string) => Promise<void>,
  ) {
    const root = await NodeFS.promises.mkdtemp(
      NodePath.join(NodeOS.tmpdir(), "held-upload-receipt-"),
    );
    const workspace = NodePath.join(root, "workspace");
    await NodeFS.promises.mkdir(workspace);
    const credentials = NodePath.join(root, "credentials.json");
    await NodeFS.promises.writeFile(
      credentials,
      JSON.stringify({ bootstrapToken: "private-grant" }),
      { mode: 0o600 },
    );
    try {
      await run(root, workspace, NodePath.join(root, "upload.json"), credentials);
    } finally {
      await NodeFS.promises.rm(root, { recursive: true, force: true });
    }
  }
  effectIt.effect(
    "persists an immutable private capability receipt and reads all literal secrets",
    () =>
      Effect.gen(function* () {
        const platform = yield* HostProcessPlatform;
        yield* Effect.promise(() =>
          owned(async (_root, _workspace, path, credentials) => {
            await writeRemoteUploadCapabilityReceipt(path, receipt);
            const bytes = await NodeFS.promises.readFile(path, "utf8");
            expect(JSON.parse(bytes)).toEqual(receipt);
            if (platform !== "win32")
              expect((await NodeFS.promises.stat(path)).mode & 0o777).toBe(0o600);
            await expect(
              writeRemoteUploadCapabilityReceipt(path, {
                ...receipt,
                relativeUrl: "/api/transfers/replacement",
              }),
            ).rejects.toThrow("private upload receipt");
            expect(await NodeFS.promises.readFile(path, "utf8")).toBe(bytes);
            expect(
              await readRemoteFixtureSecrets({
                credentialReceiptPath: credentials,
                uploadReceiptPath: path,
                requireUploadReceipt: true,
              }),
            ).toEqual(
              expect.arrayContaining(["private-grant", receipt.relativeUrl, "private-capability"]),
            );
          }),
        );
      }),
  );
  it.each(["malformed", "unexpected-fields", "traversal", "missing", "oversized"])(
    "refuses %s receipts with closed errors",
    async (kind) => {
      await owned(async (_root, _workspace, path, credentials) => {
        const text =
          kind === "malformed"
            ? '{"private-capability":'
            : kind === "unexpected-fields"
              ? JSON.stringify({ ...receipt, privateUnknown: "private-capability" })
              : kind === "traversal"
                ? JSON.stringify({ ...receipt, relativeDirectory: "../outside" })
                : kind === "oversized"
                  ? "private-capability".repeat(1000)
                  : null;
        if (text !== null) await NodeFS.promises.writeFile(path, text);
        const error = await readRemoteFixtureSecrets({
          credentialReceiptPath: credentials,
          uploadReceiptPath: path,
          requireUploadReceipt: true,
        }).catch((error: unknown) => error);
        expect(error).toBeInstanceOf(SeededDesktopUpgradeSmokeError);
        expect(String(error)).not.toContain("private-capability");
      });
    },
  );
  it.each([
    ["credentials", false],
    ["credentials", true],
    ["upload", false],
    ["upload", true],
  ] as const)(
    "rejects present null %s even when required upload is %s",
    async (kind, requireUploadReceipt) => {
      await owned(async (_root, _workspace, path, credentials) => {
        await NodeFS.promises.writeFile(kind === "credentials" ? credentials : path, "null");
        const error = await readRemoteFixtureSecrets({
          credentialReceiptPath: credentials,
          uploadReceiptPath: path,
          requireUploadReceipt,
        }).catch((error: unknown) => error);
        expect(error).toBeInstanceOf(SeededDesktopUpgradeSmokeError);
        expect(String(error)).not.toContain("private-grant");
      });
    },
  );
  it("requires bootstrap receipt once the remote phase could have returned credentials", async () => {
    await owned(async (_root, _workspace, path, credentials) => {
      await NodeFS.promises.rm(credentials);
      const input = {
        credentialReceiptPath: credentials,
        uploadReceiptPath: path,
        requireUploadReceipt: false,
        requireCredentialReceipt: true,
      };
      await expect(readRemoteFixtureSecrets(input)).rejects.toThrow(
        "private remote credential receipt",
      );
    });
  });
  it("allows absent receipts before any remote phase could start", async () => {
    await owned(async (_root, _workspace, path, credentials) => {
      await NodeFS.promises.rm(credentials);
      expect(
        await readRemoteFixtureSecrets({
          credentialReceiptPath: credentials,
          uploadReceiptPath: path,
          requireUploadReceipt: false,
        }),
      ).toEqual([]);
    });
  });
  it("waits for actual process-stop settlement before removing only owned upload residue", async () => {
    await owned(async (_root, workspace, path) => {
      await writeRemoteUploadCapabilityReceipt(path, receipt);
      const target = NodePath.join(workspace, directory);
      await NodeFS.promises.mkdir(target);
      await NodeFS.promises.writeFile(NodePath.join(target, "protection-witness.txt"), "o");
      await NodeFS.promises.writeFile(
        NodePath.join(target, ".protection-witness.txt.1-2-3.coddy-studio-upload.part"),
        "o",
      );
      const gate = Promise.withResolvers<void>();
      const stopping = stopRemoteUploadFixture({
        workspaceRoot: workspace,
        uploadReceiptPath: path,
        stop: () => gate.promise,
      });
      expect(NodeFS.existsSync(target)).toBe(true);
      gate.resolve();
      await stopping;
      expect(NodeFS.existsSync(target)).toBe(false);
      expect(NodeFS.existsSync(workspace)).toBe(true);
    });
  });
  it("refuses malformed bootstrap JSON without exposing the parse input", async () => {
    await owned(async (_root, _workspace, path, credentials) => {
      await writeRemoteUploadCapabilityReceipt(path, receipt);
      await NodeFS.promises.writeFile(credentials, '{"private-grant":');
      const error = await readRemoteFixtureSecrets({
        credentialReceiptPath: credentials,
        uploadReceiptPath: path,
      }).catch((error: unknown) => error);
      expect(error).toBeInstanceOf(SeededDesktopUpgradeSmokeError);
      expect(String(error)).not.toContain("private-grant");
    });
  });
  it("preserves residue when process cleanup fails or the directory contains unknown work", async () => {
    await owned(async (_root, workspace, path) => {
      await writeRemoteUploadCapabilityReceipt(path, receipt);
      const target = NodePath.join(workspace, directory);
      await NodeFS.promises.mkdir(target);
      await NodeFS.promises.writeFile(NodePath.join(target, "unexpected.txt"), "preserve");
      await expect(
        stopRemoteUploadFixture({
          workspaceRoot: workspace,
          uploadReceiptPath: path,
          stop: async () => {
            throw new Error("stop refused");
          },
        }),
      ).rejects.toThrow();
      expect(NodeFS.existsSync(target)).toBe(true);
      await expect(
        stopRemoteUploadFixture({
          workspaceRoot: workspace,
          uploadReceiptPath: path,
          stop: async () => {},
        }),
      ).rejects.toThrow();
      expect(await NodeFS.promises.readFile(NodePath.join(target, "unexpected.txt"), "utf8")).toBe(
        "preserve",
      );
    });
  });
});

/** Execute the real retention/dispatch bodies with inert native ports and owned temporary files. */
const remoteRetentionFixture = async () => {
  const root = await NodeFS.promises.mkdtemp(NodePath.join(NodeOS.tmpdir(), "remote-retention-"));
  const runRoot = NodePath.join(root, "remote");
  const evidenceDirectory = NodePath.join(runRoot, "evidence");
  await NodeFS.promises.mkdir(evidenceDirectory, { recursive: true });
  const source = await NodeFS.promises.readFile(
    new URL("./seeded-desktop-upgrade-smoke.ts", import.meta.url),
    "utf8",
  );
  const phaseStart = source.indexOf("const runWebDriverPhase = async");
  const phaseEnd = source.indexOf("\nconst startMockUpdateServer", phaseStart);
  const laneStart = source.indexOf("const runUpgradeLane = async");
  const laneEnd = source.indexOf("\nconst copyBoundedEvidence", laneStart);
  const ownerStart = source.indexOf(
    "      await runUpgradeLane({\n        appBinaryPath: remoteApp,",
  );
  const ownerEnd = source.indexOf("\n      });", ownerStart) + "\n      });".length;
  const finallyStart = source.indexOf("    const secrets = [signingKey, signingPassword];");
  const finallyEnd = source.indexOf("\n  }\n  if (failure !== undefined)", finallyStart);
  if (
    [phaseStart, phaseEnd, laneStart, laneEnd, ownerStart, finallyStart, finallyEnd].some(
      (index) => index < 0,
    )
  )
    throw new Error("Remote evidence owner source is unavailable.");
  const phaseInput = {
    appBinaryPath: NodePath.join(root, "owned-app"),
    backendPort: 14953,
    candidateVersion: "0.7.3",
    dataRoot: NodePath.join(runRoot, "data"),
    evidenceDirectory,
    expectedDataRoot: NodePath.join(runRoot, "data"),
    lane: "remote-install",
    phase: "seed-and-install",
    platform: "linux",
    projectId: "remote-fixture",
    repositoryRoot,
    restartTimeoutMs: 30_000,
    resultPath: NodePath.join(runRoot, "before.json"),
    runRoot,
    workspaceRoot: NodePath.join(runRoot, "workspace"),
    webdriverPort: 15053,
    wsl: false,
  };
  const runCommand = vi.fn(async () => ({
    exitCode: 1,
    stdout: "fixture-private-returned-token",
    stderr: "private receipt write failed",
  }));
  const copyBoundedEvidence = vi.fn(
    async (_input: { readonly withholdLanes?: ReadonlyArray<string> }): Promise<void> => {},
  );
  const cleanup = vi.fn(async () => {});
  const context = NodeVM.createContext({
    NodeFS,
    NodePath,
    NodeURL,
    process: { env: {} },
    REMOTE_UPDATE_DOWNLOAD_BUDGET_MS: 1,
    REMOTE_UPDATE_INSTALL_BUDGET_MS: 1,
    REMOTE_UPDATE_RESTART_BUDGET_MS: 1,
    seededUpgradeVitePlusExecutable,
    createSeededUpgradeDriverSpec,
    createSeededUpgradeWdioConfig,
    readRemoteFixtureSecrets,
    redactAndBoundUpgradeEvidence,
    isMissingCredentialReceipt: (error: unknown) =>
      error instanceof SeededDesktopUpgradeSmokeError &&
      error.message === "The private remote credential receipt is unavailable.",
    assertWebDriverPhaseExit,
    assertRemoteInstallPort,
    SeededDesktopUpgradeSmokeError,
    runCommand,
    remotePhaseStarted: false,
    signingKey: "private-signing-key",
    signingPassword: "private-signing-password",
    failure: undefined,
    remoteApp: phaseInput.appBinaryPath,
    runId: "fixture",
    input: {
      ...phaseInput,
      updaterPort: 14950,
      artifactDirectory: NodePath.join(root, "retained"),
    },
    layout: { remoteInstall: phaseInput },
    requestLogPath: NodePath.join(root, "requests.jsonl"),
    readObservation: async (path: string) =>
      JSON.parse(await NodeFS.promises.readFile(path, "utf8")),
    copyBoundedEvidence,
    cleanup: { cleanup },
  });
  const evaluate = (body: string): Promise<unknown> =>
    NodeVM.runInContext(NodeModule.stripTypeScriptTypes(`(async () => { ${body} })()`), context, {
      timeout: 1_000,
    }) as Promise<unknown>;
  const runPhase = async (prepareFailure = false) => {
    if (prepareFailure) {
      await NodeFS.promises.writeFile(NodePath.join(runRoot, "seed-and-install-driver"), "owned");
    }
    return evaluate(
      source.slice(phaseStart, phaseEnd) +
        "\n" +
        source.slice(laneStart, laneEnd) +
        "\n" +
        source.slice(ownerStart, ownerEnd),
    );
  };
  return {
    root,
    runRoot,
    evidenceDirectory,
    runCommand,
    copyBoundedEvidence,
    cleanup,
    runPhase,
    finalize: () => evaluate(source.slice(finallyStart, finallyEnd) + "\n return failure;"),
    dispose: () => NodeFS.promises.rm(root, { recursive: true, force: true }),
  };
};

describe("remote phase receipt retention boundaries", () => {
  it("refuses phase logs after credentials return but receipt publication fails", async () => {
    const fixture = await remoteRetentionFixture();
    try {
      const error = await fixture.runPhase().catch((error: unknown) => error);
      const log = await NodeFS.promises.readFile(
        NodePath.join(fixture.evidenceDirectory, "seed-and-install.log"),
        "utf8",
      );
      expect(log).toContain("private credential receipt was not published");
      expect(log).not.toContain("fixture-private-returned-token");
      expect(error).toBeInstanceOf(SeededDesktopUpgradeSmokeError);
      expect(String(error)).not.toContain("fixture-private-returned-token");
    } finally {
      await fixture.dispose();
    }
  });
  it.each(["returned", "thrown"])(
    "refuses final copies after a %s remote phase failure without a bootstrap receipt",
    async (kind) => {
      const fixture = await remoteRetentionFixture();
      try {
        if (kind === "thrown")
          fixture.runCommand.mockRejectedValue(new Error("fixture launch failure"));
        await fixture.runPhase().catch(() => undefined);
        const finalError = await fixture.finalize();
        expect(fixture.copyBoundedEvidence).toHaveBeenCalledOnce();
        expect(fixture.copyBoundedEvidence.mock.calls[0]?.[0]?.withholdLanes).toEqual([
          "remote-install",
        ]);
        const marker = await NodeFS.promises.readFile(
          NodePath.join(fixture.root, "retained", "remote-install-phase-withheld.txt"),
          "utf8",
        );
        expect(marker).toContain("private credential receipt was not published");
        expect(marker).not.toContain("fixture-private-returned-token");
        expect(finalError).toBeInstanceOf(SeededDesktopUpgradeSmokeError);
        expect(String(finalError)).not.toContain("fixture-private-returned-token");
        expect(fixture.cleanup).toHaveBeenCalledOnce();
      } finally {
        await fixture.dispose();
      }
    },
  );
  it("keeps earlier evidence when local phase preparation fails before any launch attempt", async () => {
    const fixture = await remoteRetentionFixture();
    try {
      await expect(fixture.runPhase(true)).rejects.toThrow();
      expect(fixture.runCommand).not.toHaveBeenCalled();
      expect(await fixture.finalize()).toBeUndefined();
      expect(fixture.copyBoundedEvidence).toHaveBeenCalledOnce();
      expect(fixture.cleanup).toHaveBeenCalledOnce();
    } finally {
      await fixture.dispose();
    }
  });
  it("retains redacted failure evidence with a valid bootstrap receipt before install dispatch", async () => {
    const fixture = await remoteRetentionFixture();
    try {
      await NodeFS.promises.writeFile(
        NodePath.join(fixture.runRoot, "remote-bootstrap.secret.json"),
        JSON.stringify({ bootstrapToken: "fixture-private-returned-token" }),
      );
      await expect(fixture.runPhase()).rejects.toThrow();
      const log = await NodeFS.promises.readFile(
        NodePath.join(fixture.evidenceDirectory, "seed-and-install.log"),
        "utf8",
      );
      expect(log).toContain("[REDACTED]");
      expect(log).not.toContain("fixture-private-returned-token");
      expect(await fixture.finalize()).toBeUndefined();
      expect(fixture.copyBoundedEvidence).toHaveBeenCalledOnce();
      expect(fixture.cleanup).toHaveBeenCalledOnce();
    } finally {
      await fixture.dispose();
    }
  });
});

const versionFixture = async () => {
  const root = await NodeFS.promises.mkdtemp(NodePath.join(NodeOS.tmpdir(), "seeded-versions-"));
  const originals = new Map(
    await Promise.all(
      releaseVersionFiles.map(
        async (path) =>
          [
            path,
            await NodeFS.promises.readFile(NodePath.join(repositoryRoot, path), "utf8"),
          ] as const,
      ),
    ),
  );
  const checkout = async (name: string, version: string) => {
    const path = NodePath.join(root, name);
    for (const [relativePath, content] of originals) {
      const destination = NodePath.join(path, relativePath);
      await NodeFS.promises.mkdir(NodePath.dirname(destination), { recursive: true });
      await NodeFS.promises.writeFile(destination, content);
    }
    // A prior tag's helper must not own current harness preparation.
    await NodeFS.promises.mkdir(NodePath.join(path, "scripts"));
    await NodeFS.promises.writeFile(
      NodePath.join(path, "scripts/update-release-package-versions.ts"),
      'throw new Error("Do not run the old checkout helper");\n',
    );
    const overlayPath = NodePath.join(root, `${name}-overlay.json`);
    await NodeFS.promises.writeFile(overlayPath, JSON.stringify({ version }));
    return { checkout: path, repositoryRoot, overlayPath, version };
  };
  return { root, originals, checkout };
};
const publicPairingGrant = {
  id: "fixture-grant",
  credential: "fixture-distinct-grant",
  scopes: ["orchestration:read", "orchestration:operate"],
  subject: "one-time-token",
  createdAt: "2026-10-02T08:00:00Z",
  expiresAt: "2026-10-02T08:05:00Z",
  reach: "another-device",
};

/** Executes the generated credential callback and its receipt/driver handoff without a desktop. */
const remoteGrantFixture = (responses: ReadonlyArray<unknown>) => {
  const input = {
    candidateVersion: "0.7.3",
    expectedDataRoot: absolute("remote", "data"),
    lane: "remote-install" as const,
    phase: "seed-and-install" as const,
    projectId: "remote-project",
    resultPath: absolute("remote", "before.json"),
    workspaceRoot: absolute("remote", "workspace"),
    platform: "linux" as const,
    appBinaryPath: absolute("RemoteLane.AppImage"),
    remoteInstallDriverPath: "fixture:driver",
    remoteHarnessPath: "fixture:host",
    remoteSecretPath: absolute("remote", "private.json"),
    remoteUploadSecretPath: absolute("remote", "upload-private.json"),
    remoteEvidencePath: absolute("remote", "evidence", "remote-rpc.json"),
  };
  const spec = createSeededUpgradeDriverSpec(input);
  const start = spec.indexOf("    let credentials;");
  const end = spec.indexOf("\n  });\n});", start);
  if (start < 0 || end < 0) throw new Error("Generated remote credential scenario is missing.");
  // Replace module loading only; run the actual browser callback and handoff statements.
  const source = spec.slice(start, end).replaceAll("await import(", "await loadFixture(");
  const files = new Map<string, { contents: string; mode?: number }>();
  let responseIndex = 0;
  const fetch = vi.fn(async (_url: URL, _options: { signal?: AbortSignal }) => ({
    ok: true,
    json: async () => responses[Math.min(responseIndex++, responses.length - 1)],
  }));
  const driver = vi.fn(async (_credentials: unknown) => ({ phases: ["succeeded"] }));
  const execute = async (callback: (...args: unknown[]) => unknown, ...args: unknown[]) =>
    callback(...args);
  const context = {
    input,
    widened: true,
    traceRemote: () => {},
    observation: { projectId: "remote-project" },
    browser: { execute },
    window: {
      desktopBridge: {
        getLocalEnvironmentBootstraps: () => [
          {
            id: "primary",
            httpBaseUrl: "http://127.0.0.1:43123",
            bootstrapToken: "fixture-desktop-bootstrap",
          },
        ],
        getLocalEnvironmentBearerToken: async () => "fixture-desktop-bearer",
      },
    },
    NodeFS: {
      writeFileSync: (path: string, contents: string, options?: { mode: number }) =>
        files.set(path, { contents, ...options }),
    },
    loadFixture: async (path: string) => {
      if (path === input.remoteInstallDriverPath) return { runRemoteInstallDriver: driver };
      if (path === input.remoteHarnessPath)
        return { captureRemoteInstallHostEvidence: async () => ({ requesterLogLines: 1 }) };
      throw new Error("Unexpected fixture module.");
    },
    fetch,
    URL,
    Date,
    AbortController,
    setTimeout,
    clearTimeout,
  };
  return {
    input,
    context,
    files,
    fetch,
    driver,
    run: () =>
      NodeVM.runInNewContext(`(async () => { ${source} })()`, context, {
        timeout: 1_000,
      }) as Promise<void>,
  };
};

describe("generated remote sharing grant handoff", () => {
  afterEach(() => vi.useRealTimers());

  it("redeems the published grant shape and retains credentials only in the private receipt", async () => {
    decodePairingLink(publicPairingGrant);
    const fixture = remoteGrantFixture([[publicPairingGrant]]);
    await fixture.run();
    expect(fixture.driver).toHaveBeenCalledWith({
      endpoint: "http://127.0.0.1:43123",
      bootstrapToken: "fixture-distinct-grant",
      candidateVersion: "0.7.3",
      workspaceRoot: fixture.input.workspaceRoot,
      uploadReceiptPath: fixture.input.remoteUploadSecretPath,
      requireWide: true,
      onInstallDispatched: expect.any(Function),
    });
    const receipt = fixture.files.get(fixture.input.remoteSecretPath);
    expect(receipt?.mode).toBe(0o600);
    expect(JSON.parse(receipt!.contents)).toEqual({
      endpoint: "http://127.0.0.1:43123",
      bootstrapToken: "fixture-distinct-grant",
    });
    expect(NodePath.dirname(fixture.input.remoteSecretPath)).not.toBe(
      NodePath.dirname(fixture.input.remoteEvidencePath),
    );
    for (const [path, file] of fixture.files) {
      if (path !== fixture.input.remoteSecretPath)
        expect(file.contents).not.toContain("fixture-distinct-grant");
    }
  });

  it("waits for minting after native exposure has already widened", async () => {
    vi.useFakeTimers();
    const fixture = remoteGrantFixture([[], [publicPairingGrant]]);
    const outcome = fixture.run().then(
      () => null,
      (error: Error) => error.message,
    );
    await vi.runAllTimersAsync();
    expect(await outcome).toBeNull();
    expect(fixture.fetch).toHaveBeenCalledTimes(2);
    expect(fixture.driver).toHaveBeenCalledOnce();
    expect(vi.getTimerCount()).toBe(0);
  });

  it.each([
    ["this-computer", [{ ...publicPairingGrant, reach: "this-computer" }]],
    ["custom", [{ ...publicPairingGrant, reach: "custom" }]],
    ["blank credential", [{ ...publicPairingGrant, credential: "  " }]],
    ["missing id", [{ ...publicPairingGrant, id: undefined }]],
    ["null entry", [null]],
    ["no grant", []],
  ])(
    "fails closed within a bound for %s without using the desktop bootstrap",
    async (_name, links) => {
      vi.useFakeTimers();
      const fixture = remoteGrantFixture([links]);
      const outcome = fixture.run().then(
        () => "unexpected success",
        (error: Error) => error.message,
      );
      await vi.runAllTimersAsync();
      expect(await outcome).toBe("Remote verification has no live native sharing grant.");
      expect(fixture.driver).not.toHaveBeenCalled();
      expect(fixture.files.size).toBe(0);
      expect(vi.getTimerCount()).toBe(0);
    },
  );

  it("rejects malformed list responses without exposing their contents", async () => {
    const fixture = remoteGrantFixture([{ privateDetail: "fixture-private-response" }]);
    await expect(fixture.run()).rejects.toThrow(
      "Remote verification pairing grant response invalid.",
    );
    expect(fixture.files.size).toBe(0);
    expect(fixture.driver).not.toHaveBeenCalled();
  });

  it.each(["http", "body", "transport"])(
    "sanitizes %s failures before any private handoff",
    async (failure) => {
      vi.useFakeTimers();
      const fixture = remoteGrantFixture([[publicPairingGrant]]);
      fixture.fetch.mockImplementation(async () => {
        if (failure === "transport") throw new Error("fixture-private-network-detail");
        return {
          ok: failure !== "http",
          json: async () => {
            throw new Error("fixture-private-body-detail");
          },
        };
      });
      const outcome = fixture.run().then(
        () => "unexpected success",
        (error: Error) => error.message,
      );
      await vi.runAllTimersAsync();
      expect(String(await outcome)).toContain("Remote verification pairing grant unavailable.");
      expect(String(await outcome)).toContain("status=");
      expect(String(await outcome)).toContain("attempts=");
      expect(String(await outcome)).toContain("endpointChanged=false");
      expect(String(await outcome)).not.toContain("fixture-private");
      expect(fixture.fetch.mock.calls.length).toBeGreaterThan(1);
      expect(fixture.files.size).toBe(0);
      expect(fixture.driver).not.toHaveBeenCalled();
    },
  );

  it("retries a restart-time pairing refusal until the grant is published", async () => {
    vi.useFakeTimers();
    const fixture = remoteGrantFixture([[publicPairingGrant]]);
    fixture.fetch
      .mockRejectedValueOnce(new Error("fixture-private-reset"))
      .mockResolvedValueOnce({ ok: true, json: async () => [publicPairingGrant] });
    const outcome = fixture.run().then(
      () => null,
      (error: Error) => error.message,
    );
    await vi.runAllTimersAsync();
    expect(await outcome).toBeNull();
    expect(fixture.driver).toHaveBeenCalledOnce();
    expect(fixture.files.size).toBeGreaterThan(0);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("uses the bootstrap published after the restarted server replaces the cached endpoint", async () => {
    vi.useFakeTimers();
    const fixture = remoteGrantFixture([[publicPairingGrant]]);
    let reads = 0;
    fixture.context.window.desktopBridge.getLocalEnvironmentBootstraps = () => {
      reads += 1;
      return [
        {
          id: "primary",
          httpBaseUrl: reads === 1 ? "http://127.0.0.1:1" : "http://127.0.0.1:43123",
          bootstrapToken: "fixture-desktop-bootstrap",
        },
      ];
    };
    fixture.fetch.mockImplementation(async (url: URL) => {
      if (url.origin === "http://127.0.0.1:1") throw new Error("fixture-private-reset");
      return { ok: true, status: 200, json: async () => [publicPairingGrant] };
    });
    const outcome = fixture.run().then(
      () => null,
      (error: Error) => error.message,
    );
    await vi.runAllTimersAsync();
    expect(await outcome).toBeNull();
    expect(reads).toBeGreaterThan(1);
    expect(fixture.driver).toHaveBeenCalledOnce();
    expect(fixture.fetch.mock.calls.map((call) => call[0]?.origin)).toEqual([
      "http://127.0.0.1:1",
      "http://127.0.0.1:43123",
    ]);
  });

  it("records share and credential steps without the grant or its endpoint", () => {
    const spec = createSeededUpgradeDriverSpec({
      candidateVersion: "0.7.3",
      expectedDataRoot: absolute("remote", "data"),
      lane: "remote-install",
      phase: "seed-and-install",
      projectId: "remote-project",
      resultPath: absolute("remote", "before.json"),
      workspaceRoot: absolute("remote", "workspace"),
      platform: "linux",
      appBinaryPath: absolute("RemoteLane.AppImage"),
      remoteInstallDriverPath: "fixture:driver",
      remoteHarnessPath: "fixture:host",
      remoteSecretPath: absolute("remote", "private.json"),
      remoteUploadSecretPath: absolute("remote", "upload-private.json"),
      remoteEvidencePath: absolute("remote", "evidence", "remote-rpc.json"),
      remoteTracePath: absolute("remote", "evidence", "remote-credential-trace.log"),
    });
    const start = spec.indexOf("    const traceRemote = (entry) => {");
    const end = spec.indexOf("    await browser.execute(() => { window.location.hash", start);
    if (start < 0 || end < 0) throw new Error("Generated remote trace is missing.");
    const lines: string[] = [];
    const traceRemote = NodeVM.runInNewContext(`${spec.slice(start, end)}\ntraceRemote;`, {
      input: { remoteTracePath: absolute("remote", "evidence", "remote-credential-trace.log") },
      NodeFS: {
        appendFileSync: (_path: string, line: string) => {
          lines.push(line);
        },
      },
      Date,
    }) as (entry: {
      readonly step: string;
      readonly message?: string;
      readonly found?: boolean;
      readonly clicked?: boolean;
      readonly widened?: boolean;
    }) => void;
    const secret = "fixture-distinct-grant-token-value";
    traceRemote({
      step: "credentials",
      found: true,
      clicked: true,
      widened: true,
      message: `grant ${secret} at http://10.0.0.8:43123/secret`,
    });
    expect(lines).toHaveLength(1);
    const parsed = JSON.parse(lines[0]!) as {
      readonly step: string;
      readonly message: string;
      readonly found: boolean;
      readonly widened: boolean;
    };
    expect(parsed).toMatchObject({
      step: "credentials",
      found: true,
      clicked: true,
      widened: true,
    });
    expect(parsed.message).toContain("[redacted]");
    expect(parsed.message).toContain("[url]");
    expect(parsed.message).not.toContain(secret);
    expect(parsed.message).not.toContain("10.0.0.8");
    const failingWriter = NodeVM.runInNewContext(`${spec.slice(start, end)}\ntraceRemote;`, {
      input: { remoteTracePath: absolute("remote", "evidence", "remote-credential-trace.log") },
      NodeFS: {
        appendFileSync: () => {
          throw new Error("fixture-private-disk");
        },
      },
      Date,
    }) as (entry: { readonly step: string }) => void;
    expect(() => failingWriter({ step: "credentials" })).not.toThrow();
  });

  it("aborts a stalled grant request before the embedded driver command deadline", async () => {
    vi.useFakeTimers();
    const fixture = remoteGrantFixture([]);
    const aborted = vi.fn();
    fixture.fetch.mockImplementation(
      (_url, options) =>
        new Promise((_resolve, reject) => {
          options.signal?.addEventListener("abort", () => {
            aborted();
            reject(new Error("fixture-private-abort-detail"));
          });
        }),
    );
    const startedAt = vi.getMockedSystemTime()!.getTime();
    const outcome = fixture.run().then(
      () => ({
        message: "unexpected success",
        elapsed: vi.getMockedSystemTime()!.getTime() - startedAt,
      }),
      (error: Error) => ({
        message: error.message,
        elapsed: vi.getMockedSystemTime()!.getTime() - startedAt,
      }),
    );
    await vi.runAllTimersAsync();
    expect((await outcome).message).toContain("Remote verification pairing grant unavailable.");
    expect((await outcome).message).toContain("status=none");
    expect((await outcome).message).not.toContain("fixture-private");
    expect((await outcome).elapsed).toBeGreaterThan(0);
    expect((await outcome).elapsed).toBeLessThan(30_000);
    expect(aborted.mock.calls.length).toBeGreaterThan(1);
    expect(fixture.driver).not.toHaveBeenCalled();
    expect(fixture.files.size).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
  });
});

describe("seeded packaging build budgets", () => {
  // Exercise the actual command and its three call sites without launching a
  // native build, updater, or application. Keep the existing command checks.
  const source = NodeFS.readFileSync(
    new URL("./seeded-desktop-upgrade-smoke.ts", import.meta.url),
    "utf8",
  );
  const commandStart = source.indexOf("const requireCommandSuccess = async");
  const commandEnd = source.indexOf("\nconst writePrivateJson", commandStart);
  const buildStart = source.indexOf("const buildPackagedApplication = async");
  const buildEnd = source.indexOf("\nexport const seededUpgradeBundleRoot", buildStart);
  const callsStart = source.indexOf("    await buildPackagedApplication({", buildEnd);
  const callsEnd = source.indexOf("    await publishCandidateUpdater({", callsStart);
  if ([commandStart, commandEnd, buildStart, buildEnd, callsStart, callsEnd].some((i) => i < 0))
    throw new Error("Seeded packaging command source is unavailable.");

  const fixture = (platform: SeededDesktopUpgradeSmokeInput["platform"], arch: "arm64" | "x64") => {
    const layout = createSeededUpgradeRunLayout(absolute("work"), "budget-fixture");
    const runCommand = vi.fn(async (_input: Parameters<typeof runBoundedCommand>[0]) => ({
      exitCode: 0,
      stderr: "",
      stdout: "",
    }));
    const prepareSeededUpgradeBuild = vi.fn(async () => {});
    const signingEnvironment = { TAURI_SIGNING_PRIVATE_KEY: "fixture-private-key" };
    const context = {
      NodePath,
      runCommand,
      prepareSeededUpgradeBuild,
      seededUpgradeVitePlusExecutable,
      seededUpgradeRustTarget,
      SeededDesktopUpgradeSmokeError,
      layout,
      candidateOverlay: absolute("candidate-overlay.json"),
      previousOverlay: absolute("previous-overlay.json"),
      protectedOverlay: absolute("protected-overlay.json"),
      signingEnvironment,
      input: {
        arch,
        platform,
        bundle: platform === "mac" ? "dmg" : platform === "win" ? "nsis" : "appimage",
        repositoryRoot,
        candidateVersion: "9.9.9-upgrade.1",
        previousVersion: "1.2.3",
        wsl: false,
      },
    };
    return {
      ...context,
      run: () =>
        NodeVM.runInNewContext(
          NodeModule.stripTypeScriptTypes(
            `(async () => { ${source.slice(commandStart, commandEnd)}\n${source.slice(buildStart, buildEnd)}\n${source.slice(callsStart, callsEnd)} })()`,
          ),
          context,
          { timeout: 1_000 },
        ) as Promise<void>,
    };
  };

  it.each([
    ["mac", "x64", 90],
    ["mac", "arm64", 90],
    ["linux", "x64", 45],
    ["linux", "arm64", 45],
    ["win", "x64", 45],
    ["win", "arm64", 45],
  ] as const)(
    "bounds all three %s/%s packaging commands at %i minutes",
    async (platform, arch, minutes) => {
      const owned = fixture(platform, arch);
      await owned.run();
      expect(owned.prepareSeededUpgradeBuild).toHaveBeenCalledTimes(3);
      expect(owned.runCommand).toHaveBeenCalledTimes(6);
      const builds = [
        [owned.layout.candidateCheckout, owned.layout.candidateBuildRoot, owned.candidateOverlay],
        [
          owned.layout.previousStable.checkout,
          owned.layout.previousStable.buildRoot,
          owned.previousOverlay,
        ],
        [
          owned.layout.protectedBaseline.checkout,
          owned.layout.protectedBaseline.buildRoot,
          owned.protectedOverlay,
        ],
      ];
      expect(new Set(builds.map(([, target]) => target)).size).toBe(3);
      for (const [index, [checkout, target, overlay]] of builds.entries()) {
        expect(owned.runCommand).toHaveBeenNthCalledWith(index * 2 + 1, {
          command: seededUpgradeVitePlusExecutable,
          args: ["install", "--frozen-lockfile"],
          cwd: checkout,
          inherit: true,
          timeoutMs: 10 * 60_000,
        });
        expect(owned.runCommand).toHaveBeenNthCalledWith(index * 2 + 2, {
          command: seededUpgradeVitePlusExecutable,
          args: [
            "run",
            "--filter",
            "@coddy/desktop",
            "build",
            "--features",
            "desktop-e2e",
            "--config",
            NodePath.join(checkout!, "apps/desktop/src-tauri/tauri.release.conf.json"),
            "--config",
            NodePath.join(checkout!, "apps/desktop/src-tauri/tauri.e2e.conf.json"),
            "--config",
            overlay,
            "--bundles",
            platform === "mac" ? "app,dmg" : owned.input.bundle,
            "--target",
            seededUpgradeRustTarget(platform, arch),
          ],
          cwd: checkout,
          env: { ...owned.signingEnvironment, CARGO_TARGET_DIR: target },
          inherit: true,
          timeoutMs: minutes * 60_000,
        });
      }
    },
  );

  it.each([1, 2])(
    "still stops on failed command %i before later packages",
    async (failedCommand) => {
      const owned = fixture("mac", "x64");
      if (failedCommand === 2)
        owned.runCommand.mockResolvedValueOnce({ exitCode: 0, stderr: "", stdout: "" });
      owned.runCommand.mockResolvedValueOnce({ exitCode: 7, stderr: "", stdout: "" });
      await expect(owned.run()).rejects.toThrow("exited with code 7");
      expect(owned.runCommand).toHaveBeenCalledTimes(failedCommand);
      expect(owned.prepareSeededUpgradeBuild).toHaveBeenCalledTimes(1);
    },
  );
});

/** Minimal VERSIONINFO whose ProductVersion alignment is relative to the resource, not the file. */
const windowsVersionResource = (productVersion: string, prefixLength = 0): Buffer => {
  const marker = Buffer.from("VS_VERSION_INFO\u0000", "utf16le");
  const key = Buffer.from("ProductVersion\u0000", "utf16le");
  const value = Buffer.from(`${productVersion}\u0000`, "utf16le");
  let body = Buffer.concat([Buffer.alloc(6), marker, key]);
  const misalignment = body.length % 4;
  if (misalignment !== 0) body = Buffer.concat([body, Buffer.alloc(4 - misalignment)]);
  body = Buffer.concat([body, value]);
  body.writeUInt16LE(body.length, 0);
  return Buffer.concat([Buffer.alloc(prefixLength, 0xab), body]);
};

describe("seeded packaged desktop upgrade harness", () => {
  it("keeps polling after a joined command deadline but still requires the installed candidate", async () => {
    const candidate = "0.7.4-upgrade.synthetic";
    const installed = {
      exists: true,
      productVersion: candidate,
      sha256: "a".repeat(64),
      installers: [],
      error: null,
    };
    const run = vi
      .fn()
      .mockRejectedValueOnce(new SeededUpgradeCommandTimeoutError("synthetic command deadline"))
      .mockResolvedValue({ exitCode: 0, stdout: JSON.stringify(installed), stderr: "" });
    let now = 0;
    const observations: unknown[] = [];
    await waitForUpgradeCondition({
      description: "installed Windows candidate",
      intervalMs: 10,
      now: () => now,
      sleep: async (milliseconds) => {
        now += milliseconds;
      },
      timeoutMs: 50,
      probe: async () => {
        const result = await readWindowsCandidateProbe(run);
        observations.push(result);
        return result.exitCode === 0 && windowsCandidateIsInstalled(result.observation, candidate);
      },
    });
    expect(run).toHaveBeenCalledTimes(2);
    expect(observations[0]).toEqual({
      exitCode: null,
      observation: { error: "Windows version probe command deadline" },
    });
    expect(now).toBe(10);
  });

  it("fails the unchanged overall poll when every joined command sample is unavailable", async () => {
    const run = vi.fn(async () => {
      throw new SeededUpgradeCommandTimeoutError("synthetic command deadline");
    });
    let now = 0;
    await expect(
      waitForUpgradeCondition({
        description: "installed Windows candidate",
        intervalMs: 10,
        now: () => now,
        sleep: async (milliseconds) => {
          now += milliseconds;
        },
        timeoutMs: 20,
        probe: async () => {
          const result = await readWindowsCandidateProbe(run);
          return result.exitCode === 0 && windowsCandidateIsInstalled(result.observation, "0.7.4");
        },
      }),
    ).rejects.toThrow(/installed Windows candidate.*20ms/);
    expect(run).toHaveBeenCalledTimes(2);
    expect(now).toBe(20);
  });

  it("propagates unverified cleanup and spawn failures instead of retrying their messages", async () => {
    for (const error of [
      new SeededDesktopUpgradeSmokeError("synthetic cleanup deadline"),
      new Error("synthetic command timed out without joined cleanup"),
    ]) {
      await expect(readWindowsCandidateProbe(async () => Promise.reject(error))).rejects.toBe(
        error,
      );
    }
  });

  it("joins close rather than treating an exit code as completed child cleanup", async () => {
    const child = new NodeChildProcess.ChildProcess();
    Object.defineProperty(child, "exitCode", { value: 0 });
    const kill = vi.spyOn(child, "kill").mockReturnValue(true);
    let settled = false;
    const cleanup = terminateSeededUpgradeChild(child).then(() => {
      settled = true;
    });
    await Promise.resolve();
    expect(settled).toBe(false);
    child.emit("close", 0, null);
    await cleanup;
    expect(kill).not.toHaveBeenCalled();
    await expect(terminateSeededUpgradeChild(child)).resolves.toBeUndefined();
  });

  it("fails closed at the existing cleanup deadline when the exact child never closes", async () => {
    vi.useFakeTimers();
    const child = new NodeChildProcess.ChildProcess();
    const kill = vi.spyOn(child, "kill").mockReturnValue(true);
    const unref = vi.spyOn(child, "unref").mockImplementation(() => undefined);
    const rejection = expect(terminateSeededUpgradeChild(child)).rejects.toThrow(
      /child cleanup did not close within 5000ms/,
    );
    try {
      await vi.advanceTimersByTimeAsync(5_000);
      await rejection;
      expect(kill.mock.calls).toEqual([["SIGTERM"], ["SIGKILL"]]);
      expect(unref).toHaveBeenCalledOnce();
    } finally {
      child.emit("close", null, "SIGKILL");
      vi.useRealTimers();
    }
  });

  it("kills the Windows process tree when owned-child cleanup gives up", () => {
    expect(ownedChildTreeKillCommand("win32", 4242)).toEqual({
      args: ["/F", "/T", "/PID", "4242"],
      command: "taskkill.exe",
    });
    expect(ownedChildTreeKillCommand("win32", undefined)).toBeUndefined();
    expect(ownedChildTreeKillCommand("linux", 4242)).toBeUndefined();
  });

  it("exits nonzero when the smoke caller reports a failure", () => {
    const exit = vi.fn();
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      reportSeededUpgradeSmokeFailure(
        new SeededDesktopUpgradeSmokeError("Owned child cleanup did not close within 5000ms."),
        exit,
      );
      expect(exit).toHaveBeenCalledExactlyOnceWith(1);
      expect(error).toHaveBeenCalledWith("Owned child cleanup did not close within 5000ms.");
      reportSeededUpgradeSmokeFailure("not an error", exit);
      expect(exit).toHaveBeenLastCalledWith(1);
      expect(error).toHaveBeenLastCalledWith("Seeded packaged-upgrade smoke failed.");
    } finally {
      error.mockRestore();
    }
  });

  it("waits for the exact Windows candidate and installer exit before cleanup", () => {
    const candidate = "0.7.3-upgrade.46";
    const installed = {
      exists: true,
      productVersion: candidate,
      sha256: "a".repeat(64),
      installers: [],
      error: null,
    };
    expect(windowsCandidateIsInstalled(installed, candidate)).toBe(true);
    expect(windowsCandidateIsInstalled({ ...installed, productVersion: "0.7.2" }, candidate)).toBe(
      false,
    );
    expect(
      windowsCandidateIsInstalled({ ...installed, productVersion: "0.7.3-upgrade.45" }, candidate),
    ).toBe(false);
    expect(windowsCandidateIsInstalled({ ...installed, exists: false }, candidate)).toBe(false);
    expect(windowsCandidateIsInstalled({ ...installed, sha256: null }, candidate)).toBe(false);
    expect(
      windowsCandidateIsInstalled({ ...installed, installers: [{ pid: 42 }] }, candidate),
    ).toBe(false);
    expect(windowsCandidateIsInstalled({ ...installed, error: "probe failed" }, candidate)).toBe(
      false,
    );
    expect(windowsCandidateIsInstalled({}, candidate)).toBe(false);
    expect(windowsCandidateIsInstalled(null, candidate)).toBe(false);
  });

  it("reads ProductVersion from a VERSIONINFO resource that is not file-aligned", () => {
    const version = "0.8.3-upgrade.194";
    const bytes = windowsVersionResource(version, 2);
    expect(productVersionFromExecutableBytes(bytes)).toBe(version);
    expect(productVersionFromExecutableBytes(Buffer.from("ProductVersion"))).toBeNull();
  });

  it("sees the updater's coddy-studio-desktop installer while it runs", async () => {
    const observation = await observeWindowsInstalledCandidate({
      appBinaryPath: "C:\\Coddy Studio\\coddy-studio-desktop.exe",
      candidateVersion: "0.7.3-upgrade.223",
      readFile: async () => Buffer.from(""),
      listProcesses: async () =>
        `"coddy-studio-desktop-0.7.3-upg","4200","Console","1","9,000 K"\r\n"explorer.exe","1","Console","1","1 K"`,
    });
    expect(observation.installers).toEqual([{ pid: 4200, parentPid: null, path: null }]);
  });

  it("matches a truncated installer image without treating a short prefix as that installer", () => {
    const expected = "Coddy Studio-0.8.3-upgrade.194-installer.exe";
    const truncated = expected.slice(0, 25);
    const csv = [
      `"${truncated}","592","Console","1","12,345 K"`,
      `"${expected}","593","Console","1","12,345 K"`,
      `"Coddy Studio","9","Console","1","1 K"`,
      `"notepad.exe","11","Console","1","1 K"`,
    ].join("\r\n");
    expect(windowsInstallerProcesses(csv, expected)).toEqual([
      { pid: 592, parentPid: null, path: null },
      { pid: 593, parentPid: null, path: null },
    ]);
  });

  it("records an in-process Windows sample without treating a hung installer lookup as installed", async () => {
    const version = "0.8.3-upgrade.194";
    const bytes = windowsVersionResource(version);
    const installed = await observeWindowsInstalledCandidate({
      appBinaryPath: "C:\\Program Files\\Coddy Studio\\coddy-studio.exe",
      candidateVersion: version,
      readFile: async () => bytes,
      listProcesses: async () => `"notepad.exe","11"\r\n`,
    });
    expect(installed.productVersion).toBe(version);
    expect(installed.sha256).toMatch(/^[a-f0-9]{64}$/);
    expect(installed.error).toBeNull();
    expect(windowsCandidateIsInstalled(installed, version)).toBe(true);

    const previous = await observeWindowsInstalledCandidate({
      appBinaryPath: installed.path,
      candidateVersion: version,
      readFile: async () => windowsVersionResource("0.8.2"),
      listProcesses: async () => "",
    });
    expect(previous.productVersion).toBe("0.8.2");
    expect(previous.sha256).toBeNull();
    expect(windowsCandidateIsInstalled(previous, version)).toBe(false);

    const missing = Object.assign(new Error("missing"), { code: "ENOENT" });
    const absent = await observeWindowsInstalledCandidate({
      appBinaryPath: installed.path,
      candidateVersion: version,
      readFile: async () => Promise.reject(missing),
      listProcesses: async () => "",
    });
    expect(absent.exists).toBe(false);
    expect(absent.error).toBe("executable read ENOENT");

    const locked = await observeWindowsInstalledCandidate({
      appBinaryPath: installed.path,
      candidateVersion: version,
      readFile: async () => Promise.reject(Object.assign(new Error("slow"), { code: "ABORT_ERR" })),
      listProcesses: async () => Promise.reject(Object.assign(new Error("slow"), { killed: true })),
    });
    expect(locked.error).toBe("executable read timed out; installer lookup timed out");
    expect(windowsCandidateIsInstalled(locked, version)).toBe(false);

    const replacedDuringLookup = await observeWindowsInstalledCandidate({
      appBinaryPath: installed.path,
      candidateVersion: version,
      readFile: async () => bytes,
      listProcesses: async () => Promise.reject(Object.assign(new Error("slow"), { killed: true })),
    });
    expect(replacedDuringLookup.productVersion).toBe(version);
    expect(replacedDuringLookup.sha256).toMatch(/^[a-f0-9]{64}$/);
    expect(replacedDuringLookup.error).toBe("installer lookup timed out");
    expect(windowsCandidateIsInstalled(replacedDuringLookup, version)).toBe(false);

    const running = await observeWindowsInstalledCandidate({
      appBinaryPath: installed.path,
      candidateVersion: version,
      readFile: async () => bytes,
      listProcesses: async () => {
        const image = "Coddy Studio-0.8.3-upgrade.194-installer.exe".slice(0, 25);
        return `"${image}","592"\r\n`;
      },
    });
    expect(running.installers).toEqual([{ pid: 592, parentPid: null, path: null }]);
    expect(windowsCandidateIsInstalled(running, version)).toBe(false);
  });

  it("writes every Windows sample and finishes only when the candidate is installed", async () => {
    const root = await NodeFS.promises.mkdtemp(NodePath.join(NodeOS.tmpdir(), "windows-handoff-"));
    try {
      const version = "0.8.3-upgrade.194";
      const installer = "Coddy Studio-0.8.3-upgrade.194-installer.exe".slice(0, 25);
      let reads = 0;
      let now = 0;
      await waitForWindowsInstalledCandidate({
        appBinaryPath: "C:\\Program Files\\Coddy Studio\\coddy-studio.exe",
        candidateVersion: version,
        evidenceDirectory: root,
        intervalMs: 10,
        now: () => now,
        readFile: async () => {
          reads += 1;
          return windowsVersionResource(reads === 1 ? "0.8.2" : version);
        },
        listProcesses: async () => (reads === 1 ? `"${installer}","592"\r\n` : ""),
        sleep: async (milliseconds) => {
          now += milliseconds;
        },
        timeoutMs: 50,
      });
      const rows = (
        await NodeFS.promises.readFile(NodePath.join(root, "windows-install-handoff.log"), "utf8")
      )
        .trim()
        .split("\n")
        .map(
          (line) =>
            JSON.parse(line) as {
              readonly exitCode: number | null;
              readonly observation: {
                readonly productVersion: string;
                readonly installers: unknown;
              };
            },
        );
      expect(rows.map((row) => row.observation.productVersion)).toEqual(["0.8.2", version]);
      expect(rows[0]?.observation.installers).toEqual([{ pid: 592, parentPid: null, path: null }]);
      expect(rows[1]?.exitCode).toBe(0);
      expect(now).toBe(10);

      now = 0;
      await expect(
        waitForWindowsInstalledCandidate({
          appBinaryPath: "C:\\Program Files\\Coddy Studio\\coddy-studio.exe",
          candidateVersion: version,
          evidenceDirectory: root,
          intervalMs: 10,
          now: () => now,
          readFile: async () => windowsVersionResource("0.8.2"),
          listProcesses: async () => "",
          sleep: async (milliseconds) => {
            now += milliseconds;
          },
          timeoutMs: 20,
        }),
      ).rejects.toThrow(
        "Timed out waiting for Windows candidate 0.8.3-upgrade.194 at the installed application path after 20ms.",
      );
    } finally {
      await NodeFS.promises.rm(root, { recursive: true, force: true });
    }
  });

  it("keeps the redacted remote trace when raw remote logs are withheld", async () => {
    const root = await NodeFS.promises.mkdtemp(NodePath.join(NodeOS.tmpdir(), "upgrade-evidence-"));
    try {
      const layout = createSeededUpgradeRunLayout(root, "copy-test");
      const secret = "fixture-private-returned-token";
      await NodeFS.promises.mkdir(layout.previousStable.evidenceDirectory, { recursive: true });
      await NodeFS.promises.mkdir(layout.remoteInstall.evidenceDirectory, { recursive: true });
      await NodeFS.promises.mkdir(NodePath.join(layout.remoteInstall.evidenceDirectory, "nested"), {
        recursive: true,
      });
      await NodeFS.promises.mkdir(layout.remoteInstall.dataRoot, { recursive: true });
      await NodeFS.promises.writeFile(
        NodePath.join(layout.previousStable.evidenceDirectory, "result.json"),
        '{"lane":"previous-stable"}\n',
      );
      await NodeFS.promises.writeFile(
        NodePath.join(layout.remoteInstall.evidenceDirectory, "remote-credential-trace.log"),
        `${JSON.stringify({ step: "credentials", message: `grant ${secret}` })}\n`,
      );
      await NodeFS.promises.writeFile(
        NodePath.join(layout.remoteInstall.evidenceDirectory, "seed-and-install.log"),
        "Remote WebDriver output was withheld because the private credential receipt was not published.\n",
      );
      await NodeFS.promises.writeFile(
        NodePath.join(layout.remoteInstall.evidenceDirectory, "wdio-0-0.log"),
        `RESULT ${secret}\n`,
      );
      await NodeFS.promises.writeFile(
        NodePath.join(
          layout.remoteInstall.evidenceDirectory,
          "nested",
          "remote-credential-trace.log",
        ),
        secret,
      );
      await NodeFS.promises.writeFile(
        NodePath.join(layout.remoteInstall.dataRoot, "secret.txt"),
        secret,
      );
      const artifactDirectory = NodePath.join(root, "artifact");
      await copyBoundedEvidence({
        artifactDirectory,
        layout,
        requestLogPath: NodePath.join(root, "missing-requests.jsonl"),
        secrets: [secret],
        withholdLanes: ["remote-install"],
      });
      const names = (await NodeFS.promises.readdir(artifactDirectory)).toSorted();
      expect(names).toEqual([
        "previous-stable-result.json",
        "remote-install-remote-credential-trace.log",
        "remote-install-seed-and-install.log",
      ]);
      const trace = await NodeFS.promises.readFile(
        NodePath.join(artifactDirectory, "remote-install-remote-credential-trace.log"),
        "utf8",
      );
      expect(trace).toContain("[REDACTED]");
      expect(trace).not.toContain(secret);
      const retained = (
        await Promise.all(
          names.map((name) =>
            NodeFS.promises.readFile(NodePath.join(artifactDirectory, name), "utf8"),
          ),
        )
      ).join("\n");
      expect(retained).not.toContain(secret);
    } finally {
      await NodeFS.promises.rm(root, { recursive: true, force: true });
    }
  });

  it("generates a syntactically valid isolated remote-install phase", async () => {
    const directory = await NodeFS.promises.mkdtemp(
      NodePath.join(NodeOS.tmpdir(), "coddy-studio-remote-spec-"),
    );
    try {
      const spec = createSeededUpgradeDriverSpec({
        candidateVersion: "0.7.3",
        expectedDataRoot: NodePath.join(directory, "data"),
        lane: "remote-install",
        phase: "seed-and-install",
        projectId: "remote-project",
        resultPath: NodePath.join(directory, "before.json"),
        workspaceRoot: NodePath.join(directory, "workspace"),
        platform: "linux",
        appBinaryPath: NodePath.join(directory, "RemoteLane.AppImage"),
        remoteInstallDriverPath: "file:///isolated/remote-install-driver.ts",
        remoteHarnessPath: "file:///isolated/seeded-desktop-upgrade-smoke.ts",
        remoteSecretPath: NodePath.join(directory, "private.json"),
        remoteUploadSecretPath: NodePath.join(directory, "upload-private.json"),
        remoteEvidencePath: NodePath.join(directory, "evidence.json"),
        remoteTracePath: NodePath.join(directory, "remote-credential-trace.log"),
      });
      const path = NodePath.join(directory, "remote.e2e.mjs");
      await NodeFS.promises.writeFile(path, spec);
      const result = NodeChildProcess.spawnSync(process.execPath, ["--check", path], {
        encoding: "utf8",
        timeout: 10_000,
      });
      expect(result.status, result.stderr).toBe(0);
      expect(spec).toContain("remote-credential-trace.log");
      expect(spec).toContain('step: "share-ui"');
      expect(spec).toContain('step: "credentials"');
      expect(spec).toContain("[redacted]");
      expect(spec).not.toContain("bridge.installUpdate()");
    } finally {
      await NodeFS.promises.rm(directory, { recursive: true, force: true });
    }
  });
  it("requires remote phase receipts and helper paths before generating its executable scenario", () => {
    expect(() =>
      createSeededUpgradeDriverSpec({
        candidateVersion: "0.7.3",
        expectedDataRoot: absolute("remote", "data"),
        lane: "remote-install",
        phase: "seed-and-install",
        projectId: "remote-project",
        resultPath: absolute("remote", "before.json"),
        workspaceRoot: absolute("remote", "workspace"),
      }),
    ).toThrow(SeededDesktopUpgradeSmokeError);
  });
  it("rejects a local invocation before inspecting files or starting applications", async () => {
    vi.stubEnv("CI", "false");
    try {
      await expect(
        runSeededDesktopUpgradeSmoke({} as SeededDesktopUpgradeSmokeInput),
      ).rejects.toThrow("CI-only");
    } finally {
      vi.unstubAllEnvs();
    }
  });
  it("keeps remote installation in its own root and avoids user/dev ports", () => {
    const layout = createSeededUpgradeRunLayout(absolute("work"), "remote-test");
    expect(layout.remoteInstall.dataRoot).toBe(absolute("work", "remote-test", "remote", "data"));
    expect(layout.remoteInstall.dataRoot).not.toBe(layout.protectedBaseline.dataRoot);
    for (const port of [3773, 5733, 13773, 18431, 18432, 0, 65536])
      expect(() => assertRemoteInstallPort(port)).toThrow(SeededDesktopUpgradeSmokeError);
    expect(assertRemoteInstallPort(43123)).toBe(43123);
  });
  it("counts only the isolated remote AppImage mount prefix", () => {
    expect(
      countAppImageMounts(
        "image /tmp/.mount_Remote123 fuse ro 0 0\nimage /tmp/.mount_bibcod456 fuse ro 0 0\ntmpfs /tmp tmpfs rw 0 0",
        ".mount_Remote",
      ),
    ).toBe(1);
  });
  it("requires a candidate restart, stable storage, protection progress, backup, and one isolated runtime", () => {
    const good: RemoteInstallEvidence = {
      before: { bootId: "b1", serverVersion: "0.7.2", storageInstanceId: "s1" },
      after: { bootId: "b2", serverVersion: "0.7.3-upgrade.1", storageInstanceId: "s1" },
      phases: ["downloading", "installing", "succeeded"],
      sawPercent: true,
      sawStage: true,
      heldUpload: {
        admitted: true,
        releasedOnWaitingStage: true,
        completed: true,
        bytesMatch: true,
        noPartials: true,
        requestClosed: true,
        holdLimitMs: 20_000,
      },
      preUpdateBackups: 1,
      requesterLogLines: 1,
      appImageMounts: 1,
      runtimeProcesses: 1,
    };
    expect(() => verifyRemoteInstallOutcome(good, "0.7.3-upgrade.1")).not.toThrow();
    const invalid = [
      { ...good, after: { ...good.after, bootId: "b1" } },
      { ...good, after: { ...good.after, serverVersion: "0.7.2" } },
      { ...good, after: { ...good.after, storageInstanceId: "s2" } },
      { ...good, phases: [] },
      { ...good, sawPercent: false },
      { ...good, sawStage: false },
      { ...good, heldUpload: { ...good.heldUpload, releasedOnWaitingStage: false } },
      { ...good, heldUpload: { ...good.heldUpload, requestClosed: false } },
      { ...good, heldUpload: { ...good.heldUpload, bytesMatch: false } },
      { ...good, heldUpload: { ...good.heldUpload, noPartials: false } },
      { ...good, preUpdateBackups: 0 },
      { ...good, requesterLogLines: 2 },
      { ...good, appImageMounts: 2 },
      { ...good, runtimeProcesses: 2 },
    ];
    for (const evidence of invalid)
      expect(() => verifyRemoteInstallOutcome(evidence, "0.7.3-upgrade.1")).toThrow(
        SeededDesktopUpgradeSmokeError,
      );
  });
  it("selects only process ids with this lane's exact data root", () => {
    expect(
      scopedCleanupPids(
        [
          {
            pid: 10,
            comm: "RemoteLane",
            environ: "CODDY_STUDIO_HOME=/tmp/remote/data\u0000HOME=/home/test\u0000",
          },
          {
            pid: 11,
            comm: "coddy-studio-desktop",
            environ: "CODDY_STUDIO_HOME=/tmp/remote/data-other\u0000",
          },
          { pid: 12, comm: "coddy-studio-desktop", environ: "HOME=/home/user\u0000" },
        ],
        "/tmp/remote/data",
      ),
    ).toEqual([10]);
  });
  it("launches the native Vite+ executable on every host", () => {
    expect(seededUpgradeVitePlusExecutable).toBe("vp");
  });

  it("targets only the exact updater-restarted application on every host", () => {
    expect(
      restartedApplicationCleanupPlan(
        String.raw`C:\Program Files\Coddy Studio\coddy-studio-desktop.exe`,
        "win",
      ),
    ).toEqual({
      args: ["/F", "/T", "/IM", "coddy-studio-desktop.exe"],
      command: "taskkill.exe",
    });
    expect(
      restartedApplicationCleanupPlan(
        "/private/tmp/Coddy Studio (test).app/Contents/MacOS/coddy-studio-desktop",
        "mac",
      ),
    ).toEqual({
      args: ["-TERM", "-x", "coddy-studio-desktop"],
      command: "pkill",
    });
    expect(restartedApplicationCleanupPlan("/tmp/installed/Coddy Studio.AppImage", "linux")).toEqual({
      args: ["-TERM", "-x", "coddy-studio-desktop"],
      command: "pkill",
    });
  });

  it("canonicalizes symlinked work roots before installing an updater target", async () => {
    const temporaryBase = await NodeFS.promises.mkdtemp(
      NodePath.join(NodeOS.tmpdir(), "coddy-studio-upgrade-canonical-owner-"),
    );
    try {
      const root = await NodeFS.promises.mkdtemp(NodePath.join(temporaryBase, "fixture-"));
      try {
        const target = NodePath.join(root, "target");
        const alias = NodePath.join(root, "alias");
        await NodeFS.promises.mkdir(target);
        await NodeFS.promises.symlink(target, alias, "junction");

        await expect(canonicalizeSeededUpgradeWorkRoot(alias)).resolves.toBe(
          await NodeFS.promises.realpath(target),
        );
      } finally {
        await NodeFS.promises.rm(root, { recursive: true, force: true });
      }
      await expect(NodeFS.promises.readdir(temporaryBase)).resolves.toEqual([]);
    } finally {
      await NodeFS.promises.rm(temporaryBase, { recursive: true, force: true });
    }
  });

  it("removes only the generated dependency tree before worktree cleanup", async () => {
    const checkout = await NodeFS.promises.mkdtemp(
      NodePath.join(NodeOS.tmpdir(), "coddy-studio-upgrade-cleanup-"),
    );
    try {
      await NodeFS.promises.mkdir(NodePath.join(checkout, "node_modules", ".pnpm", "deep"), {
        recursive: true,
      });
      await NodeFS.promises.writeFile(NodePath.join(checkout, "keep.txt"), "keep");

      await removeSeededUpgradeDependencyTree(checkout);

      await expect(NodeFS.promises.stat(NodePath.join(checkout, "node_modules"))).rejects.toThrow();
      await expect(
        NodeFS.promises.readFile(NodePath.join(checkout, "keep.txt"), "utf8"),
      ).resolves.toBe("keep");
    } finally {
      await NodeFS.promises.rm(checkout, { recursive: true, force: true });
    }
  });

  it("parses deterministic platform arguments and resolves only absolute isolated roots", () => {
    const repositoryRoot = NodePath.resolve("/repo");
    const input = parseSeededDesktopUpgradeSmokeArgs(
      [
        "--platform",
        "mac",
        "--arch",
        "arm64",
        "--bundle",
        "dmg",
        "--candidate-version",
        "0.3.11",
        "--previous-tag",
        "v0.3.10",
        "--previous-version",
        "0.3.10",
        "--public-key-file",
        absolute("keys", "updater.key.pub"),
        "--run-id",
        "run-17-mac-arm64",
        "--work-root",
        absolute("work"),
        "--artifact-dir",
        absolute("evidence"),
        "--updater-port",
        "4312",
        "--restart-timeout-ms",
        "90000",
      ],
      repositoryRoot,
    );

    expect(input).toEqual({
      arch: "arm64",
      artifactDirectory: absolute("evidence"),
      bundle: "dmg",
      candidateVersion: "0.3.11",
      platform: "mac",
      previousTag: "v0.3.10",
      previousVersion: "0.3.10",
      publicKeyFile: absolute("keys", "updater.key.pub"),
      repositoryRoot,
      restartTimeoutMs: 90_000,
      runId: "run-17-mac-arm64",
      updaterPort: 4_312,
      wsl: false,
      workRoot: absolute("work"),
    });
  });

  it("reserves the ordinary remote-install lane's highest port before any launch", () => {
    const base = [
      "--platform",
      "linux",
      "--arch",
      "x64",
      "--bundle",
      "appimage",
      "--candidate-version",
      "0.3.11",
      "--previous-tag",
      "v0.3.10",
      "--previous-version",
      "0.3.10",
      "--public-key-file",
      absolute("keys", "updater.key.pub"),
      "--run-id",
      "port-boundary",
      "--work-root",
      absolute("work"),
      "--artifact-dir",
      absolute("evidence"),
      "--updater-port",
      "65432",
    ];
    expect(parseSeededDesktopUpgradeSmokeArgs(base, "/repo").updaterPort).toBe(65_432);
    expect(() =>
      parseSeededDesktopUpgradeSmokeArgs(base.with(base.length - 1, "65433"), "/repo"),
    ).toThrow(/leave room/);
  });

  it("accepts WSL mode only for the supported Windows x64 target", () => {
    const base = [
      "--wsl",
      "--platform",
      "win",
      "--arch",
      "x64",
      "--bundle",
      "nsis",
      "--candidate-version",
      "0.3.11",
      "--previous-tag",
      "v0.3.10",
      "--previous-version",
      "0.3.10",
      "--public-key-file",
      absolute("keys", "updater.key.pub"),
      "--run-id",
      "run-17-win-wsl",
      "--work-root",
      absolute("work"),
      "--artifact-dir",
      absolute("evidence"),
    ];

    expect(parseSeededDesktopUpgradeSmokeArgs(base, "/repo").wsl).toBe(true);
    expect(() =>
      parseSeededDesktopUpgradeSmokeArgs(base.with(2, "mac").with(6, "dmg"), "/repo"),
    ).toThrow(/WSL.*Windows x64/);
  });

  it("accepts Windows ARM64 while rejecting relative roots and private-key arguments", () => {
    const base = [
      "--platform",
      "win",
      "--arch",
      "arm64",
      "--bundle",
      "nsis",
      "--candidate-version",
      "0.3.11",
      "--previous-tag",
      "v0.3.10",
      "--previous-version",
      "0.3.10",
      "--public-key-file",
      absolute("keys", "updater.key.pub"),
      "--run-id",
      "run-17-win-x64",
      "--work-root",
      absolute("work"),
      "--artifact-dir",
      absolute("evidence"),
    ];
    expect(parseSeededDesktopUpgradeSmokeArgs(base, "/repo").arch).toBe("arm64");
    expect(() =>
      parseSeededDesktopUpgradeSmokeArgs(
        base.with(3, "x64").with(base.indexOf(absolute("work")), "relative"),
        "/repo",
      ),
    ).toThrow(/absolute/);
    expect(() =>
      parseSeededDesktopUpgradeSmokeArgs(
        [...base.with(3, "x64"), "--private-key", "do-not-accept-secrets"],
        "/repo",
      ),
    ).toThrow(/private-key|Unknown option/);
  });

  it("maps Linux and Windows ARM64 to their native updater and Rust targets", () => {
    expect(updaterTargetFor("linux", "arm64")).toBe("linux-aarch64");
    expect(updaterTargetFor("win", "arm64")).toBe("windows-aarch64");
    expect(seededUpgradeRustTarget("linux", "arm64")).toBe("aarch64-unknown-linux-gnu");
    expect(seededUpgradeRustTarget("win", "arm64")).toBe("aarch64-pc-windows-msvc");
    expect(seededUpgradeBundleRoot("/tmp/build", "win", "arm64")).toBe(
      NodePath.join("/tmp/build", "aarch64-pc-windows-msvc", "release", "bundle"),
    );
  });

  it("creates disjoint owned checkouts for candidate and both baselines", () => {
    const layout = createSeededUpgradeRunLayout(absolute("work"), "run-17");

    expect(layout.previousStable.dataRoot).toBe(absolute("work", "run-17", "previous", "data"));
    expect(layout.previousStable.buildRoot).toBe(absolute("work", "run-17", "previous", "build"));
    expect(layout.protectedBaseline.dataRoot).toBe(absolute("work", "run-17", "protected", "data"));
    expect(layout.previousStable.dataRoot).not.toBe(layout.protectedBaseline.dataRoot);
    expect(layout.previousStable.checkout).not.toBe(layout.protectedBaseline.checkout);
    expect(layout.candidateBuildRoot).toBe(absolute("work", "run-17", "candidate-build"));
    expect(layout.candidateCheckout).toBe(absolute("work", "run-17", "candidate-checkout"));
    expect(layout.candidateCheckout).not.toBe(layout.previousStable.checkout);
    expect(layout.candidateCheckout).not.toBe(layout.protectedBaseline.checkout);
    expect(layout.updaterRoot).toBe(absolute("work", "run-17", "updater"));
  });

  it("pins real checkout manifests and lock entries for every build without changing the caller", async () => {
    const fixture = await versionFixture();
    try {
      for (const [name, version] of [
        ["candidate", "9.9.9-upgrade.1"],
        ["previous", "1.2.3"],
        ["protected", "1.2.3"],
      ] as const) {
        const input = await fixture.checkout(name, version);
        await prepareSeededUpgradeBuild(input);
        await expect(assertSeededUpgradeBuildVersion(input)).resolves.toBeUndefined();
        for (const relativePath of releasePackageFiles) {
          const manifest = JSON.parse(
            await NodeFS.promises.readFile(NodePath.join(input.checkout, relativePath), "utf8"),
          );
          expect(manifest.version).toBe(version);
        }
        for (const relativePath of releaseRustPackageFiles) {
          const manifest = parseToml(
            await NodeFS.promises.readFile(NodePath.join(input.checkout, relativePath), "utf8"),
          );
          expect((manifest.package as TomlTable).version).toBe(version);
        }
        const lock = parseToml(
          await NodeFS.promises.readFile(NodePath.join(input.checkout, "Cargo.lock"), "utf8"),
        );
        expect(
          (lock.package as TomlTable[])
            .filter((pkg) => ["coddy-studio-desktop", "coddy-studio-server"].includes(String(pkg.name)))
            .map((pkg) => [pkg.name, pkg.version]),
        ).toEqual([
          ["coddy-studio-desktop", version],
          ["coddy-studio-server", version],
        ]);
      }
      for (const [path, content] of fixture.originals) {
        expect(await NodeFS.promises.readFile(NodePath.join(repositoryRoot, path), "utf8")).toBe(
          content,
        );
      }
    } finally {
      await NodeFS.promises.rm(fixture.root, { recursive: true, force: true });
    }
  });

  it("refuses the calling checkout and its physical alias before rewriting versions", async () => {
    const fixture = await versionFixture();
    try {
      const input = await fixture.checkout("caller", "9.9.9-upgrade.1");
      const alias = NodePath.join(fixture.root, "caller-alias");
      await NodeFS.promises.symlink(input.checkout, alias, "junction");
      for (const checkout of [input.checkout, alias]) {
        await expect(
          prepareSeededUpgradeBuild({
            ...input,
            repositoryRoot: input.checkout,
            checkout,
          }),
        ).rejects.toThrow(/disposable checkout/);
      }
      for (const [path, content] of fixture.originals) {
        expect(await NodeFS.promises.readFile(NodePath.join(input.checkout, path), "utf8")).toBe(
          content,
        );
      }
    } finally {
      await NodeFS.promises.rm(fixture.root, { recursive: true, force: true });
    }
  });

  it("rejects an overlay mismatch before preparing a build", async () => {
    const fixture = await versionFixture();
    try {
      const input = await fixture.checkout("candidate", "9.9.9-upgrade.1");
      await NodeFS.promises.writeFile(input.overlayPath, '{"version":"0.0.1"}');
      await expect(prepareSeededUpgradeBuild(input)).rejects.toThrow(/overlay version/);
      for (const [path, content] of fixture.originals) {
        expect(await NodeFS.promises.readFile(NodePath.join(input.checkout, path), "utf8")).toBe(
          content,
        );
      }
    } finally {
      await NodeFS.promises.rm(fixture.root, { recursive: true, force: true });
    }
  });

  it("fails closed when release metadata or overlay drifts after preparation", async () => {
    const fixture = await versionFixture();
    try {
      const input = await fixture.checkout("candidate", "9.9.9-upgrade.1");
      await prepareSeededUpgradeBuild(input);
      for (const path of [
        ...releaseVersionFiles.map((relativePath) => NodePath.join(input.checkout, relativePath)),
        input.overlayPath,
      ]) {
        const content = await NodeFS.promises.readFile(path, "utf8");
        await NodeFS.promises.writeFile(path, content.replaceAll(input.version, "0.0.1"));
        await expect(assertSeededUpgradeBuildVersion(input)).rejects.toThrow(/version/);
        await NodeFS.promises.writeFile(path, content);
      }
    } finally {
      await NodeFS.promises.rm(fixture.root, { recursive: true, force: true });
    }
  });

  it("builds a deterministic public-key updater overlay without private key material", () => {
    const overlay = buildSeededUpgradeOverlay({
      endpoint: "http://127.0.0.1:4312/latest.json",
      identifier: "dev.coddy.studio.upgradesmoke.run-17",
      publicKey: "public-test-key",
      version: "0.3.11",
    });

    expect(overlay).toEqual({
      identifier: "dev.coddy.studio.upgradesmoke.run-17",
      version: "0.3.11",
      bundle: { createUpdaterArtifacts: true },
      plugins: {
        updater: {
          dangerousInsecureTransportProtocol: true,
          endpoints: ["http://127.0.0.1:4312/latest.json"],
          pubkey: "public-test-key",
        },
      },
    });
    expect(JSON.stringify(overlay)).not.toMatch(/private|password|signing/i);
  });

  it("requires every baseline package version to be strictly older than the candidate", () => {
    expect(() => assertBaselineVersionIsOlder("0.3.10", "0.3.11")).not.toThrow();
    expect(() => assertBaselineVersionIsOlder("0.3.11-beta.1", "0.3.11")).not.toThrow();
    expect(() => assertBaselineVersionIsOlder("0.3.11", "0.3.11")).toThrow(/strictly older/);
    expect(() => assertBaselineVersionIsOlder("0.4.0", "0.3.11")).toThrow(/strictly older/);
  });

  it("accepts nullable old identity only in the previous-stable compatibility lane", () => {
    const before = {
      appVersion: "0.3.10",
      effectiveRoot: absolute("work", "data"),
      projectId: "project-seeded",
      projectIds: ["project-seeded"],
      storageInstanceId: null,
    } as const;
    const after = {
      appVersion: "0.3.11",
      effectiveRoot: absolute("work", "data"),
      projectIds: ["project-seeded"],
      storageInstanceId: "8a8c318a-e2c3-4f78-a61c-3ba53b0a10af",
      preUpdateBackups: [],
    } as const;

    expect(() =>
      verifySeededUpgradeOutcome("previous-stable", before, after, "0.3.11"),
    ).not.toThrow();
    expect(() => verifySeededUpgradeOutcome("protected-baseline", before, after, "0.3.11")).toThrow(
      /baseline storage identity/,
    );
    expect(() =>
      verifySeededUpgradeOutcome(
        "previous-stable",
        before,
        { ...after, appVersion: "0.3.10" },
        "0.3.11",
      ),
    ).toThrow(/candidate application version/);
  });

  it("requires protected identity, project, effective root, and pre-update backup continuity", () => {
    const storageInstanceId = "8a8c318a-e2c3-4f78-a61c-3ba53b0a10af";
    const before = {
      appVersion: "0.3.10",
      effectiveRoot: absolute("work", "data"),
      projectId: "project-seeded",
      projectIds: ["project-seeded"],
      storageInstanceId,
    } as const;
    const after = {
      appVersion: "0.3.11",
      effectiveRoot: before.effectiveRoot,
      projectIds: ["project-seeded"],
      storageInstanceId,
      preUpdateBackups: [{ storageInstanceId, trigger: "pre-update" }],
    } as const;

    expect(() =>
      verifySeededUpgradeOutcome("protected-baseline", before, after, "0.3.11"),
    ).not.toThrow();
    expect(() =>
      verifySeededUpgradeOutcome(
        "protected-baseline",
        before,
        {
          ...after,
          storageInstanceId: "d0ac2738-e32b-4d7b-87bc-6ada325ee42e",
        },
        "0.3.11",
      ),
    ).toThrow(/storage identity changed/);
    expect(() =>
      verifySeededUpgradeOutcome(
        "protected-baseline",
        before,
        {
          ...after,
          projectIds: [],
        },
        "0.3.11",
      ),
    ).toThrow(/seeded project/);
    expect(() =>
      verifySeededUpgradeOutcome(
        "protected-baseline",
        before,
        {
          ...after,
          preUpdateBackups: [],
        },
        "0.3.11",
      ),
    ).toThrow(/pre-update backup/);
  });

  it("polls readiness and restart conditions under a deterministic timeout", async () => {
    let now = 0;
    let attempts = 0;
    await expect(
      waitForUpgradeCondition({
        description: "updater readiness",
        intervalMs: 10,
        now: () => now,
        probe: async () => ++attempts === 3,
        sleep: async (milliseconds) => {
          now += milliseconds;
        },
        timeoutMs: 50,
      }),
    ).resolves.toBeUndefined();
    expect(attempts).toBe(3);

    await expect(
      waitForUpgradeCondition({
        description: "candidate restart",
        intervalMs: 10,
        now: () => now,
        probe: async () => false,
        sleep: async (milliseconds) => {
          now += milliseconds;
        },
        timeoutMs: 20,
      }),
    ).rejects.toThrow(/candidate restart.*20ms/);
  });

  it("reports a missing remote evidence file without a filesystem crash", () => {
    const missing = remoteEvidenceReadFailure(
      Object.assign(new Error("ENOENT: no such file or directory, open 'remote-rpc.json'"), {
        code: "ENOENT",
      }),
    );
    expect(missing).toBeInstanceOf(Error);
    expect(missing?.message).toBe("The remote-install lane finished without remote-rpc.json.");
    expect(missing?.message.includes("no such file")).toBe(false);
    expect(remoteEvidenceReadFailure(new Error("invalid"))).toBeUndefined();
  });

  it("accepts a nonzero seed phase only after the updater install was issued", () => {
    expect(() =>
      assertWebDriverPhaseExit({
        exitCode: 1,
        installAttempted: false,
        lane: "previous-stable",
        phase: "seed-and-install",
      }),
    ).toThrow(/WebDriver phase exited/);
    expect(() =>
      assertWebDriverPhaseExit({
        exitCode: 1,
        installAttempted: true,
        lane: "previous-stable",
        phase: "seed-and-install",
      }),
    ).not.toThrow();
    expect(() =>
      assertWebDriverPhaseExit({
        exitCode: 1,
        installAttempted: true,
        lane: "previous-stable",
        phase: "verify",
      }),
    ).toThrow(/WebDriver phase exited/);
  });

  it("cleans every started process in reverse order even when one cleanup fails", async () => {
    const calls: string[] = [];
    const registry = new ManagedProcessRegistry();
    registry.add("updater", async () => {
      calls.push("updater");
    });
    registry.add("baseline", async () => {
      calls.push("baseline");
      throw new Error("already exited");
    });
    registry.add("driver", async () => {
      calls.push("driver");
    });

    await expect(registry.cleanup()).rejects.toThrow(/baseline/);
    expect(calls).toEqual(["driver", "baseline", "updater"]);
    await expect(registry.cleanup()).resolves.toBeUndefined();
  });

  it("reaps a timed-out command before rejecting its caller", async () => {
    const root = await NodeFS.promises.mkdtemp(NodePath.join(NodeOS.tmpdir(), "coddy-studio-command-"));
    const pidPath = NodePath.join(root, "pid.txt");
    try {
      const command = process.execPath;
      const pendingCommand = runBoundedCommand({
        command,
        args: [
          "-e",
          `require("node:fs").writeFileSync(${JSON.stringify(pidPath)}, String(process.pid)); setInterval(() => {}, 1000);`,
        ],
        cwd: root,
        // Node startup can be delayed while the full release graph is
        // compiling Rust targets. Give the child time to publish its PID;
        // runBoundedCommand still owns the timeout and reap assertion.
        timeoutMs: 2_000,
      });
      await expect(pendingCommand).rejects.toBeInstanceOf(SeededUpgradeCommandTimeoutError);
      await expect(pendingCommand).rejects.toThrow(/timed out/);

      const pid = Number(await NodeFS.promises.readFile(pidPath, "utf8"));
      let alive = true;
      try {
        process.kill(pid, 0);
      } catch {
        alive = false;
      }
      expect(alive).toBe(false);
    } finally {
      await NodeFS.promises.rm(root, { recursive: true, force: true });
    }
  });

  it("retains late stdout and waits for close after the normal command exits", async () => {
    const root = await NodeFS.promises.mkdtemp(
      NodePath.join(NodeOS.tmpdir(), "coddy-studio-command-close-"),
    );
    const finishedPath = NodePath.join(root, "writer-finished");
    const writerSource = `
      setTimeout(() => {
        process.stdout.write("late-tail");
        require("node:fs").writeFileSync(${JSON.stringify(finishedPath)}, "finished");
      }, 125);
    `;
    const parentSource = `
      const writer = require("node:child_process").spawn(
        process.execPath,
        ["-e", ${JSON.stringify(writerSource)}],
        { stdio: ["ignore", "inherit", "inherit"] }
      );
      writer.once("spawn", () => {
        process.stdout.write("early|");
        process.exit(0);
      });
    `;
    try {
      const result = await runBoundedCommand({
        command: process.execPath,
        args: ["-e", parentSource],
        cwd: root,
        timeoutMs: 2_000,
      });
      expect(result.exitCode).toBe(0);
      expect(result.stdout).toBe("early|late-tail");
      expect(NodeFS.existsSync(finishedPath)).toBe(true);
    } finally {
      // Even the old exit-only counterexample owns its bounded writer until
      // completion, before removing the directory that writer uses.
      await waitForUpgradeCondition({
        description: "synthetic writer completion",
        intervalMs: 10,
        timeoutMs: 2_000,
        probe: async () => NodeFS.existsSync(finishedPath),
      });
      await NodeFS.promises.rm(root, { recursive: true, force: true });
    }
  });

  it.each(["raw", "truncated", "json", "slash-escaped", "unicode-slash", "encoded-token"] as const)(
    "redacts %s signed transfer labels before evidence bounds",
    (kind) => {
      const capability = "capability-prefix-" + "private".repeat(60);
      const full = "HTTP POST /api/transfers/" + capability;
      const truncated = full.slice(0, 159) + "…";
      const label =
        kind === "raw"
          ? full
          : kind === "json"
            ? JSON.stringify({ operation: truncated })
            : kind === "slash-escaped"
              ? truncated.replaceAll("/", "\\/")
              : kind === "unicode-slash"
                ? truncated.replaceAll("/", "\\u002f")
                : kind === "encoded-token"
                  ? truncated.replace("capability", "\\u0063apability")
                  : truncated;
      const evidence = redactAndBoundUpgradeEvidence(label, {
        maxBytes: 512,
        roots: [],
        secrets: [capability],
      });
      expect(evidence).not.toContain("capability-prefix");
      expect(evidence).not.toContain("privateprivate");
      expect(evidence).toContain("[REDACTED]");
    },
  );

  it("redacts secrets and roots before bounding retained failure evidence", () => {
    const privateKey = "PRIVATE-TEST-KEY-MATERIAL";
    const root = absolute("work", "run-17", "protected", "data");
    const evidence = redactAndBoundUpgradeEvidence(
      `token=bootstrap-secret\nkey=${privateKey}\nroot=${root}\n${"x".repeat(500)}`,
      {
        maxBytes: 140,
        roots: [root],
        secrets: ["bootstrap-secret", privateKey],
      },
    );

    expect(evidence).not.toContain("bootstrap-secret");
    expect(evidence).not.toContain(privateKey);
    expect(evidence).not.toContain(root);
    expect(evidence).toContain("[REDACTED]");
    expect(Buffer.byteLength(evidence)).toBeLessThanOrEqual(140);
  });

  it("builds a single-platform local manifest from the signed candidate payload", () => {
    expect(
      buildLocalUpdaterManifest({
        artifact: "Coddy Studio_0.3.11_x64-setup.exe",
        baseUrl: "http://127.0.0.1:4312/",
        candidateVersion: "0.3.11",
        signature: "encoded-minisign-signature",
        target: "windows-x86_64",
      }),
    ).toEqual({
      version: "0.3.11",
      notes: "Coddy Studio seeded packaged-upgrade smoke",
      pub_date: "2026-01-01T00:00:00Z",
      platforms: {
        "windows-x86_64": {
          signature: "encoded-minisign-signature",
          url: "http://127.0.0.1:4312/Coddy Studio_0.3.11_x64-setup.exe",
        },
      },
    });
  });

  it("generates public-boundary WebDriver phases without SQLite or direct store reads", () => {
    const seed = createSeededUpgradeDriverSpec({
      candidateVersion: "0.3.11",
      expectedDataRoot: absolute("work", "data"),
      lane: "protected-baseline",
      phase: "seed-and-install",
      projectId: "seeded-project",
      resultPath: absolute("work", "before.json"),
      workspaceRoot: absolute("work", "workspace"),
    });
    const verify = createSeededUpgradeDriverSpec({
      candidateVersion: "0.3.11",
      expectedDataRoot: absolute("work", "data"),
      lane: "protected-baseline",
      phase: "verify",
      projectId: "seeded-project",
      resultPath: absolute("work", "after.json"),
      workspaceRoot: absolute("work", "workspace"),
    });
    const combined = `${seed}\n${verify}`;

    expect(combined).toContain("The packaged desktop bridge did not become ready.");
    expect(combined).toContain("browser.waitUntil");
    expect(combined).toContain("getLocalEnvironmentBootstraps");
    expect(combined).toContain("getLocalEnvironmentBearerToken");
    expect(combined).toContain("/.well-known/coddy-studio/environment");
    expect(combined).toContain("orchestration.dispatchCommand");
    expect(combined).toContain("project.create");
    expect(combined).toContain("orchestration.subscribeShell");
    expect(combined).toContain("Load failed");
    expect(verify).toContain('request("orchestration.subscribeShell", {}, true)');
    const startupRetry =
      /Load failed|environment descriptor request failed|Timed out opening RPC|RPC failed|Timed out waiting for orchestration\.subscribeShell/;
    expect(verify).toContain(startupRetry.source);
    expect(
      startupRetry.test("WebDriverError: Load failed when running execute/sync with method POST"),
    ).toBe(true);
    expect(
      startupRetry.test(
        "WebDriverError: Timed out waiting for orchestration.subscribeShell. when running execute/sync",
      ),
    ).toBe(true);
    expect(
      startupRetry.test("The candidate application version was not running after update."),
    ).toBe(false);
    expect(seed).not.toContain("tasklist.exe");
    expect(seed).not.toContain("Get-Process");
    expect(seed).not.toContain("Get-CimInstance");
    expect(seed).not.toContain('state?.phase === "protecting") finish');
    expect(seed).not.toContain("setTimeout(resolve, 30000)");
    expect(combined).toContain("getProjectDataStatuses");
    expect(combined).toContain("getUpdateState");
    expect(seed).toContain("downloadUpdate");
    expect(seed).toContain("installUpdate");
    expect(seed.indexOf("checkForUpdate")).toBeLessThan(seed.indexOf("downloadUpdate"));
    expect(seed).toContain("installAttempted: true");
    expect(verify).not.toContain("downloadUpdate()");
    expect(combined).not.toMatch(/sqlite|state\.sqlite|better-sqlite|rusqlite/i);
  });

  it("switches to WSL-only through the desktop bridge and requires a running WSL primary", () => {
    const spec = createSeededUpgradeDriverSpec({
      candidateVersion: "0.3.11",
      expectedDataRoot: absolute("work", "data"),
      lane: "protected-baseline",
      phase: "seed-and-install",
      projectId: "seeded-wsl-project",
      resultPath: absolute("work", "before.json"),
      workspaceRoot: "/tmp/coddy-studio-seeded-wsl-project",
      wsl: true,
    });

    expect(spec).toContain("setWslOnly(true)");
    expect(spec).toContain("setWslBackendEnabled(true)");
    expect(spec).toContain("runningDistro");
    expect(spec).toContain("getLocalEnvironmentBootstraps");
  });

  it("normalizes WebDriver requests for the embedded Tauri service", () => {
    const config = createSeededUpgradeWdioConfig({
      appBinaryPath: absolute("installed", "coddy-studio-desktop"),
      artifactDirectory: absolute("evidence"),
      restartTimeoutMs: 120_000,
      specPath: absolute("driver", "seeded-upgrade.e2e.ts"),
      webdriverPort: 44_450,
    });

    expect(config).toContain("transformRequest");
    expect(config).toContain('headers.delete("content-length")');
    expect(config).toContain("captureBackendLogs: true");
  });
});

describe("windowedApplicationRunning", () => {
  const row = (image: string, title: string) =>
    `"${image}","4242","Console","1","120,000 K","Running","host\\runner","0:00:03","${title}"`;

  it("finds the relaunched app by its window and ignores windowless helpers", () => {
    expect(
      windowedApplicationRunning(
        [row("coddy-studio-desktop.exe", "N/A"), row("coddy-studio-desktop.exe", "Coddy Studio")].join("\r\n"),
        "coddy-studio-desktop.exe",
      ),
    ).toBe(true);
    expect(
      windowedApplicationRunning(row("coddy-studio-desktop.exe", "N/A"), "coddy-studio-desktop.exe"),
    ).toBe(false);
    expect(windowedApplicationRunning(row("other.exe", "Coddy Studio"), "coddy-studio-desktop.exe")).toBe(
      false,
    );
    expect(
      windowedApplicationRunning(
        "INFO: No tasks are running which match the specified criteria.",
        "coddy-studio-desktop.exe",
      ),
    ).toBe(false);
  });
});

describe("waitForWindowsRelaunchedApplication", () => {
  it("treats a timed-out process listing as one more sample", async () => {
    let calls = 0;
    let clock = 0;
    await waitForWindowsRelaunchedApplication({
      appBinaryPath: "C:\\Program Files\\Coddy Studio\\coddy-studio-desktop.exe",
      timeoutMs: 10_000,
      now: () => clock,
      sleep: async (milliseconds) => {
        clock += milliseconds;
      },
      listProcesses: async () => {
        calls += 1;
        if (calls === 1) throw Object.assign(new Error("timed out"), { killed: true });
        return `"coddy-studio-desktop.exe","1","Console","1","1 K","Running","u","0:00:01","Coddy Studio"`;
      },
    });
    expect(calls).toBe(2);
  });

  it("still fails when the listing cannot run at all", async () => {
    await expect(
      waitForWindowsRelaunchedApplication({
        appBinaryPath: "coddy-studio-desktop.exe",
        timeoutMs: 10_000,
        listProcesses: async () => {
          throw Object.assign(new Error("spawn tasklist.exe ENOENT"), { code: "ENOENT" });
        },
      }),
    ).rejects.toThrow("ENOENT");
  });
});
