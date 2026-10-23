import * as NodeURL from "node:url";

import * as NodeServices from "@effect/platform-node/NodeServices";
import { AlchemyContext } from "alchemy/AlchemyContext";
import { ArtifactStore, createArtifactStore } from "alchemy/Artifacts";
import { AuthProviders } from "alchemy/Auth/AuthProvider";
import * as Interaction from "alchemy/Interaction";
import { Cli } from "alchemy/Report";
import * as InMemoryState from "alchemy/State/InMemoryState";
import * as State from "alchemy/State/State";
import { afterEach, beforeEach, describe, expect, it } from "@effect/vitest";
import * as Data from "effect/Data";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as PlatformError from "effect/PlatformError";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as TestConsole from "effect/testing/TestConsole";
import * as ChildProcess from "effect/process/ChildProcess";
import * as ChildProcessSpawner from "effect/process/ChildProcessSpawner";
import * as FetchHttpClient from "effect/http/FetchHttpClient";

class TestDeployFailure extends Data.TaggedError("TestDeployFailure")<{
  readonly message: string;
}> {}

import {
  deploy,
  hasDeployChanges,
  missingRelayPublicConfigFields,
  publicConfigFromOutput,
  reconcileRootEnvRelayUrl,
  RelayDeployError,
  RelayDeployOperations,
  serializeGithubOutput,
  type RelayDeployOptions,
  type RelayProvisioningRequirements,
} from "./deploy.ts";

const pathSeparator = NodeURL.fileURLToPath(new URL(".", import.meta.url)).includes("\\")
  ? "\\"
  : "/";
const pathFromUrl = (url: URL) => NodeURL.fileURLToPath(url).replace(/[\\/]+$/u, "");
const joinPath = (...segments: ReadonlyArray<string>) => segments.join(pathSeparator);
const relayRoot = pathFromUrl(new URL("..", import.meta.url));
const repoRoot = pathFromUrl(new URL("../../..", import.meta.url));
const relayDefaultEnvPath = joinPath(relayRoot, ".env");
const rootEnvPath = joinPath(repoRoot, ".env");
const deployScriptPath = pathFromUrl(new URL("./deploy.ts", import.meta.url));
const deployScriptUrl = NodeURL.pathToFileURL(deployScriptPath).href;
const decodeUnknownJson = Schema.decodeUnknownSync(Schema.fromJsonString(Schema.Unknown));

const coverageResults = (value: unknown): ReadonlyArray<unknown> => {
  if (typeof value !== "object" || value === null || !("result" in value)) {
    return [];
  }
  return Array.isArray(value.result) ? value.result : [];
};

const collectStreamAsString = <E>(stream: Stream.Stream<Uint8Array, E>): Effect.Effect<string, E> =>
  stream.pipe(
    Stream.decodeText(),
    Stream.runFold(
      () => "",
      (contents, chunk) => contents + chunk,
    ),
  );

const runDeployCli = Effect.fn("test.runDeployCli")(function* (args: ReadonlyArray<string>) {
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const fs = yield* FileSystem.FileSystem;
  const coverageDirectory = yield* fs.makeTempDirectory({ prefix: "relay-deploy-v8-" });
  return yield* Effect.gen(function* () {
    const child = yield* spawner.spawn(
      ChildProcess.make(process.execPath, ["--", deployScriptPath, ...args], {
        cwd: repoRoot,
        env: { NODE_V8_COVERAGE: coverageDirectory },
        extendEnv: true,
      }),
    );
    const [stdout, stderr, exitCode] = yield* Effect.all(
      [collectStreamAsString(child.stdout), collectStreamAsString(child.stderr), child.exitCode],
      { concurrency: "unbounded" },
    );
    let coveredDeployScript = false;
    for (const filename of yield* fs.readDirectory(coverageDirectory)) {
      const contents = yield* fs.readFileString(joinPath(coverageDirectory, filename));
      const coverage = decodeUnknownJson(contents);
      coveredDeployScript ||= coverageResults(coverage).some(
        (entry) =>
          typeof entry === "object" &&
          entry !== null &&
          "url" in entry &&
          entry.url === deployScriptUrl,
      );
    }
    return { stdout, stderr, exitCode: Number(exitCode), coveredDeployScript } as const;
  }).pipe(
    Effect.ensuring(
      fs.remove(coverageDirectory, { recursive: true }).pipe(Effect.orElseSucceed(() => undefined)),
    ),
  );
});

const completeRelayOutput = () => ({ url: "https://relay.example.test" });

const deployHarness = {
  applyCount: 0,
  applyError: undefined as TestDeployFailure | undefined,
  applyOutput: completeRelayOutput() as unknown,
  displayedPlanCount: 0,
  plan: {
    deletions: {},
    resources: { relay: { action: "create", bindings: [] } },
  } as unknown,
  planForces: [] as Array<boolean>,
  promptApproved: true,
  promptMessages: [] as Array<string>,
  provisionRequests: [] as Array<RelayProvisioningRequirements>,
  stateOutput: completeRelayOutput() as unknown,
  stateRequests: [] as Array<{ readonly stack: string; readonly stage: string }>,
};

