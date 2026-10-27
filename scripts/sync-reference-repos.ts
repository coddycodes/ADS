#!/usr/bin/env node

import * as NodeRuntime from "@effect/platform-node/NodeRuntime";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as Cause from "effect/Cause";
import * as Console from "effect/Console";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as FileSystem from "effect/FileSystem";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import * as Ref from "effect/Ref";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";
import type * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import { Command, Flag } from "effect/cli";
import { ChildProcess, ChildProcessSpawner } from "effect/process";
import { fromYaml } from "@coddy/shared/schemaYaml";

import { referenceRepos, type ReferenceRepo } from "./lib/reference-repos.ts";

export type ReferenceRepoSyncAction = "replace";
export type ReferenceRepoSyncPhase = "verify-clean" | "fetch" | "build" | "apply" | "rollback";

export interface ReferenceRepoSyncOptions {
  readonly rootDir?: string | undefined;
  readonly repoId?: string | undefined;
  readonly latest?: boolean | undefined;
  readonly dryRun?: boolean | undefined;
}

export interface ReferenceRepoSyncPlan {
  readonly repo: ReferenceRepo;
  readonly action: ReferenceRepoSyncAction;
  readonly ref: string;
  readonly fetchArgs: ReadonlyArray<string>;
}

export class ReferenceRepoSelectionError extends Schema.TaggedError<ReferenceRepoSelectionError>()(
  "ReferenceRepoSelectionError",
  {
    repoId: Schema.String,
    expectedRepoIds: Schema.Array(Schema.String),
  },
) {
  override get message(): string {
    return `Unknown reference repo "${this.repoId}". Expected one of: ${this.expectedRepoIds.join(", ")}.`;
  }
}

export class ReferenceRepoPathValidationError extends Schema.TaggedError<ReferenceRepoPathValidationError>()(
  "ReferenceRepoPathValidationError",
  {
    repoId: Schema.String,
    field: Schema.Literals(["prefix", "prunePath"]),
    value: Schema.String,
    reason: Schema.Literals([
      "empty",
      "absolute",
      "backslash",
      "ambiguous-segment",
      "windows-ambiguous",
      "outside-repos",
    ]),
  },
) {
  override get message(): string {
    return `Reference repo "${this.repoId}" has unsafe ${this.field} path "${this.value}" (${this.reason}).`;
  }
}

