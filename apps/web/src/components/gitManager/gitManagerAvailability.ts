import type { SupervisorConnectionState } from "@coddy/client-runtime/connection";
import type { ServerConfig } from "@coddy/contracts";

export type GitManagerAvailability =
  | { readonly kind: "ready" }
  | { readonly kind: "pending"; readonly reason: string }
  | { readonly kind: "disconnected"; readonly reason: string }
  | { readonly kind: "unsupported"; readonly missingCapability: "gitManagerReads" };

export const GIT_MANAGER_BRANCH_SYNC_DISABLED_REASON =
  "This environment does not support Git Manager branch and sync operations.";
export const GIT_MANAGER_STASH_MERGE_DISABLED_REASON =
  "This environment does not support Git Manager stash and merge operations.";
export const GIT_MANAGER_REWRITE_DISABLED_REASON =
  "This environment does not support Git Manager rewrite operations.";
export const GIT_MANAGER_TAG_DISABLED_REASON =
  "This environment does not support Git Manager tag operations.";
export const GIT_MANAGER_PULL_REQUESTS_DISABLED_REASON =
  "This environment does not support Git Manager pull request operations.";
export const GIT_MANAGER_MERGE_INTO_DISABLED_REASON =
  "This environment does not support merging into a branch that is not checked out.";
export const GIT_MANAGER_LIVE_SIGNAL_DISABLED_REASON =
  "This environment does not support Git Manager live updates. Use Refresh to load new repository data.";

export interface GitManagerCapabilityDisabledReasons {
  readonly branchSync: string | null;
  readonly stashMerge: string | null;
  readonly rewrite: string | null;
  readonly tag: string | null;
  readonly pullRequests: string | null;
  readonly liveSignal: string | null;
  readonly mergeInto: string | null;
}

export function resolveGitManagerCapabilityDisabledReasons(
  serverConfig: ServerConfig | null,
): GitManagerCapabilityDisabledReasons {
  const capabilities = serverConfig?.environment?.capabilities;
  return {
    branchSync:
      capabilities?.gitManagerBranchSyncOperations === true
        ? null
        : GIT_MANAGER_BRANCH_SYNC_DISABLED_REASON,
    stashMerge:
      capabilities?.gitManagerStashMergeOperations === true
        ? null
        : GIT_MANAGER_STASH_MERGE_DISABLED_REASON,
    rewrite:
      capabilities?.gitManagerRewriteOperations === true
        ? null
        : GIT_MANAGER_REWRITE_DISABLED_REASON,
    tag: capabilities?.gitManagerTagOperations === true ? null : GIT_MANAGER_TAG_DISABLED_REASON,
    pullRequests:
      capabilities?.gitManagerPullRequests === true
        ? null
        : GIT_MANAGER_PULL_REQUESTS_DISABLED_REASON,
    liveSignal:
      capabilities?.gitManagerLiveSignal === true ? null : GIT_MANAGER_LIVE_SIGNAL_DISABLED_REASON,
    mergeInto:
      capabilities?.gitManagerMergeIntoOperations === true
        ? null
        : GIT_MANAGER_MERGE_INTO_DISABLED_REASON,
  };
}

function reconnectingReason(environmentLabel: string): string {
  return `Reconnecting to ${environmentLabel}. Git Manager loads when the connection is back.`;
}

function disconnectedReason(
  connectionState: SupervisorConnectionState,
  environmentLabel: string,
): string {
  switch (connectionState.phase) {
    case "available":
      return "This environment is disconnected.";
    case "offline":
      return "This environment is offline.";
    case "backoff":
      return reconnectingReason(environmentLabel);
    case "blocked":
      return connectionState.lastFailure?.message ?? "This environment connection is blocked.";
    case "connecting":
    case "connected":
      return "This environment is unavailable.";
  }
}

export function resolveGitManagerAvailability(
  connectionState: SupervisorConnectionState | null,
  serverConfig: ServerConfig | null,
  environmentLabel: string,
): GitManagerAvailability {
  if (connectionState === null) {
    return { kind: "pending", reason: "Waiting for the environment connection state." };
  }
  if (!connectionState.desired || connectionState.phase === "available") {
    return { kind: "disconnected", reason: "This environment is disconnected." };
  }
  if (connectionState.phase === "connecting") {
    if (connectionState.lastFailure !== null) {
      return { kind: "pending", reason: reconnectingReason(environmentLabel) };
    }
    return {
      kind: "pending",
      reason:
        connectionState.stage === "synchronizing"
          ? "This environment is synchronizing."
          : "This environment is connecting.",
    };
  }
  if (connectionState.phase !== "connected") {
    return { kind: "disconnected", reason: disconnectedReason(connectionState, environmentLabel) };
  }
  if (serverConfig === null) {
    return { kind: "pending", reason: "Waiting for Git Manager capabilities." };
  }
  if (serverConfig.environment.capabilities.gitManagerReads !== true) {
    return { kind: "unsupported", missingCapability: "gitManagerReads" };
  }
  return { kind: "ready" };
}
