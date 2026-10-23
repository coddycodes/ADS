#!/usr/bin/env node

import * as NodeRuntime from "@effect/platform-node/NodeRuntime";
import * as NodeServices from "@effect/platform-node/NodeServices";
// @effect-diagnostics-next-line nodeBuiltinImport:off - Hashes build inputs outside an Effect Crypto service.
import * as NodeCrypto from "node:crypto";
import * as Config from "effect/Config";
import * as Console from "effect/Console";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import * as Schema from "effect/Schema";
import { Argument, Command, Flag } from "effect/cli";
import { fromJsonStringPretty } from "@bibcode/shared/schemaJson";

export class ReleasePackageManifestError extends Schema.TaggedError<ReleasePackageManifestError>()(
  "ReleasePackageManifestError",
  {
    operation: Schema.Literals(["read", "decode", "encode", "write", "replace", "cleanup"]),
    filePath: Schema.String,
    cause: Schema.Defect(),
    rollbackFailures: Schema.Array(
      Schema.Struct({
        operation: Schema.Literals(["restore-original", "remove-staged", "cleanup-backup"]),
        filePath: Schema.String,
        cause: Schema.Defect(),
      }),
    ),
  },
) {
  override get message(): string {
    return `Failed to ${this.operation} release package manifest '${this.filePath}'.`;
  }
}

export class ReleaseGitHubOutputConfigurationError extends Schema.TaggedError<ReleaseGitHubOutputConfigurationError>()(
  "ReleaseGitHubOutputConfigurationError",
  { cause: Schema.Defect() },
) {
  override get message(): string {
    return "Failed to resolve GITHUB_OUTPUT for release package version output.";
  }
}