const deployOptions = (overrides: Partial<RelayDeployOptions> = {}): RelayDeployOptions => ({
  adopt: false,
  dryRun: false,
  envFile: Option.none(),
  force: false,
  githubOutput: false,
  readState: false,
  stage: Option.none(),
  yes: false,
  ...overrides,
});

const virtualFileSystem = (
  initialFiles: Readonly<Record<string, string>> = {},
  writeFailure?: TestDeployFailure,
  readFailures: Readonly<Record<string, TestDeployFailure>> = {},
  realPaths: Readonly<Record<string, string>> = {},
  writeFailures: Readonly<Record<string, TestDeployFailure>> = {},
  interruptRenameTargets: ReadonlySet<string> = new Set(),
  partialCommitFailures: Readonly<
    Record<string, { readonly contentsAfterFailure: string; readonly error: TestDeployFailure }>
  > = {},
) => {
  const files = new Map(Object.entries(initialFiles));
  const pendingPartialCommitFailures = new Map(Object.entries(partialCommitFailures));
  const pendingInterruptRenameTargets = new Set(interruptRenameTargets);
  const writes: Array<{ readonly path: string; readonly value: string }> = [];
  let tempFileIndex = 0;
  const fileSystem = FileSystem.makeNoop({
    exists: (path) => Effect.succeed(files.has(path)),
    readFileString: (path) => {
      const readFailure = readFailures[path];
      if (readFailure !== undefined) {
        return Effect.fail(readFailure) as never;
      }
      const contents = files.get(path);
      return contents === undefined
        ? Effect.fail(
            PlatformError.systemError({
              _tag: "NotFound",
              method: "readFileString",
              module: "FileSystem",
              pathOrDescriptor: path,
            }),
          )
        : Effect.succeed(contents);
    },
    realPath: (path) => Effect.succeed(realPaths[path] ?? path),
    makeTempFile: (options) =>
      Effect.sync(() => {
        const tempPath = joinPath(
          options?.directory ?? repoRoot,
          `${options?.prefix ?? "tmp-"}${tempFileIndex++}${options?.suffix ?? ""}`,
        );
        files.set(tempPath, "");
        return tempPath;
      }),
    remove: (path) =>
      Effect.sync(() => {
        files.delete(path);
      }),
    rename: (oldPath, newPath) => {
      const partialFailure = pendingPartialCommitFailures.get(newPath);
      if (partialFailure !== undefined) {
        pendingPartialCommitFailures.delete(newPath);
        return Effect.gen(function* () {
          files.set(newPath, partialFailure.contentsAfterFailure);
          return yield* partialFailure.error;
        }) as never;
      }
      if (pendingInterruptRenameTargets.has(newPath)) {
        pendingInterruptRenameTargets.delete(newPath);
        return Effect.interrupt as never;
      }
      const contents = files.get(oldPath);
      return contents === undefined
        ? Effect.fail(
            PlatformError.systemError({
              _tag: "NotFound",
              method: "rename",
              module: "FileSystem",
              pathOrDescriptor: oldPath,
            }),
          )
        : Effect.sync(() => {
            files.set(newPath, contents);
            files.delete(oldPath);
          });
    },
    writeFileString: (path, value, options) => {
      const partialFailure = pendingPartialCommitFailures.get(path);
      if (partialFailure !== undefined) {
        pendingPartialCommitFailures.delete(path);
        return Effect.gen(function* () {
          files.set(path, partialFailure.contentsAfterFailure);
          return yield* partialFailure.error;
        }) as never;
      }
      const pathWriteFailure = writeFailures[path] ?? writeFailure;
      if (pathWriteFailure !== undefined) {
        return Effect.fail(pathWriteFailure) as never;
      }
      return Effect.sync(() => {
        const next = options?.flag === "a" ? `${files.get(path) ?? ""}${value}` : value;
        files.set(path, next);
        writes.push({ path, value });
      });
    },
  });

  return {
    files,
    writes,
    layer: FileSystem.layerNoop(fileSystem),
  };
};

