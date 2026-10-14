// Test-only fixture support. Import through the dedicated testSupport package subpath.
import type { ExecutionEnvironmentCapabilities } from "@bibcode/contracts";

export function makeTestExecutionEnvironmentCapabilities(
  overrides: Partial<ExecutionEnvironmentCapabilities> = {},
): ExecutionEnvironmentCapabilities {
  return {
    repositoryIdentity: false,
    terminalOrderedInput: false,
    terminalSizeOwnership: false,
    terminalImagePaste: false,
    pullRequestCreateOptions: false,
    worktreeCatalog: false,
    worktreeCatalogRefreshReason: false,
    vcsStatusSummary: false,
    gitManagerReads: false,
    gitManagerCommitOperations: false,
    gitManagerBranchSyncOperations: false,
    gitManagerStashMergeOperations: false,
    gitManagerMergeIntoOperations: false,
    gitManagerPartialStaging: false,
    gitManagerRewriteOperations: false,
    gitManagerTagOperations: false,
    gitManagerLiveSignal: false,
    gitManagerPullRequests: false,
    gitPullRequestBranchSelection: false,
    pullRequestsReads: false,
    pullRequestsMutations: false,
    activityProtocolVersion: null,
    remoteUpdateControl: false,
    remoteUpdateProgress: false,
    vcsCloneReattach: false,
    attachmentStaging: false,
    profiler: false,
    devicePreviews: false,
    ...overrides,
  };
}
