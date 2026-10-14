import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";

import {
  EnvironmentId,
  NonNegativeInt,
  ProjectId,
  ThreadId,
  TrimmedNonEmptyString,
} from "./baseSchemas.ts";
import { RemoteUpdateSupport } from "./remoteUpdate.ts";

export const ExecutionEnvironmentPlatformOs = Schema.Literals([
  "darwin",
  "linux",
  "windows",
  "unknown",
]);
export type ExecutionEnvironmentPlatformOs = typeof ExecutionEnvironmentPlatformOs.Type;

export const ExecutionEnvironmentPlatformArch = Schema.Literals(["arm64", "x64", "other"]);
export type ExecutionEnvironmentPlatformArch = typeof ExecutionEnvironmentPlatformArch.Type;

export const ExecutionEnvironmentPlatform = Schema.Struct({
  os: ExecutionEnvironmentPlatformOs,
  arch: ExecutionEnvironmentPlatformArch,
});
export type ExecutionEnvironmentPlatform = typeof ExecutionEnvironmentPlatform.Type;

export const ExecutionEnvironmentCapabilities = Schema.Struct({
  attachmentStaging: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  repositoryIdentity: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  worktreeCatalog: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  worktreeCatalogRefreshReason: Schema.Boolean.pipe(
    Schema.withDecodingDefault(Effect.succeed(false)),
  ),
  vcsStatusSummary: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  gitManagerReads: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  gitManagerCommitOperations: Schema.Boolean.pipe(
    Schema.withDecodingDefault(Effect.succeed(false)),
  ),
  gitManagerBranchSyncOperations: Schema.Boolean.pipe(
    Schema.withDecodingDefault(Effect.succeed(false)),
  ),
  gitManagerStashMergeOperations: Schema.Boolean.pipe(
    Schema.withDecodingDefault(Effect.succeed(false)),
  ),
  gitManagerMergeIntoOperations: Schema.Boolean.pipe(
    Schema.withDecodingDefault(Effect.succeed(false)),
  ),
  gitManagerPartialStaging: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  gitManagerRewriteOperations: Schema.Boolean.pipe(
    Schema.withDecodingDefault(Effect.succeed(false)),
  ),
  gitManagerTagOperations: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  gitManagerLiveSignal: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  gitManagerPullRequests: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  /** Stacked request actions honor reviewed target and source branches before publication. */
  gitPullRequestBranchSelection: Schema.Boolean.pipe(
    Schema.withDecodingDefault(Effect.succeed(false)),
  ),
  pullRequestsReads: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  pullRequestsMutations: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  activityProtocolVersion: Schema.NullOr(Schema.Literal(2)).pipe(
    Schema.withDecodingDefault(Effect.succeed(null)),
  ),
  remoteUpdateControl: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  /** The server reports update progress, `bootId` and `updater.activeWork`. */
  remoteUpdateProgress: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  terminalOrderedInput: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  terminalSizeOwnership: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  /** `terminal.stageImagePaste` turns a pasted clipboard image into a server file path. */
  terminalImagePaste: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  /** `git.runStackedAction` accepts `pullRequestOptions` and reports `pr.warning`. */
  pullRequestCreateOptions: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  vcsCloneReattach: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  /** `server.getProfilerStatus` and `server.reportProfilerClientBatch`, and `enableProfiler`. */
  profiler: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  /** `device.*` methods, the device media socket, and `enableDevicePreviews`. */
  devicePreviews: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
});
export type ExecutionEnvironmentCapabilities = typeof ExecutionEnvironmentCapabilities.Type;

export const REMOTE_PROTOCOL_VERSION = 1;
export const MIN_COMPATIBLE_REMOTE_PROTOCOL = 1;

export const ExecutionEnvironmentDescriptor = Schema.Struct({
  environmentId: EnvironmentId,
  label: TrimmedNonEmptyString,
  platform: ExecutionEnvironmentPlatform,
  serverVersion: TrimmedNonEmptyString,
  storageInstanceId: Schema.NullOr(TrimmedNonEmptyString).pipe(
    Schema.withDecodingDefault(Effect.succeed(null)),
  ),
  /** Random for each server start; never persisted and never a storage identity. */
  bootId: Schema.NullOr(TrimmedNonEmptyString).pipe(
    Schema.withDecodingDefault(Effect.succeed(null)),
  ),
  remoteUpdateSupport: Schema.NullOr(RemoteUpdateSupport).pipe(
    Schema.withDecodingDefault(Effect.succeed(null)),
  ),
  remoteProtocolVersion: NonNegativeInt.pipe(Schema.withDecodingDefault(Effect.succeed(0))),
  minCompatibleRemoteProtocol: NonNegativeInt.pipe(Schema.withDecodingDefault(Effect.succeed(0))),
  capabilities: ExecutionEnvironmentCapabilities,
});
export type ExecutionEnvironmentDescriptor = typeof ExecutionEnvironmentDescriptor.Type;

export const EnvironmentConnectionState = Schema.Literals([
  "connecting",
  "connected",
  "disconnected",
  "error",
]);
export type EnvironmentConnectionState = typeof EnvironmentConnectionState.Type;

export const RepositoryIdentityLocator = Schema.Struct({
  source: Schema.Literal("git-remote"),
  remoteName: TrimmedNonEmptyString,
  remoteUrl: TrimmedNonEmptyString,
});
export type RepositoryIdentityLocator = typeof RepositoryIdentityLocator.Type;

export const RepositoryIdentity = Schema.Struct({
  canonicalKey: TrimmedNonEmptyString,
  locator: RepositoryIdentityLocator,
  rootPath: Schema.optionalKey(TrimmedNonEmptyString),
  displayName: Schema.optionalKey(TrimmedNonEmptyString),
  provider: Schema.optionalKey(TrimmedNonEmptyString),
  owner: Schema.optionalKey(TrimmedNonEmptyString),
  name: Schema.optionalKey(TrimmedNonEmptyString),
});
export type RepositoryIdentity = typeof RepositoryIdentity.Type;

export const ScopedProjectRef = Schema.Struct({
  environmentId: EnvironmentId,
  projectId: ProjectId,
});
export type ScopedProjectRef = typeof ScopedProjectRef.Type;

export const ScopedThreadRef = Schema.Struct({
  environmentId: EnvironmentId,
  threadId: ThreadId,
});
export type ScopedThreadRef = typeof ScopedThreadRef.Type;

export const ScopedThreadSessionRef = Schema.Struct({
  environmentId: EnvironmentId,
  threadId: ThreadId,
});
export type ScopedThreadSessionRef = typeof ScopedThreadSessionRef.Type;