const testDeployOperations = (): RelayDeployOperations["Service"] =>
  RelayDeployOperations.of({
    apply: (() =>
      Effect.suspend(() => {
        deployHarness.applyCount += 1;
        return deployHarness.applyError === undefined
          ? Effect.succeed(deployHarness.applyOutput)
          : Effect.fail(deployHarness.applyError);
      })) as unknown as RelayDeployOperations["Service"]["apply"],
    cli: Effect.succeed({
      approvePlan: () => Effect.succeed(deployHarness.promptApproved),
      displayPlan: () =>
        Effect.sync(() => {
          deployHarness.displayedPlanCount += 1;
        }),
      startApplySession: () => Effect.die("not used by relay deploy tests"),
      startPlanningSession: () => Effect.die("not used by relay deploy tests"),
    }),
    confirm: ((options: { readonly message: string }) =>
      options) as unknown as RelayDeployOperations["Service"]["confirm"],
    makePlan: ((_stack: unknown, options: { readonly force?: boolean }) =>
      Effect.sync(() => {
        deployHarness.planForces.push(options.force ?? false);
        return deployHarness.plan;
      })) as RelayDeployOperations["Service"]["makePlan"],
    runPrompt: ((prompt: unknown) =>
      Effect.sync(() => {
        if (
          typeof prompt === "object" &&
          prompt !== null &&
          "message" in prompt &&
          typeof prompt.message === "string"
        ) {
          deployHarness.promptMessages.push(prompt.message);
        }
        return deployHarness.promptApproved;
      })) as RelayDeployOperations["Service"]["runPrompt"],
    stack: (requirements) => {
      deployHarness.provisionRequests.push(requirements);
      return Effect.succeed({
        actions: {},
        bindings: {},
        name: "BiBCodeRelay",
        output: {},
        resources: {},
        services: Layer.empty,
        stage: "test",
      } as never);
    },
    state: (() =>
      Layer.succeed(
        State.State,
        InMemoryState.InMemoryService().pipe(
          Effect.map((service) => ({
            ...service,
            getOutput: (request: { readonly stack: string; readonly stage: string }) =>
              Effect.sync(() => {
                deployHarness.stateRequests.push(request);
                return deployHarness.stateOutput;
              }),
          })),
        ),
      )) as RelayDeployOperations["Service"]["state"],
  });

const runDeploy = (
  options: RelayDeployOptions,
  fileSystem: ReturnType<typeof virtualFileSystem>,
  operations: RelayDeployOperations["Service"] = testDeployOperations(),
) =>
  deploy(options).pipe(
    Effect.provide(
      Layer.mergeAll(
        NodeServices.layer,
        FetchHttpClient.layer,
        Layer.succeed(AuthProviders, {}),
        Layer.succeed(ArtifactStore, createArtifactStore()),
        Layer.succeed(RelayDeployOperations, operations),
        Layer.succeed(Cli, {
          approvePlan: () => Effect.succeed(true),
          displayPlan: () => Effect.void,
          startApplySession: () => Effect.die("not used"),
          startPlanningSession: () => Effect.die("not used"),
        }),
        Interaction.layerNonInteractive(),
        Layer.succeed(AlchemyContext, {
          adopt: false,
          dev: false,
          dotAlchemy: joinPath(repoRoot, ".alchemy-test"),
        }),
        fileSystem.layer,
      ),
    ),
    Effect.scoped,
  );

const environmentBeforeTest = {
  GITHUB_OUTPUT: process.env.GITHUB_OUTPUT,
  USER: process.env.USER,
  USERNAME: process.env.USERNAME,
  stage: process.env.stage,
};

const restoreEnvironment = () => {
  for (const [name, value] of Object.entries(environmentBeforeTest)) {
    if (value === undefined) {
      delete process.env[name];
    } else {
      process.env[name] = value;
    }
  }
};

beforeEach(() => {
  deployHarness.applyCount = 0;
  deployHarness.applyError = undefined;
  deployHarness.applyOutput = completeRelayOutput();
  deployHarness.displayedPlanCount = 0;
  deployHarness.plan = {
    deletions: {},
    resources: { relay: { action: "create", bindings: [] } },
  };
  deployHarness.planForces = [];
  deployHarness.promptApproved = true;
  deployHarness.promptMessages = [];
  deployHarness.provisionRequests = [];
  deployHarness.stateOutput = completeRelayOutput();
  deployHarness.stateRequests = [];
});

afterEach(() => {
  restoreEnvironment();
});

describe("RelayDeployError", () => {
  it("reports the incomplete state source, stage, and missing fields", () => {
    const missingFields = missingRelayPublicConfigFields({});
    const error = new RelayDeployError({
      source: "alchemy_state",
      stage: "production",
      missingFields,
    });

    expect(error).toMatchObject({
      source: "alchemy_state",
      stage: "production",
      missingFields: ["url"],
    });
    expect(error.message).toBe(
      "Relay deploy output from 'alchemy_state' for stage 'production' is missing required public config fields: url",
    );
  });
});

describe("hasDeployChanges", () => {
  it("detects resource, binding, and deletion changes", () => {
    expect(hasDeployChanges({ resources: {}, deletions: {} } as never)).toBe(false);
    expect(
      hasDeployChanges({
        resources: {
          api: { action: "create", bindings: [] },
        },
        deletions: {},
      } as never),
    ).toBe(true);
    expect(
      hasDeployChanges({
        resources: {
          api: { action: "noop", bindings: [{ action: "update" }] },
        },
        deletions: {},
      } as never),
    ).toBe(true);
    expect(
      hasDeployChanges({
        resources: {},
        deletions: {
          api: { action: "delete", bindings: [] },
        },
      } as never),
    ).toBe(true);
  });
});