export class ReleaseGitHubOutputWriteError extends Schema.TaggedError<ReleaseGitHubOutputWriteError>()(
  "ReleaseGitHubOutputWriteError",
  {
    filePath: Schema.String,
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Failed to append release package version output to '${this.filePath}'.`;
  }
}

export const releasePackageFiles = [
  "apps/server/package.json",
  "apps/desktop/package.json",
  "apps/web/package.json",
  "packages/contracts/package.json",
] as const;

export const releaseRustPackageFiles = [
  "apps/server/Cargo.toml",
  "apps/desktop/src-tauri/Cargo.toml",
] as const;

export const releaseCargoLockFile = "Cargo.lock" as const;

export const releaseVersionFiles = [
  ...releasePackageFiles,
  ...releaseRustPackageFiles,
  releaseCargoLockFile,
] as const;

interface UpdateReleasePackageVersionsOptions {
  readonly rootDir?: string | undefined;
  readonly copyFile?: FileSystem.FileSystem["copyFile"] | undefined;
  readonly encodePackageJson?: typeof encodePackageJson | undefined;
  readonly moveFile?: FileSystem.FileSystem["rename"] | undefined;
  readonly removeFile?: FileSystem.FileSystem["remove"] | undefined;
  readonly transactionId?: (() => string) | undefined;
  readonly writeFileString?: FileSystem.FileSystem["writeFileString"] | undefined;
}

const PackageJsonSchema = Schema.Record(Schema.String, Schema.Unknown);
const PackageJsonPrettyJson = fromJsonStringPretty(PackageJsonSchema);
const PackageJsonCompactJson = Schema.fromJsonString(PackageJsonSchema);
const decodePackageJson = Schema.decodeUnknownEffect(PackageJsonPrettyJson);
const encodePackageJson = Schema.encodeEffect(PackageJsonPrettyJson);
const encodeCompactPackageJson = Schema.encodeEffect(PackageJsonCompactJson);

interface ManifestTextStyle {
  readonly compact: boolean;
  readonly finalNewline: boolean;
  readonly indent: string;
  readonly newline: "\n" | "\r\n";
}

interface ReleasePackageTransactionFailure {
  readonly operation: "restore-original" | "remove-staged" | "cleanup-backup";
  readonly filePath: string;
  readonly cause: PlatformError.PlatformError;
}

interface ReleasePackageChange {
  readonly backupPath: string;
  readonly content: string;
  readonly filePath: string;
  readonly tempPath: string;
  backupAttempted: boolean;
  backupReady: boolean;
  replacementAttempted: boolean;
  stageAttempted: boolean;
}

interface ReleasePackageTransactionState {
  committed: boolean;
  finalized: boolean;
  readonly rollbackFailures: Array<ReleasePackageTransactionFailure>;
}

const settleFileSystemOperation = <A, E, R>(
  operation: Effect.Effect<A, PlatformError.PlatformError>,
  handlers: {
    readonly onFailure: (cause: PlatformError.PlatformError) => Effect.Effect<void, E, R>;
    readonly onSuccess: (value: A) => void;
  },
) =>
  Effect.uninterruptible(
    operation.pipe(
      Effect.matchEffect({
        onFailure: handlers.onFailure,
        onSuccess: (value) => Effect.sync(() => handlers.onSuccess(value)),
      }),
    ),
  );

const detectManifestTextStyle = (source: string): ManifestTextStyle => {
  const compact = !source.includes("\n");
  const indent = /\r?\n([ \t]+)"/.exec(source)?.[1] ?? "  ";
  return {
    compact,
    finalNewline: source.endsWith("\n"),
    indent,
    newline: source.includes("\r\n") ? "\r\n" : "\n",
  };
};

const applyManifestTextStyle = (encoded: string, style: ManifestTextStyle): string => {
  if (style.compact) return encoded;

  const content = encoded
    .split("\n")
    .map((line) => {
      const spaces = line.length - line.trimStart().length;
      return `${style.indent.repeat(spaces / 2)}${line.slice(spaces)}`;
    })
    .join(style.newline);
  return style.finalNewline ? `${content}${style.newline}` : content;
};

interface CargoVersionReplacement {
  readonly changed: boolean;
  readonly content: string;
}

const cargoVersionLine = /^([ \t]*version[ \t]*=[ \t]*")([^"\r\n]+)("[^\r\n]*)$/m;

const replaceCargoPackageVersion = (source: string, version: string): CargoVersionReplacement => {
  const packageHeader = /^\[package\][ \t]*\r?$/m.exec(source);
  if (!packageHeader) throw new Error("Cargo manifest is missing a [package] section.");

  const bodyStart = packageHeader.index + packageHeader[0].length;
  const followingSource = source.slice(bodyStart);
  const nextSection = /^\[[^\r\n]+\][ \t]*\r?$/m.exec(followingSource);
  const bodyEnd = nextSection ? bodyStart + nextSection.index : source.length;
  const body = source.slice(bodyStart, bodyEnd);
  const versionLine = cargoVersionLine.exec(body);
  if (!versionLine || versionLine.index === undefined) {
    throw new Error("Cargo [package] section is missing a string version.");
  }

  const currentVersion = versionLine[2]!;
  if (currentVersion === version) return { changed: false, content: source };

  const valueStart = bodyStart + versionLine.index + versionLine[1]!.length;
  const valueEnd = valueStart + currentVersion.length;
  return {
    changed: true,
    content: `${source.slice(0, valueStart)}${version}${source.slice(valueEnd)}`,
  };
};

const replaceCargoLockVersions = (source: string, version: string): CargoVersionReplacement => {
  const packageHeaders = Array.from(source.matchAll(/^\[\[package\]\][ \t]*\r?$/gm));
  const targetNames = new Set(["bibcode-desktop", "bibcode-server"]);
  const seenNames = new Set<string>();
  const replacements: Array<{ readonly start: number; readonly end: number }> = [];

  for (const [index, header] of packageHeaders.entries()) {
    const bodyStart = header.index! + header[0].length;
    const bodyEnd = packageHeaders[index + 1]?.index ?? source.length;
    const body = source.slice(bodyStart, bodyEnd);
    const name = /^[ \t]*name[ \t]*=[ \t]*"([^"\r\n]+)"[ \t]*$/m.exec(body)?.[1];
    if (!name || !targetNames.has(name)) continue;
    if (seenNames.has(name)) throw new Error(`Cargo.lock contains duplicate ${name} packages.`);
    seenNames.add(name);

    const versionLine = cargoVersionLine.exec(body);
    if (!versionLine || versionLine.index === undefined) {
      throw new Error(`Cargo.lock package ${name} is missing a string version.`);
    }
    const currentVersion = versionLine[2]!;
    if (currentVersion === version) continue;
    const start = bodyStart + versionLine.index + versionLine[1]!.length;
    replacements.push({ start, end: start + currentVersion.length });
  }

  const missingNames = Array.from(targetNames).filter((name) => !seenNames.has(name));
  if (missingNames.length > 0) {
    throw new Error(`Cargo.lock is missing release packages: ${missingNames.join(", ")}.`);
  }
  if (replacements.length === 0) return { changed: false, content: source };

  let content = source;
  for (const replacement of replacements.toReversed()) {
    content = `${content.slice(0, replacement.start)}${version}${content.slice(replacement.end)}`;
  }
  return { changed: true, content };
};

export const updateReleasePackageVersions = Effect.fn("updateReleasePackageVersions")(function* (
  version: string,
  options: UpdateReleasePackageVersionsOptions = {},
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const rootDir = path.resolve(options.rootDir ?? process.cwd());
  const copyFile = options.copyFile ?? fs.copyFile;
  const encode = options.encodePackageJson ?? encodePackageJson;
  const moveFile = options.moveFile ?? fs.rename;
  const removeFile = options.removeFile ?? fs.remove;
  const writeFileString = options.writeFileString ?? fs.writeFileString;
  const transactionId = options.transactionId?.() ?? NodeCrypto.randomUUID();
  const changes: Array<ReleasePackageChange> = [];

  for (const relativePath of releasePackageFiles) {
    const filePath = path.join(rootDir, relativePath);
    const packageJsonText = yield* fs.readFileString(filePath).pipe(
      Effect.mapError(
        (cause) =>
          new ReleasePackageManifestError({
            operation: "read",
            filePath,
            cause,
            rollbackFailures: [],
          }),
      ),
    );
    const packageJson = yield* decodePackageJson(packageJsonText).pipe(
      Effect.mapError(
        (cause) =>
          new ReleasePackageManifestError({
            operation: "decode",
            filePath,
            cause,
            rollbackFailures: [],
          }),
      ),
    );
    if (packageJson.version === version) {
      continue;
    }

    const style = detectManifestTextStyle(packageJsonText);
    const selectedEncode =
      options.encodePackageJson ?? (style.compact ? encodeCompactPackageJson : encode);
    const packageJsonString = yield* selectedEncode({ ...packageJson, version }).pipe(
      Effect.mapError(
        (cause) =>
          new ReleasePackageManifestError({
            operation: "encode",
            filePath,
            cause,
            rollbackFailures: [],
          }),
      ),
    );
    changes.push({
      backupPath: `${filePath}.bibcode-${transactionId}.backup`,
      content: applyManifestTextStyle(packageJsonString, style),
      filePath,
      tempPath: `${filePath}.bibcode-${transactionId}.stage`,
      backupAttempted: false,
      backupReady: false,
      replacementAttempted: false,
      stageAttempted: false,
    });
  }

  for (const relativePath of [...releaseRustPackageFiles, releaseCargoLockFile]) {
    const filePath = path.join(rootDir, relativePath);
    const source = yield* fs.readFileString(filePath).pipe(
      Effect.mapError(
        (cause) =>
          new ReleasePackageManifestError({
            operation: "read",
            filePath,
            cause,
            rollbackFailures: [],
          }),
      ),
    );
    const replacement = yield* Effect.try({
      try: () =>
        relativePath === releaseCargoLockFile
          ? replaceCargoLockVersions(source, version)
          : replaceCargoPackageVersion(source, version),
      catch: (cause) =>
        new ReleasePackageManifestError({
          operation: "decode",
          filePath,
          cause,
          rollbackFailures: [],
        }),
    });
    if (!replacement.changed) continue;

    changes.push({
      backupPath: `${filePath}.bibcode-${transactionId}.backup`,
      content: replacement.content,
      filePath,
      tempPath: `${filePath}.bibcode-${transactionId}.stage`,
      backupAttempted: false,
      backupReady: false,
      replacementAttempted: false,
      stageAttempted: false,
    });
  }

  if (changes.length === 0) return { changed: false };

  const finalizeTransaction = Effect.fn("finalizeReleasePackageVersionTransaction")(function* (
    state: ReleasePackageTransactionState,
  ) {
    if (state.finalized) return;

    if (!state.committed) {
      for (const change of changes.toReversed()) {
        if (!change.replacementAttempted || !change.backupReady) continue;
        yield* settleFileSystemOperation(moveFile(change.backupPath, change.filePath), {
          onFailure: (cause) =>
            Effect.sync(() => {
              state.rollbackFailures.push({
                operation: "restore-original",
                filePath: change.filePath,
                cause,
              });
            }),
          onSuccess: () => {
            change.backupReady = false;
            change.replacementAttempted = false;
          },
        });
      }
    }

    for (const change of changes) {
      if (change.stageAttempted) {
        yield* settleFileSystemOperation(removeFile(change.tempPath, { force: true }), {
          onFailure: (cause) =>
            Effect.sync(() => {
              state.rollbackFailures.push({
                operation: "remove-staged",
                filePath: change.filePath,
                cause,
              });
            }),
          onSuccess: () => {
            change.stageAttempted = false;
          },
        });
      }

      const backupIsRequiredForRecovery =
        !state.committed && change.replacementAttempted && change.backupReady;
      if (change.backupAttempted && !backupIsRequiredForRecovery) {
        yield* settleFileSystemOperation(removeFile(change.backupPath, { force: true }), {
          onFailure: (cause) =>
            Effect.sync(() => {
              state.rollbackFailures.push({
                operation: "cleanup-backup",
                filePath: change.filePath,
                cause,
              });
            }),
          onSuccess: () => {
            change.backupAttempted = false;
            change.backupReady = false;
          },
        });
      }
    }

    state.finalized = true;
  });

  return yield* Effect.acquireUseRelease(
    Effect.sync((): ReleasePackageTransactionState => ({
      committed: false,
      finalized: false,
      rollbackFailures: [],
    })),
    (state) =>
      Effect.gen(function* () {
        const failAfterFinalization = (
          operation: "write" | "replace",
          filePath: string,
          cause: PlatformError.PlatformError,
        ) =>
          Effect.uninterruptible(finalizeTransaction(state)).pipe(
            Effect.flatMap(() =>
              Effect.fail(
                new ReleasePackageManifestError({
                  operation,
                  filePath,
                  cause,
                  rollbackFailures: state.rollbackFailures,
                }),
              ),
            ),
          );

        for (const change of changes) {
          change.stageAttempted = true;
          yield* settleFileSystemOperation(
            writeFileString(change.tempPath, change.content, { flag: "wx" }),
            {
              onFailure: (cause) => failAfterFinalization("write", change.filePath, cause),
              onSuccess: () => undefined,
            },
          );
        }

        for (const change of changes) {
          change.backupAttempted = true;
          yield* settleFileSystemOperation(copyFile(change.filePath, change.backupPath), {
            onFailure: (cause) => failAfterFinalization("replace", change.filePath, cause),
            onSuccess: () => {
              change.backupReady = true;
            },
          });
          change.replacementAttempted = true;

          yield* settleFileSystemOperation(moveFile(change.tempPath, change.filePath), {
            onFailure: (cause) => failAfterFinalization("replace", change.filePath, cause),
            onSuccess: () => {
              change.stageAttempted = false;
            },
          });
        }

        state.committed = true;
        const cleanupFailures: Array<ReleasePackageTransactionFailure> = [];
        for (const change of changes) {
          yield* settleFileSystemOperation(removeFile(change.backupPath, { force: true }), {
            onFailure: (cause) =>
              Effect.sync(() => {
                cleanupFailures.push({
                  operation: "cleanup-backup",
                  filePath: change.filePath,
                  cause,
                });
              }),
            onSuccess: () => {
              change.backupAttempted = false;
              change.backupReady = false;
            },
          });
        }
        state.finalized = true;

        const [cleanupFailure, ...additionalCleanupFailures] = cleanupFailures;
        if (cleanupFailure) {
          return yield* new ReleasePackageManifestError({
            operation: "cleanup",
            filePath: cleanupFailure.filePath,
            cause: cleanupFailure.cause,
            rollbackFailures: additionalCleanupFailures,
          });
        }

        return { changed: true };
      }),
    (state) => finalizeTransaction(state),
  );
});

const writeGithubOutput = Effect.fn("writeGithubOutput")(function* (changed: boolean) {
  const fs = yield* FileSystem.FileSystem;
  const githubOutputPath = yield* Config.NonEmptyString("GITHUB_OUTPUT").pipe(
    Effect.mapError(
      (cause) =>
        new ReleaseGitHubOutputConfigurationError({
          cause,
        }),
    ),
  );
  yield* fs.writeFileString(githubOutputPath, `changed=${changed}\n`, { flag: "a" }).pipe(
    Effect.mapError(
      (cause) =>
        new ReleaseGitHubOutputWriteError({
          filePath: githubOutputPath,
          cause,
        }),
    ),
  );
});

export const updateReleasePackageVersionsCommand = Command.make(
  "update-release-package-versions",
  {
    version: Argument.String("version").pipe(
      Argument.withDescription("Release version to write into each releasable package manifest."),
    ),
    root: Flag.String("root").pipe(
      Flag.withDescription("Workspace root used to resolve the release package manifests."),
      Flag.optional,
    ),
    githubOutput: Flag.Boolean("github-output").pipe(
      Flag.withDescription("Append changed=<boolean> to GITHUB_OUTPUT."),
      Flag.withDefault(false),
    ),
  },
  ({ version, root, githubOutput }) =>
    updateReleasePackageVersions(version, {
      rootDir: Option.getOrUndefined(root),
    }).pipe(
      Effect.tap(({ changed }) =>
        changed
          ? Effect.void
          : Console.log("All release package versions already match release version."),
      ),
      Effect.tap(({ changed }) => (githubOutput ? writeGithubOutput(changed) : Effect.void)),
    ),
).pipe(Command.withDescription("Update release package versions across the workspace."));

type MainLauncher = <E, A>(effect: Effect.Effect<A, E, never>) => void;

export const runUpdateReleasePackageVersionsMain = (
  isMain: boolean,
  launch: MainLauncher = NodeRuntime.runMain,
) => {
  if (!isMain) return false;

  launch(
    Command.run(updateReleasePackageVersionsCommand, { version: "0.0.0" }).pipe(
      Effect.provide(NodeServices.layer),
    ),
  );
  return true;
};

runUpdateReleasePackageVersionsMain(import.meta.main);
