import {
  CheckpointRef,
  EnvironmentId,
  ProjectId,
  ProviderInstanceId,
  ThreadId,
  TurnId,
  type ReviewDiffPreviewResult,
  type SourceControlDiscoveryResult,
  type VcsStatusResult,
} from "@bibcode/contracts";
import { createModelSelection } from "@bibcode/shared/model";
import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";
import type { Thread } from "../../src/types";

export const remainingEnvironmentId = EnvironmentId.make("typography-remaining-owned");
export const remainingThreadRef = {
  environmentId: remainingEnvironmentId,
  threadId: ThreadId.make("typography-diff-owned"),
};
export const remainingCwd = "/component-fixtures/issue27/remaining-diff";
export const publishCwd = "/component-fixtures/issue27/publish-repository";
export const remainingPatch =
  "diff --git a/src/typography.ts b/src/typography.ts\nindex 1111111..2222222 100644\n--- a/src/typography.ts\n+++ b/src/typography.ts\n@@ -1 +1 @@\n-export const label = 'before';\n+export const label = 'Readable fixture diff';\n";
export const remainingThread: Thread = {
  environmentId: remainingEnvironmentId,
  id: remainingThreadRef.threadId,
  projectId: ProjectId.make("typography-remaining-project"),
  title: "Typography diff thread",
  modelSelection: createModelSelection(ProviderInstanceId.make("codex"), "fixture-model"),
  runtimeMode: "full-access",
  interactionMode: "default",
  branch: "feature/typography",
  worktreePath: remainingCwd,
  latestTurn: null,
  createdAt: "2026-10-10T12:00:00.000Z",
  updatedAt: "2026-10-10T12:00:00.000Z",
  archivedAt: null,
  deletedAt: null,
  messages: [],
  proposedPlans: [],
  activities: [],
  session: null,
  checkpoints: [1, 2].map((count) => ({
    turnId: TurnId.make(`fixture-typography-turn-${count}`),
    checkpointTurnCount: count,
    checkpointRef: CheckpointRef.make(`fixture-typography-ref-${count}`),
    status: "ready",
    files: [],
    assistantMessageId: null,
    completedAt: `2026-10-10T12:0${count}:00.000Z`,
  })),
};
export const remainingDiff: ReviewDiffPreviewResult = {
  cwd: remainingCwd,
  generatedAt: DateTime.makeUnsafe("2026-10-10T12:00:00.000Z"),
  sources: [
    {
      id: "fixture-branch",
      kind: "branch-range",
      title: "Branch changes",
      baseRef: "main",
      headRef: "feature/typography",
      diff: remainingPatch,
      diffHash: "fixture-branch-hash",
      truncated: true,
    },
    {
      id: "fixture-working-tree",
      kind: "working-tree",
      title: "Working tree",
      baseRef: null,
      headRef: "feature/typography",
      diff: remainingPatch,
      diffHash: "fixture-working-hash",
      truncated: false,
    },
  ],
};
export const publishStatus: VcsStatusResult = {
  isRepo: true,
  hasPrimaryRemote: false,
  isDefaultRef: false,
  refName: "feature/typography",
  hasWorkingTreeChanges: false,
  workingTree: { files: [], insertions: 0, deletions: 0 },
  hasUpstream: false,
  aheadCount: 0,
  behindCount: 0,
  pr: null,
};
export const publishDiscovery: SourceControlDiscoveryResult = {
  versionControlSystems: [],
  sourceControlProviders: [
    {
      kind: "github",
      label: "GitHub",
      executable: "gh",
      status: "available",
      version: Option.some("1.0.0"),
      installHint: "Owned fixture GitHub provider",
      detail: Option.none(),
      auth: {
        status: "authenticated",
        account: Option.none(),
        host: Option.some("github.com"),
        detail: Option.none(),
      },
    },
    {
      kind: "gitlab",
      label: "GitLab",
      executable: "glab",
      status: "missing",
      version: Option.none(),
      installHint: "Install the GitLab CLI",
      detail: Option.some("Owned fixture CLI is unavailable."),
      auth: {
        status: "unknown",
        account: Option.none(),
        host: Option.none(),
        detail: Option.some("Owned fixture auth is unavailable."),
      },
    },
    {
      kind: "bitbucket",
      label: "Bitbucket",
      status: "missing",
      version: Option.none(),
      installHint: "Configure the Bitbucket provider",
      detail: Option.none(),
      auth: {
        status: "unknown",
        account: Option.none(),
        host: Option.none(),
        detail: Option.none(),
      },
    },
    {
      kind: "azure-devops",
      label: "Azure DevOps",
      executable: "az",
      status: "missing",
      version: Option.none(),
      installHint: "Install the Azure CLI",
      detail: Option.none(),
      auth: {
        status: "unknown",
        account: Option.none(),
        host: Option.none(),
        detail: Option.none(),
      },
    },
  ],
};