describe("reconcileRootEnvRelayUrl", () => {
  it("adds the relay URL to an empty root env file", () => {
    expect(reconcileRootEnvRelayUrl("", "https://relay.example.test")).toBe(
      "BIBCODE_RELAY_URL=https://relay.example.test\n",
    );
  });

  it("preserves unrelated root env entries while replacing a previous relay URL", () => {
    expect(
      reconcileRootEnvRelayUrl(
        "BIBCODE_CLERK_PUBLISHABLE_KEY=pk_test_example\nBIBCODE_RELAY_URL=https://old.example.test\n",
        "https://relay.example.test",
      ),
    ).toBe(
      "BIBCODE_CLERK_PUBLISHABLE_KEY=pk_test_example\nBIBCODE_RELAY_URL=https://relay.example.test\n",
    );
  });

  it("rejects line-break injection in root environment values", () => {
    expect(() =>
      reconcileRootEnvRelayUrl(
        "",
        "https://relay.example.test\nBIBCODE_RELAY_URL=https://unexpected.example.test",
      ),
    ).toThrow("Relay deployment environment values cannot contain line breaks.");
  });
});

describe("serializeGithubOutput", () => {
  it("serializes relay deploy metadata for GitHub Actions outputs", () => {
    expect(
      serializeGithubOutput({
        changed: false,
        result: "noop",
        relay_url: "https://relay.example.test",
      }),
    ).toBe("changed=false\nresult=noop\nrelay_url=https://relay.example.test\n");
  });

  it("rejects line-break injection in GitHub command-file values", () => {
    expect(() =>
      serializeGithubOutput({
        relay_url: "https://relay.example.test\nresult=exfiltrated",
      }),
    ).toThrow("GitHub output values cannot contain line breaks.");
  });

  it("rejects invalid GitHub command-file keys", () => {
    expect(() => serializeGithubOutput({ "safe\nINJECTED": "value" })).toThrow(
      "GitHub output keys must be portable environment identifiers.",
    );
    expect(() => serializeGithubOutput({ "unsafe-key": "value" })).toThrow(
      "GitHub output keys must be portable environment identifiers.",
    );
  });
});