export class ReferenceRepoVersionSourceError extends Schema.TaggedError<ReferenceRepoVersionSourceError>()(
  "ReferenceRepoVersionSourceError",
  {
    operation: Schema.Literals(["read", "parse"]),
    repoId: Schema.String,
    sourcePath: Schema.String,
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Reference repo "${this.repoId}" version source operation "${this.operation}" failed for ${this.sourcePath}.`;
  }
}

export class ReferenceRepoVersionResolutionError extends Schema.TaggedError<ReferenceRepoVersionResolutionError>()(
  "ReferenceRepoVersionResolutionError",
  {
    repoId: Schema.String,
    sourcePath: Schema.String,
    packageVersionPath: Schema.Array(Schema.String),
  },
) {
  override get message(): string {
    return `No version was found for reference repo "${this.repoId}" at ${this.sourcePath}:${this.packageVersionPath.join(".")}.`;
  }
}

export class ReferenceRepoWorkspaceDirtyError extends Schema.TaggedError<ReferenceRepoWorkspaceDirtyError>()(
  "ReferenceRepoWorkspaceDirtyError",
  {
    rootDir: Schema.String,
    statusLength: Schema.Finite,
  },
) {
  override get message(): string {
    return `Reference repository sync requires a clean index and working tree at ${this.rootDir}.`;
  }
}

export class ReferenceRepoSyncBusyError extends Schema.TaggedError<ReferenceRepoSyncBusyError>()(
  "ReferenceRepoSyncBusyError",
  {
    repoIds: Schema.Array(Schema.String),
  },
) {
  override get message(): string {
    return `Reference repository sync is busy for: ${this.repoIds.join(", ")}.`;
  }
}

export class ReferenceRepoSyncLockError extends Schema.TaggedError<ReferenceRepoSyncLockError>()(
  "ReferenceRepoSyncLockError",
  {
    repoIds: Schema.Array(Schema.String),
    operation: Schema.Literals(["resolve", "acquire", "release"]),
    failure: Schema.Literals(["invalid-common-directory", "filesystem"]),
  },
) {
  override get message(): string {
    return `Reference repository sync lock ${this.operation} failed for: ${this.repoIds.join(", ")}.`;
  }
}

export class ReferenceRepoGitSnapshotError extends Schema.TaggedError<ReferenceRepoGitSnapshotError>()(
  "ReferenceRepoGitSnapshotError",
  {
    operation: Schema.Literals(["spawn", "communicate", "exit"]),
    phase: Schema.Literals(["verify-clean", "fetch", "build", "apply", "rollback"]),
    repoId: Schema.String,
    action: Schema.Literal("replace"),
    repository: Schema.String,
    ref: Schema.String,
    rootDir: Schema.String,
    argumentCount: Schema.Finite,
    exitCode: Schema.optional(Schema.Finite),
    stdoutLength: Schema.optional(Schema.Finite),
    stderrLength: Schema.optional(Schema.Finite),
    cause: Schema.optional(Schema.Defect()),
  },
) {
  override get message(): string {
    return `Git snapshot ${this.action} for reference repo "${this.repoId}" failed during ${this.phase} ${this.operation}.`;
  }
}

export class ReferenceRepoApplyRolledBackError extends Schema.TaggedError<ReferenceRepoApplyRolledBackError>()(
  "ReferenceRepoApplyRolledBackError",
  {
    repoIds: Schema.Array(Schema.String),
    applyOperation: Schema.Literals(["spawn", "communicate", "exit"]),
    applyExitCode: Schema.optional(Schema.Finite),
  },
) {
  override get message(): string {
    return `Reference repository snapshot application failed; the original clean state was restored for: ${this.repoIds.join(", ")}.`;
  }
}

export class ReferenceRepoRollbackError extends Schema.TaggedError<ReferenceRepoRollbackError>()(
  "ReferenceRepoRollbackError",
  {
    repoIds: Schema.Array(Schema.String),
    applyOperation: Schema.optional(Schema.Literals(["spawn", "communicate", "exit"])),
    failure: Schema.Literals(["command", "verification", "timeout"]),
    rollbackOperation: Schema.optional(Schema.Literals(["spawn", "communicate", "exit"])),
    rollbackExitCode: Schema.optional(Schema.Finite),
  },
) {
  override get message(): string {
    return `Reference repository snapshot application and rollback failed for: ${this.repoIds.join(", ")}. Manual recovery is required before continuing.`;
  }
}

export const ReferenceRepoSyncError = Schema.Union([
  ReferenceRepoSelectionError,
  ReferenceRepoPathValidationError,
  ReferenceRepoVersionSourceError,
  ReferenceRepoVersionResolutionError,
  ReferenceRepoWorkspaceDirtyError,
  ReferenceRepoSyncBusyError,
  ReferenceRepoSyncLockError,
  ReferenceRepoGitSnapshotError,
  ReferenceRepoApplyRolledBackError,
  ReferenceRepoRollbackError,
]);
export type ReferenceRepoSyncError = typeof ReferenceRepoSyncError.Type;
export const isReferenceRepoSyncError = Schema.is(ReferenceRepoSyncError);

const decodeJsonSource = Schema.decodeUnknownEffect(Schema.fromJsonString(Schema.Unknown));
const decodeYamlSource = Schema.decodeEffect(fromYaml(Schema.Unknown));
const WINDOWS_ABSOLUTE_PATH = /^[A-Za-z]:/;
const WINDOWS_RESERVED_BASENAME = /^(?:CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\..*)?$/i;
const WINDOWS_FORBIDDEN_CHARACTER = /[<>:"|?*]/;
const REFERENCE_REPO_SYNC_LOCK_NAME = "coddy-studio-reference-repos-sync.lock";
const ROLLBACK_TIMEOUT = "30 seconds";

const literalGitPathspec = (value: string): string => `:(literal)${value}`;

const hasWindowsControlCharacter = (segment: string): boolean =>
  Array.from(segment).some((character) => {
    const codePoint = character.codePointAt(0)!;
    return codePoint <= 0x1f || codePoint === 0x7f;
  });

const isWindowsAmbiguousSegment = (segment: string): boolean =>
  segment.endsWith(" ") ||
  segment.endsWith(".") ||
  WINDOWS_RESERVED_BASENAME.test(segment) ||
  WINDOWS_FORBIDDEN_CHARACTER.test(segment) ||
  hasWindowsControlCharacter(segment);

const validateRepositoryRelativePath = (
  repo: ReferenceRepo,
  field: "prefix" | "prunePath",
  value: string,
): Effect.Effect<void, ReferenceRepoPathValidationError> => {
  const fail = (reason: ReferenceRepoPathValidationError["reason"]) =>
    Effect.fail(
      new ReferenceRepoPathValidationError({
        repoId: repo.id,
        field,
        value,
        reason,
      }),
    );

  if (value.length === 0) return fail("empty");
  if (value.includes("\\")) return fail("backslash");
  if (value.startsWith("/") || WINDOWS_ABSOLUTE_PATH.test(value)) return fail("absolute");

  const segments = value.split("/");
  if (
    segments.some(
      (segment) =>
        segment.length === 0 ||
        segment === "." ||
        segment === ".." ||
        segment.trim() !== segment ||
        segment.includes("\0"),
    )
  ) {
    return fail("ambiguous-segment");
  }
  if (segments.some(isWindowsAmbiguousSegment)) return fail("windows-ambiguous");

  if (field === "prefix") {
    return segments[0] === ".repos" && segments.length >= 2 ? Effect.void : fail("outside-repos");
  }
  return segments[0] === ".repos" ? fail("outside-repos") : Effect.void;
};

const validateReferenceRepoPaths = Effect.fn("validateReferenceRepoPaths")(function* (
  repo: ReferenceRepo,
) {
  yield* validateRepositoryRelativePath(repo, "prefix", repo.prefix);
  for (const prunePath of repo.prunePaths ?? []) {
    yield* validateRepositoryRelativePath(repo, "prunePath", prunePath);
  }
});

const collectStreamAsString = <E>(stream: Stream.Stream<Uint8Array, E>): Effect.Effect<string, E> =>
  stream.pipe(
    Stream.decodeText(),
    Stream.runFold(
      () => "",
      (acc, chunk) => acc + chunk,
    ),
  );

function readNestedString(input: unknown, keys: ReadonlyArray<string>): string | undefined {
  let value = input;
  for (const key of keys) {
    if (typeof value !== "object" || value === null || !(key in value)) {
      return undefined;
    }
    value = (value as Record<string, unknown>)[key];
  }
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function decodeVersionSource(
  repo: ReferenceRepo,
  sourcePath: string,
  content: string,
): Effect.Effect<unknown, ReferenceRepoSyncError> {
  const decode =
    repo.versionSourcePath.endsWith(".yaml") || repo.versionSourcePath.endsWith(".yml")
      ? decodeYamlSource
      : decodeJsonSource;
  return decode(content).pipe(
    Effect.mapError(
      (cause) =>
        new ReferenceRepoVersionSourceError({
          operation: "parse",
          repoId: repo.id,
          sourcePath,
          cause,
        }),
    ),
  );
}

function getSelectedRepos(
  repoId: string | undefined,
  configuredRepos: ReadonlyArray<ReferenceRepo>,
): Effect.Effect<ReadonlyArray<ReferenceRepo>, ReferenceRepoSyncError> {
  if (!repoId) {
    return Effect.succeed(configuredRepos);
  }

  const repo = configuredRepos.find((candidate) => candidate.id === repoId);
  return repo
    ? Effect.succeed([repo])
    : Effect.fail(
        new ReferenceRepoSelectionError({
          repoId,
          expectedRepoIds: configuredRepos.map((candidate) => candidate.id),
        }),
      );
}

export const resolveReferenceRepoRef = Effect.fn("resolveReferenceRepoRef")(function* (
  repo: ReferenceRepo,
  rootDir: string,
  latest: boolean,
) {
  if (latest) {
    return repo.latestRef;
  }

  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const versionSourcePath = path.join(rootDir, repo.versionSourcePath);
  const versionSourceContent = yield* fs.readFileString(versionSourcePath).pipe(
    Effect.mapError(
      (cause) =>
        new ReferenceRepoVersionSourceError({
          operation: "read",
          repoId: repo.id,
          sourcePath: versionSourcePath,
          cause,
        }),
    ),
  );
  const versionSource = yield* decodeVersionSource(repo, versionSourcePath, versionSourceContent);
  const version = readNestedString(versionSource, repo.packageVersionPath);

  if (!version) {
    return yield* new ReferenceRepoVersionResolutionError({
      repoId: repo.id,
      sourcePath: versionSourcePath,
      packageVersionPath: repo.packageVersionPath,
    });
  }

  if (repo.packageSourceRefPrefix && version.startsWith(repo.packageSourceRefPrefix)) {
    const ref = version.slice(repo.packageSourceRefPrefix.length);
    if (ref.length > 0) {
      return ref;
    }
  }

  return `${repo.versionTagPrefix}${version}`;
});

export const planReferenceRepoSync = Effect.fn("planReferenceRepoSync")(function* (
  repo: ReferenceRepo,
  rootDir: string,
  latest: boolean,
) {
  yield* validateReferenceRepoPaths(repo);
  const ref = yield* resolveReferenceRepoRef(repo, rootDir, latest);

  return {
    repo,
    action: "replace",
    ref,
    fetchArgs: ["fetch", "--no-tags", repo.repository, ref],
  } satisfies ReferenceRepoSyncPlan;
});

interface GitCommandOptions {
  readonly indexFile?: string | undefined;
  readonly logStdout?: boolean | undefined;
}

export type ReferenceRepoGitCommandRunner = (
  rootDir: string,
  plan: ReferenceRepoSyncPlan,
  phase: ReferenceRepoSyncPhase,
  args: ReadonlyArray<string>,
  options?: GitCommandOptions,
) => Effect.Effect<
  { readonly stderr: string; readonly stdout: string },
  ReferenceRepoGitSnapshotError,
  ChildProcessSpawner.ChildProcessSpawner | Scope.Scope
>;

export const runGitCommand = Effect.fn("runGitCommand")(function* (
  rootDir: string,
  plan: ReferenceRepoSyncPlan,
  phase: ReferenceRepoSyncPhase,
  args: ReadonlyArray<string>,
  options: GitCommandOptions = {},
) {
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const errorContext = {
    repoId: plan.repo.id,
    action: plan.action,
    repository: plan.repo.repository,
    ref: plan.ref,
    rootDir,
    argumentCount: args.length,
    phase,
  } as const;
  const child = yield* spawner
    .spawn(
      ChildProcess.make("git", args, {
        cwd: rootDir,
        ...(options.indexFile
          ? {
              env: { GIT_INDEX_FILE: options.indexFile },
              extendEnv: true,
            }
          : {}),
      }),
    )
    .pipe(
      Effect.mapError(
        (cause) =>
          new ReferenceRepoGitSnapshotError({
            ...errorContext,
            operation: "spawn",
            cause,
          }),
      ),
    );
  const [stdout, stderr, exitCode] = yield* Effect.all(
    [
      collectStreamAsString(child.stdout),
      collectStreamAsString(child.stderr),
      child.exitCode.pipe(Effect.map(Number)),
    ],
    { concurrency: "unbounded" },
  ).pipe(
    Effect.mapError(
      (cause) =>
        new ReferenceRepoGitSnapshotError({
          ...errorContext,
          operation: "communicate",
          cause,
        }),
    ),
  );

  if (exitCode !== 0) {
    return yield* new ReferenceRepoGitSnapshotError({
      ...errorContext,
      operation: "exit",
      exitCode,
      stdoutLength: stdout.length,
      stderrLength: stderr.length,
    });
  }

  if ((options.logStdout ?? false) && stdout.trim().length > 0) {
    yield* Console.log(stdout.trim());
  }

  return { stderr, stdout };
});

const findGitSnapshotError = <A>(
  exit: Exit.Exit<A, ReferenceRepoGitSnapshotError>,
): ReferenceRepoGitSnapshotError | undefined => {
  if (Exit.isSuccess(exit)) {
    return undefined;
  }
  const error = Cause.findError(exit.cause);
  return Result.isSuccess(error) ? error.success : undefined;
};

const assertCleanWorkspace = Effect.fn("assertCleanWorkspace")(function* (
  rootDir: string,
  plans: ReadonlyArray<ReferenceRepoSyncPlan>,
  commandRunner: ReferenceRepoGitCommandRunner,
) {
  const firstPlan = plans[0];
  if (firstPlan === undefined) {
    return;
  }

  const result = yield* commandRunner(rootDir, firstPlan, "verify-clean", [
    "status",
    "--porcelain=v1",
    "--untracked-files=all",
  ]);
  if (result.stdout.trim().length > 0) {
    return yield* new ReferenceRepoWorkspaceDirtyError({
      rootDir,
      statusLength: result.stdout.length,
    });
  }

  const managedResult = yield* commandRunner(rootDir, firstPlan, "verify-clean", [
    "status",
    "--porcelain=v1",
    "--untracked-files=all",
    "--ignored=matching",
    "--",
    ...plans.map((plan) => literalGitPathspec(plan.repo.prefix)),
  ]);
  if (managedResult.stdout.trim().length > 0) {
    return yield* new ReferenceRepoWorkspaceDirtyError({
      rootDir,
      statusLength: managedResult.stdout.length,
    });
  }
});

const runSnapshotSyncWithLockHeld = Effect.fn("runSnapshotSyncWithLockHeld")(function* (
  rootDir: string,
  plans: ReadonlyArray<ReferenceRepoSyncPlan>,
  commandRunner: ReferenceRepoGitCommandRunner,
  stateIsValid: Ref.Ref<boolean>,
) {
  const firstPlan = plans[0];
  if (firstPlan === undefined) {
    return;
  }

  yield* assertCleanWorkspace(rootDir, plans, commandRunner);
  const originalTree = (yield* commandRunner(rootDir, firstPlan, "verify-clean", [
    "write-tree",
  ])).stdout.trim();

  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "coddy-studio-reference-sync-" });
  const replacementIndex = path.join(tempDir, "replacement.index");
  const snapshots: Array<{ readonly plan: ReferenceRepoSyncPlan; readonly tree: string }> = [];

  for (const [index, plan] of plans.entries()) {
    yield* commandRunner(rootDir, plan, "fetch", plan.fetchArgs, { logStdout: true });
    const sourceIndex = path.join(tempDir, `source-${index}.index`);
    yield* commandRunner(rootDir, plan, "build", ["read-tree", "FETCH_HEAD^{tree}"], {
      indexFile: sourceIndex,
    });
    if (plan.repo.prunePaths && plan.repo.prunePaths.length > 0) {
      yield* commandRunner(
        rootDir,
        plan,
        "build",
        [
          "rm",
          "-r",
          "-f",
          "--cached",
          "--ignore-unmatch",
          "--",
          ...plan.repo.prunePaths.map(literalGitPathspec),
        ],
        { indexFile: sourceIndex },
      );
    }
    const sourceTree = (yield* commandRunner(rootDir, plan, "build", ["write-tree"], {
      indexFile: sourceIndex,
    })).stdout.trim();
    snapshots.push({ plan, tree: sourceTree });
  }

  yield* commandRunner(rootDir, firstPlan, "build", ["read-tree", "HEAD"], {
    indexFile: replacementIndex,
  });
  for (const snapshot of snapshots) {
    yield* commandRunner(
      rootDir,
      snapshot.plan,
      "build",
      [
        "rm",
        "-r",
        "-f",
        "--cached",
        "--ignore-unmatch",
        "--",
        literalGitPathspec(snapshot.plan.repo.prefix),
      ],
      { indexFile: replacementIndex },
    );
    yield* commandRunner(
      rootDir,
      snapshot.plan,
      "build",
      ["read-tree", `--prefix=${snapshot.plan.repo.prefix}/`, snapshot.tree],
      { indexFile: replacementIndex },
    );
  }
  const replacementTree = (yield* commandRunner(rootDir, firstPlan, "build", ["write-tree"], {
    indexFile: replacementIndex,
  })).stdout.trim();

  return yield* Effect.uninterruptibleMask((restore) =>
    Effect.gen(function* () {
      yield* restore(assertCleanWorkspace(rootDir, plans, commandRunner));
      yield* Ref.set(stateIsValid, false);
      const applyExit = yield* restore(
        commandRunner(rootDir, firstPlan, "apply", [
          "read-tree",
          "-m",
          "-u",
          originalTree,
          replacementTree,
        ]).pipe(Effect.scoped),
      ).pipe(Effect.exit);
      if (Exit.isSuccess(applyExit)) {
        yield* Ref.set(stateIsValid, true);
        return;
      }

      const repoIds = plans.map((plan) => plan.repo.id);
      const applyErrorResult = Cause.findError(applyExit.cause);
      const applyError = Result.isSuccess(applyErrorResult) ? applyErrorResult.success : undefined;
      const applyContext = applyError === undefined ? {} : { applyOperation: applyError.operation };
      const rollbackRestoreAttempt = yield* commandRunner(rootDir, firstPlan, "rollback", [
        "restore",
        `--source=${originalTree}`,
        "--staged",
        "--worktree",
        "--",
        ...plans.map((plan) => literalGitPathspec(plan.repo.prefix)),
      ]).pipe(Effect.scoped, Effect.exit, Effect.timeoutOption(ROLLBACK_TIMEOUT));
      if (Option.isNone(rollbackRestoreAttempt)) {
        return yield* new ReferenceRepoRollbackError({
          repoIds,
          ...applyContext,
          failure: "timeout",
        });
      }
      const rollbackCleanupAttempt = yield* commandRunner(rootDir, firstPlan, "rollback", [
        "clean",
        "-d",
        "-f",
        "-x",
        "--",
        ...plans.map((plan) => literalGitPathspec(plan.repo.prefix)),
      ]).pipe(Effect.scoped, Effect.exit, Effect.timeoutOption(ROLLBACK_TIMEOUT));
      if (Option.isNone(rollbackCleanupAttempt)) {
        return yield* new ReferenceRepoRollbackError({
          repoIds,
          ...applyContext,
          failure: "timeout",
        });
      }
      const rollbackRestore = rollbackRestoreAttempt.value;
      const rollbackCleanup = rollbackCleanupAttempt.value;
      if (Exit.isFailure(rollbackRestore) || Exit.isFailure(rollbackCleanup)) {
        const rollbackCommandError = Exit.isFailure(rollbackRestore)
          ? findGitSnapshotError(rollbackRestore)
          : findGitSnapshotError(rollbackCleanup);
        return yield* new ReferenceRepoRollbackError({
          repoIds,
          ...applyContext,
          failure: "command",
          ...(rollbackCommandError === undefined
            ? {}
            : { rollbackOperation: rollbackCommandError.operation }),
          ...(rollbackCommandError?.exitCode === undefined
            ? {}
            : { rollbackExitCode: rollbackCommandError.exitCode }),
        });
      }

      const [rollbackStatusAttempt, rollbackManagedStatusAttempt, rollbackTreeAttempt] =
        yield* Effect.all([
          commandRunner(rootDir, firstPlan, "rollback", [
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
          ]).pipe(Effect.scoped, Effect.exit, Effect.timeoutOption(ROLLBACK_TIMEOUT)),
          commandRunner(rootDir, firstPlan, "rollback", [
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored=matching",
            "--",
            ...plans.map((plan) => literalGitPathspec(plan.repo.prefix)),
          ]).pipe(Effect.scoped, Effect.exit, Effect.timeoutOption(ROLLBACK_TIMEOUT)),
          commandRunner(rootDir, firstPlan, "rollback", ["write-tree"]).pipe(
            Effect.scoped,
            Effect.exit,
            Effect.timeoutOption(ROLLBACK_TIMEOUT),
          ),
        ]);
      if (
        Option.isNone(rollbackStatusAttempt) ||
        Option.isNone(rollbackManagedStatusAttempt) ||
        Option.isNone(rollbackTreeAttempt)
      ) {
        return yield* new ReferenceRepoRollbackError({
          repoIds,
          ...applyContext,
          failure: "timeout",
        });
      }
      const rollbackStatus = rollbackStatusAttempt.value;
      const rollbackManagedStatus = rollbackManagedStatusAttempt.value;
      const rollbackTree = rollbackTreeAttempt.value;
      if (
        Exit.isFailure(rollbackStatus) ||
        Exit.isFailure(rollbackManagedStatus) ||
        Exit.isFailure(rollbackTree) ||
        rollbackStatus.value.stdout.trim().length > 0 ||
        rollbackManagedStatus.value.stdout.trim().length > 0 ||
        rollbackTree.value.stdout.trim() !== originalTree
      ) {
        const commandError = Exit.isFailure(rollbackStatus)
          ? findGitSnapshotError(rollbackStatus)
          : Exit.isFailure(rollbackManagedStatus)
            ? findGitSnapshotError(rollbackManagedStatus)
            : Exit.isFailure(rollbackTree)
              ? findGitSnapshotError(rollbackTree)
              : undefined;
        return yield* new ReferenceRepoRollbackError({
          repoIds,
          ...applyContext,
          failure: "verification",
          ...(commandError === undefined ? {} : { rollbackOperation: commandError.operation }),
          ...(commandError?.exitCode === undefined
            ? {}
            : { rollbackExitCode: commandError.exitCode }),
        });
      }

      yield* Ref.set(stateIsValid, true);
      if (
        applyError !== undefined &&
        !Cause.hasDies(applyExit.cause) &&
        !Cause.hasInterrupts(applyExit.cause)
      ) {
        return yield* new ReferenceRepoApplyRolledBackError({
          repoIds,
          applyOperation: applyError.operation,
          ...(applyError.exitCode === undefined ? {} : { applyExitCode: applyError.exitCode }),
        });
      }
      return yield* Effect.failCause(applyExit.cause);
    }),
  );
});

const isAlreadyExistsError = (error: PlatformError.PlatformError): boolean =>
  error.reason._tag === "AlreadyExists";

const runSnapshotSync = Effect.fn("runSnapshotSync")(function* (
  rootDir: string,
  plans: ReadonlyArray<ReferenceRepoSyncPlan>,
  commandRunner: ReferenceRepoGitCommandRunner,
) {
  const firstPlan = plans[0];
  if (firstPlan === undefined) {
    return;
  }

  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const repoIds = plans.map((plan) => plan.repo.id);
  const stateIsValid = yield* Ref.make(true);
  const commonDirResult = yield* commandRunner(rootDir, firstPlan, "verify-clean", [
    "rev-parse",
    "--path-format=absolute",
    "--git-common-dir",
  ]);
  const commonDirWithoutLf = commonDirResult.stdout.endsWith("\n")
    ? commonDirResult.stdout.slice(0, -1)
    : commonDirResult.stdout;
  const commonDirOutput = commonDirWithoutLf.endsWith("\r")
    ? commonDirWithoutLf.slice(0, -1)
    : commonDirWithoutLf;
  if (
    commonDirOutput.length === 0 ||
    commonDirOutput.includes("\0") ||
    commonDirOutput.includes("\n") ||
    commonDirOutput.includes("\r")
  ) {
    return yield* new ReferenceRepoSyncLockError({
      repoIds,
      operation: "resolve",
      failure: "invalid-common-directory",
    });
  }
  const lockPath = path.join(path.resolve(rootDir, commonDirOutput), REFERENCE_REPO_SYNC_LOCK_NAME);

  return yield* Effect.acquireUseRelease(
    fs.writeFileString(lockPath, "", { flag: "wx", mode: 0o600 }).pipe(
      Effect.mapError((error) =>
        isAlreadyExistsError(error)
          ? new ReferenceRepoSyncBusyError({ repoIds })
          : new ReferenceRepoSyncLockError({
              repoIds,
              operation: "acquire",
              failure: "filesystem",
            }),
      ),
      Effect.as(lockPath),
    ),
    () =>
      runSnapshotSyncWithLockHeld(rootDir, plans, commandRunner, stateIsValid).pipe(Effect.scoped),
    (acquiredLockPath) =>
      Ref.get(stateIsValid).pipe(
        Effect.flatMap((isValid) =>
          isValid
            ? fs.remove(acquiredLockPath).pipe(
                Effect.mapError(
                  () =>
                    new ReferenceRepoSyncLockError({
                      repoIds,
                      operation: "release",
                      failure: "filesystem",
                    }),
                ),
              )
            : Effect.void,
        ),
      ),
  );
});

export const syncReferenceRepos = Effect.fn("syncReferenceRepos")(function* (
  options: ReferenceRepoSyncOptions = {},
  configuredRepos: ReadonlyArray<ReferenceRepo> = referenceRepos,
  commandRunner: ReferenceRepoGitCommandRunner = runGitCommand,
) {
  const path = yield* Path.Path;
  const rootDir = path.resolve(options.rootDir ?? process.cwd());
  const repos = yield* getSelectedRepos(options.repoId, configuredRepos);
  const plans: Array<ReferenceRepoSyncPlan> = [];

  for (const repo of repos) {
    const plan = yield* planReferenceRepoSync(repo, rootDir, options.latest ?? false);
    plans.push(plan);
    yield* Console.log(`Syncing ${repo.id} from ${plan.ref} with exact Git snapshot replacement.`);
  }
  if (!(options.dryRun ?? false)) {
    yield* runSnapshotSync(rootDir, plans, commandRunner).pipe(Effect.scoped);
  }

  return plans;
});

export const syncReferenceReposCommand = Command.make(
  "sync-reference-repos",
  {
    repo: Flag.String("repo").pipe(
      Flag.withDescription("Sync only the named reference repo. Defaults to all configured repos."),
      Flag.optional,
    ),
    latest: Flag.Boolean("latest").pipe(
      Flag.withDescription(
        "Sync each repo from its latest branch instead of the installed version.",
      ),
      Flag.withDefault(false),
    ),
    root: Flag.String("root").pipe(
      Flag.withDescription("Workspace root used to resolve versions and subtree prefixes."),
      Flag.optional,
    ),
    dryRun: Flag.Boolean("dry-run").pipe(
      Flag.withDescription("Print planned subtree operations without running git."),
      Flag.withDefault(false),
    ),
  },
  ({ repo, latest, root, dryRun }) =>
    syncReferenceRepos({
      repoId: Option.getOrUndefined(repo),
      rootDir: Option.getOrUndefined(root),
      latest,
      dryRun,
    }),
).pipe(Command.withDescription("Sync vendored reference repositories under .repos/."));

type MainLauncher = <E, A>(effect: Effect.Effect<A, E, never>) => void;

export const runSyncReferenceReposMain = (
  isMain: boolean,
  launch: MainLauncher = NodeRuntime.runMain,
) => {
  if (!isMain) return false;

  launch(
    Command.run(syncReferenceReposCommand, { version: "0.0.0" }).pipe(
      Effect.provide(NodeServices.layer),
    ),
  );
  return true;
};

runSyncReferenceReposMain(import.meta.main);