describe("deploy", () => {
  it.effect("passes complete provisioning requirements through one deployment boundary", () =>
    Effect.gen(function* () {
      const fileSystem = virtualFileSystem({
        [relayDefaultEnvPath]: "stage=boundary-stage\n",
      });

      yield* runDeploy(
        deployOptions({
          adopt: true,
          dryRun: true,
          force: true,
          yes: false,
        }),
        fileSystem,
      );

      expect(deployHarness.provisionRequests).toHaveLength(1);
      expect(deployHarness.provisionRequests[0]!).toMatchObject({
        adopt: true,
        dryRun: true,
        force: true,
        stage: "boundary-stage",
        yes: false,
      });
      expect("configProvider" in deployHarness.provisionRequests[0]!).toBe(true);
      expect(deployHarness.planForces).toEqual([true]);
      expect(deployHarness.displayedPlanCount).toBe(1);
      expect(deployHarness.promptMessages).toEqual([]);
      expect(deployHarness.applyCount).toBe(0);
    }),
  );

  it.effect("runs injected provisioning cleanup on interruption before file staging", () =>
    Effect.gen(function* () {
      let cleaned = false;
      const baseOperations = testDeployOperations();
      const operations = RelayDeployOperations.of({
        ...baseOperations,
        stack: (requirements) =>
          Effect.acquireUseRelease(
            Effect.sync(() => {
              deployHarness.provisionRequests.push(requirements);
            }),
            () => Effect.interrupt,
            () =>
              Effect.sync(() => {
                cleaned = true;
              }),
          ),
      });
      const fileSystem = virtualFileSystem({
        [relayDefaultEnvPath]: "stage=interrupt-boundary-stage\n",
        [rootEnvPath]: "ORIGINAL=value\n",
      });

      const exit = yield* Effect.exit(
        runDeploy(deployOptions({ yes: true }), fileSystem, operations),
      );

      expect(Exit.isFailure(exit)).toBe(true);
      expect(cleaned).toBe(true);
      expect(deployHarness.provisionRequests).toHaveLength(1);
      expect(deployHarness.planForces).toEqual([]);
      expect(fileSystem.files.get(rootEnvPath)).toBe("ORIGINAL=value\n");
      expect([...fileSystem.files.keys()].some((path) => path.includes(".relay-"))).toBe(false);
    }),
  );

  it.effect("loads the explicit dotenv override before resolving the deployment stage", () =>
    Effect.gen(function* () {
      const fileSystem = virtualFileSystem({
        [joinPath(relayRoot, "override.env")]: "stage=override-stage\n",
        [relayDefaultEnvPath]: "stage=default-stage\n",
      });

      yield* runDeploy(
        deployOptions({
          envFile: Option.some("override.env"),
          readState: true,
        }),
        fileSystem,
      );

      expect(deployHarness.stateRequests).toEqual([
        { stack: "BiBCodeRelay", stage: "override-stage" },
      ]);
      expect(fileSystem.files.get(rootEnvPath)).toContain(
        "BIBCODE_RELAY_URL=https://relay.example.test",
      );
    }),
  );

  it.effect("rejects unsafe env-file path syntax before reading configuration", () =>
    Effect.gen(function* () {
      for (const envFile of [
        "../outside.env",
        joinPath(repoRoot, "outside.env"),
        "C:\\outside.env",
        "NUL.env",
        "deploy.env:secret",
      ]) {
        const error = yield* Effect.flip(
          runDeploy(
            deployOptions({ envFile: Option.some(envFile), readState: true }),
            virtualFileSystem(),
          ),
        );
        expect(error.message).toContain("Relay env file path must stay within the relay root");
      }
      expect(deployHarness.stateRequests).toEqual([]);
    }),
  );

  it.effect("rejects an env-file symlink that resolves outside the relay root", () =>
    Effect.gen(function* () {
      const envFilePath = joinPath(relayRoot, "linked.env");
      const fileSystem = virtualFileSystem(
        { [envFilePath]: "stage=escaped-stage\n" },
        undefined,
        {},
        { [envFilePath]: joinPath(relayRoot, "..") },
      );

      const error = yield* Effect.flip(
        runDeploy(
          deployOptions({ envFile: Option.some("linked.env"), readState: true }),
          fileSystem,
        ),
      );

      expect(error.message).toContain("Relay env file path must stay within the relay root");
      expect(deployHarness.stateRequests).toEqual([]);
    }),
  );

  it.effect("uses the default relay dotenv file when no override is supplied", () =>
    Effect.gen(function* () {
      const fileSystem = virtualFileSystem({
        [relayDefaultEnvPath]: "stage=default-stage\n",
      });

      yield* runDeploy(deployOptions({ readState: true }), fileSystem);

      expect(deployHarness.stateRequests).toEqual([
        { stack: "BiBCodeRelay", stage: "default-stage" },
      ]);
    }),
  );

  it.effect("falls back to the process environment when the default dotenv file is absent", () =>
    Effect.gen(function* () {
      process.env.stage = "environment-stage";
      const fileSystem = virtualFileSystem();

      yield* runDeploy(deployOptions({ readState: true }), fileSystem);

      expect(deployHarness.stateRequests).toEqual([
        { stack: "BiBCodeRelay", stage: "environment-stage" },
      ]);
    }),
  );

  it.effect("propagates default dotenv read failures instead of falling back", () =>
    Effect.gen(function* () {
      process.env.stage = "must-not-be-used";
      const readFailure = new TestDeployFailure({ message: "dotenv permission denied" });
      const fileSystem = virtualFileSystem({}, undefined, {
        [relayDefaultEnvPath]: readFailure,
      });

      expect(yield* Effect.flip(runDeploy(deployOptions({ readState: true }), fileSystem))).toBe(
        readFailure,
      );
      expect(deployHarness.stateRequests).toEqual([]);
    }),
  );

  it.effect("uses the explicit stage flag over dotenv configuration", () =>
    Effect.gen(function* () {
      const fileSystem = virtualFileSystem({
        [relayDefaultEnvPath]: "stage=dotenv-stage\n",
      });

      yield* runDeploy(
        deployOptions({
          readState: true,
          stage: Option.some("flag-stage"),
        }),
        fileSystem,
      );

      expect(deployHarness.stateRequests).toEqual([{ stack: "BiBCodeRelay", stage: "flag-stage" }]);
    }),
  );

  it.effect("uses the developer stage when no dotenv or environment stage is configured", () =>
    Effect.gen(function* () {
      delete process.env.stage;
      process.env.USER = "relay-tester";
      delete process.env.USERNAME;
      const fileSystem = virtualFileSystem();

      yield* runDeploy(deployOptions({ readState: true }), fileSystem);

      expect(deployHarness.stateRequests).toEqual([
        { stack: "BiBCodeRelay", stage: "dev_relay-tester" },
      ]);
    }),
  );

  it.effect("falls back from USER to USERNAME and then unknown for developer stages", () =>
    Effect.gen(function* () {
      delete process.env.stage;
      delete process.env.USER;
      process.env.USERNAME = "windows-relay-tester";

      yield* runDeploy(deployOptions({ readState: true }), virtualFileSystem());

      delete process.env.USERNAME;
      yield* runDeploy(deployOptions({ readState: true }), virtualFileSystem());

      expect(deployHarness.stateRequests).toEqual([
        { stack: "BiBCodeRelay", stage: "dev_windows-relay-tester" },
        { stack: "BiBCodeRelay", stage: "dev_unknown" },
      ]);
    }),
  );

  it.effect("displays a dry-run plan and appends a public outcome without applying", () =>
    Effect.gen(function* () {
      const githubOutputPath = joinPath(repoRoot, "github-output");
      process.env.GITHUB_OUTPUT = githubOutputPath;
      const fileSystem = virtualFileSystem({
        [joinPath(relayRoot, "deploy.env")]: "stage=dry-run-stage\n",
        [githubOutputPath]: "prior=true\n",
      });

      yield* runDeploy(
        deployOptions({
          dryRun: true,
          envFile: Option.some("deploy.env"),
          githubOutput: true,
        }),
        fileSystem,
      );

      expect(deployHarness.displayedPlanCount).toBe(1);
      expect(deployHarness.applyCount).toBe(0);
      expect(fileSystem.files.has(rootEnvPath)).toBe(false);
      expect(fileSystem.files.get(githubOutputPath)).toBe(
        "prior=true\nchanged=true\nresult=dry-run\n",
      );
    }),
  );

  it.effect("cancels an unapproved changed plan without mutating local relay configuration", () =>
    Effect.gen(function* () {
      deployHarness.promptApproved = false;
      const fileSystem = virtualFileSystem({
        [relayDefaultEnvPath]: "stage=cancel-stage\n",
      });

      yield* runDeploy(deployOptions(), fileSystem);

      expect(deployHarness.displayedPlanCount).toBe(1);
      expect(deployHarness.promptMessages).toEqual(["Apply this relay deployment?"]);
      expect(deployHarness.applyCount).toBe(0);
      expect(fileSystem.files.has(rootEnvPath)).toBe(false);
      expect(yield* TestConsole.logLines).toContain("Deployment cancelled.");
    }),
  );

  it.effect("applies an approved changed plan and writes the relay URL outputs", () =>
    Effect.gen(function* () {
      const githubOutputPath = joinPath(repoRoot, "github-output");
      process.env.GITHUB_OUTPUT = githubOutputPath;
      const fileSystem = virtualFileSystem({
        [githubOutputPath]: "prior=true\n",
        [joinPath(relayRoot, "deploy.env")]: "stage=apply-stage\n",
        [rootEnvPath]: "UNRELATED=value\n",
      });

      yield* runDeploy(
        deployOptions({
          envFile: Option.some("deploy.env"),
          githubOutput: true,
        }),
        fileSystem,
      );

      expect(deployHarness.displayedPlanCount).toBe(1);
      expect(deployHarness.promptMessages).toEqual(["Apply this relay deployment?"]);
      expect(deployHarness.applyCount).toBe(1);
      expect(fileSystem.files.get(rootEnvPath)).toBe(
        ["UNRELATED=value", "BIBCODE_RELAY_URL=https://relay.example.test", ""].join("\n"),
      );
      expect(fileSystem.files.get(githubOutputPath)).toBe(
        "prior=true\nchanged=true\nresult=applied\nrelay_url=https://relay.example.test\n",
      );
    }),
  );

  it.effect("applies changed plans with --yes without displaying a confirmation plan", () =>
    Effect.gen(function* () {
      const githubOutputPath = joinPath(repoRoot, "github-output");
      process.env.GITHUB_OUTPUT = githubOutputPath;
      const fileSystem = virtualFileSystem({
        [githubOutputPath]: "",
        [relayDefaultEnvPath]: "stage=yes-stage\n",
      });

      yield* runDeploy(deployOptions({ githubOutput: true, yes: true }), fileSystem);

      expect(deployHarness.displayedPlanCount).toBe(0);
      expect(deployHarness.promptMessages).toEqual([]);
      expect(deployHarness.applyCount).toBe(1);
      expect(fileSystem.files.get(githubOutputPath)).toBe(
        "changed=true\nresult=applied\nrelay_url=https://relay.example.test\n",
      );
    }),
  );

  it.effect("applies no-op plans and reports a no-op result", () =>
    Effect.gen(function* () {
      const githubOutputPath = joinPath(repoRoot, "github-output");
      process.env.GITHUB_OUTPUT = githubOutputPath;
      deployHarness.plan = { deletions: {}, resources: {} };
      const fileSystem = virtualFileSystem({
        [githubOutputPath]: "",
        [relayDefaultEnvPath]: "stage=noop-stage\n",
      });

      yield* runDeploy(deployOptions({ githubOutput: true }), fileSystem);

      expect(deployHarness.displayedPlanCount).toBe(0);
      expect(deployHarness.promptMessages).toEqual([]);
      expect(deployHarness.applyCount).toBe(1);
      expect(fileSystem.files.get(githubOutputPath)).toBe(
        "changed=false\nresult=noop\nrelay_url=https://relay.example.test\n",
      );
    }),
  );

  it.effect("separates GitHub output from an unterminated existing command", () =>
    Effect.gen(function* () {
      const githubOutputPath = joinPath(repoRoot, "github-output");
      process.env.GITHUB_OUTPUT = githubOutputPath;
      const fileSystem = virtualFileSystem({
        [githubOutputPath]: "prior=true",
        [relayDefaultEnvPath]: "stage=separator-stage\n",
      });

      yield* runDeploy(deployOptions({ githubOutput: true }), fileSystem);

      expect(fileSystem.files.get(githubOutputPath)).toBe(
        "prior=true\nchanged=true\nresult=applied\nrelay_url=https://relay.example.test\n",
      );
    }),
  );

  it.effect("creates a missing GitHub output command file", () =>
    Effect.gen(function* () {
      const githubOutputPath = joinPath(repoRoot, "github-output");
      process.env.GITHUB_OUTPUT = githubOutputPath;
      const fileSystem = virtualFileSystem({
        [relayDefaultEnvPath]: "stage=new-output-stage\n",
      });

      yield* runDeploy(deployOptions({ githubOutput: true }), fileSystem);

      expect(fileSystem.files.get(githubOutputPath)).toBe(
        "changed=true\nresult=applied\nrelay_url=https://relay.example.test\n",
      );
    }),
  );

  it.effect(
    "fails state reads with missing output fields without persisting public configuration",
    () =>
      Effect.gen(function* () {
        deployHarness.stateOutput = {};
        const fileSystem = virtualFileSystem({
          [relayDefaultEnvPath]: "stage=state-error-stage\n",
        });

        const error = yield* Effect.flip(runDeploy(deployOptions({ readState: true }), fileSystem));

        expect(error).toMatchObject({
          missingFields: ["url"],
          source: "alchemy_state",
          stage: "state-error-stage",
        });
        expect(fileSystem.files.has(rootEnvPath)).toBe(false);
      }),
  );

  it.effect(
    "fails apply results with missing output fields without persisting public configuration",
    () =>
      Effect.gen(function* () {
        deployHarness.applyOutput = {};
        const fileSystem = virtualFileSystem({
          [relayDefaultEnvPath]: "stage=apply-error-stage\n",
        });

        const error = yield* Effect.flip(runDeploy(deployOptions({ yes: true }), fileSystem));

        expect(error).toMatchObject({
          missingFields: ["url"],
          source: "alchemy_apply",
          stage: "apply-error-stage",
        });
        expect(fileSystem.files.has(rootEnvPath)).toBe(false);
      }),
  );

  it.effect("propagates deployment and filesystem failures without running later writes", () =>
    Effect.gen(function* () {
      const deploymentFailure = new TestDeployFailure({ message: "deployment operation failed" });
      deployHarness.applyError = deploymentFailure;
      const applyFileSystem = virtualFileSystem({
        [relayDefaultEnvPath]: "stage=apply-failure-stage\n",
      });

      expect(yield* Effect.flip(runDeploy(deployOptions({ yes: true }), applyFileSystem))).toBe(
        deploymentFailure,
      );
      expect(applyFileSystem.files.has(rootEnvPath)).toBe(false);

      deployHarness.applyError = undefined;
      const writeFailure = new TestDeployFailure({ message: "root environment write failed" });
      const writeFileSystem = virtualFileSystem(
        {
          [relayDefaultEnvPath]: "stage=write-failure-stage\n",
        },
        writeFailure,
      );

      expect(yield* Effect.flip(runDeploy(deployOptions({ yes: true }), writeFileSystem))).toBe(
        writeFailure,
      );
      expect(writeFileSystem.writes).toEqual([]);
    }),
  );

  it.effect("restores GitHub output byte-for-byte after a partial commit failure", () =>
    Effect.gen(function* () {
      const githubOutputPath = joinPath(repoRoot, "github-output");
      process.env.GITHUB_OUTPUT = githubOutputPath;
      const commitFailure = new TestDeployFailure({ message: "GitHub output commit failed" });
      const originalGithubOutput = "prior=true\r\nopaque=\u0001bytes\n";
      const fileSystem = virtualFileSystem(
        {
          [githubOutputPath]: originalGithubOutput,
          [relayDefaultEnvPath]: "stage=rollback-stage\n",
          [rootEnvPath]: "ORIGINAL=value\n",
        },
        undefined,
        {},
        {},
        {},
        new Set(),
        {
          [githubOutputPath]: {
            contentsAfterFailure: `${originalGithubOutput}changed=tru`,
            error: commitFailure,
          },
        },
      );

      expect(
        yield* Effect.flip(runDeploy(deployOptions({ githubOutput: true, yes: true }), fileSystem)),
      ).toBe(commitFailure);
      expect(fileSystem.files.get(rootEnvPath)).toBe("ORIGINAL=value\n");
      expect(fileSystem.files.get(githubOutputPath)).toBe(originalGithubOutput);
      expect([...fileSystem.files.keys()].filter((path) => path.includes(".relay-"))).toEqual([]);
    }),
  );

  it.effect("removes a newly created root configuration when a downstream append fails", () =>
    Effect.gen(function* () {
      const githubOutputPath = joinPath(repoRoot, "github-output");
      process.env.GITHUB_OUTPUT = githubOutputPath;
      const commitFailure = new TestDeployFailure({ message: "GitHub output commit failed" });
      const fileSystem = virtualFileSystem(
        {
          [githubOutputPath]: "prior=true\n",
          [relayDefaultEnvPath]: "stage=rollback-new-root-stage\n",
        },
        undefined,
        {},
        {},
        {},
        new Set(),
        {
          [githubOutputPath]: {
            contentsAfterFailure: "prior=true\nchanged=tru",
            error: commitFailure,
          },
        },
      );

      expect(
        yield* Effect.flip(runDeploy(deployOptions({ githubOutput: true, yes: true }), fileSystem)),
      ).toBe(commitFailure);
      expect(fileSystem.files.has(rootEnvPath)).toBe(false);
      expect(fileSystem.files.get(githubOutputPath)).toBe("prior=true\n");
    }),
  );

  it.effect("rolls back committed files and cleans staging after an interrupted rename", () =>
    Effect.gen(function* () {
      const githubOutputPath = joinPath(repoRoot, "github-output");
      process.env.GITHUB_OUTPUT = githubOutputPath;
      const fileSystem = virtualFileSystem(
        {
          [githubOutputPath]: "prior=true\n",
          [relayDefaultEnvPath]: "stage=interrupt-stage\n",
          [rootEnvPath]: "ORIGINAL=value\n",
        },
        undefined,
        {},
        {},
        {},
        new Set([githubOutputPath]),
      );

      const exit = yield* Effect.exit(
        runDeploy(deployOptions({ githubOutput: true, yes: true }), fileSystem),
      );

      expect(Exit.isFailure(exit)).toBe(true);
      expect(fileSystem.files.get(rootEnvPath)).toBe("ORIGINAL=value\n");
      expect(fileSystem.files.get(githubOutputPath)).toBe("prior=true\n");
      expect([...fileSystem.files.keys()].filter((path) => path.includes(".relay-"))).toEqual([]);
    }),
  );
});

describe("relay deploy CLI", () => {
  it.effect("runs the native main entrypoint for help", () =>
    Effect.gen(function* () {
      const result = yield* runDeployCli(["--help"]);

      expect(result.coveredDeployScript).toBe(true);
      expect(result.exitCode).toBe(0);
      expect(result.stdout).toContain("Deploy the BiBCode relay through Alchemy.");
      expect(result.stdout).toContain("--env-file string");
      expect(result.stderr).toBe("");
    }).pipe(Effect.provide(NodeServices.layer)),
  );

  it.effect("reports command parse failures through the native main entrypoint", () =>
    Effect.gen(function* () {
      const result = yield* runDeployCli(["--unknown-relay-option"]);
      const output = `${result.stdout}\n${result.stderr}`;

      expect(result.coveredDeployScript).toBe(true);
      expect(result.exitCode).not.toBe(0);
      expect(output).toContain("Unrecognized flag: --unknown-relay-option");
    }).pipe(Effect.provide(NodeServices.layer)),
  );

  it.effect("exits nonzero on config validation before a real deployment", () =>
    Effect.gen(function* () {
      const result = yield* runDeployCli(["--env-file", "../outside.env", "--read-state"]);
      const output = `${result.stdout}\n${result.stderr}`;

      expect(result.coveredDeployScript).toBe(true);
      expect(result.exitCode).not.toBe(0);
      expect(output).toContain("RelayDeployEnvFilePathError");
      expect(output).toContain("Relay env file path must stay within the relay root");
      expect(output).not.toContain("Apply this relay deployment?");
    }).pipe(Effect.provide(NodeServices.layer)),
  );
});

describe("publicConfigFromOutput", () => {
  it("reads the relay URL from persisted Alchemy output", () => {
    expect(
      publicConfigFromOutput({
        url: "https://relay.example.test",
      }),
    ).toEqual({
      relayUrl: "https://relay.example.test",
    });
  });

  it("rejects incomplete stack output", () => {
    expect(publicConfigFromOutput({})).toBeNull();
  });

  it("rejects non-object output values without throwing or exposing their contents", () => {
    expect(missingRelayPublicConfigFields("not an object")).toEqual(["url"]);
    expect(missingRelayPublicConfigFields(null)).toEqual(["url"]);
    expect(publicConfigFromOutput(null)).toBeNull();
  });

  it("rejects empty relay URLs", () => {
    expect(missingRelayPublicConfigFields({ url: "" })).toEqual(["url"]);
  });
});
