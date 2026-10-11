import type { FileDiffMetadata } from "@pierre/diffs";
import { useState, type ComponentProps, type ReactNode } from "react";
import { SourceControlCommitRow } from "~/components/SourceControlCommits";
import { LocalCommentAnnotation } from "~/components/files/LocalCommentAnnotation";
import { GitManagerInProgressStrip } from "~/components/gitManager/GitManagerInProgressStrip";
import { GitManagerChangeRow } from "~/components/gitManager/changes/GitManagerChangeRow";
import { GitManagerDiffPane } from "~/components/gitManager/changes/GitManagerDiffPane";
import { GitManagerBranchDialogs } from "~/components/gitManager/dialogs/GitManagerBranchDialogs";
import { GitManagerImageDiff } from "~/components/gitManager/diff/GitManagerImageDiff";
import { GitManagerCommitDetail } from "~/components/gitManager/history/GitManagerCommitDetail";
import { GitManagerMergeDialog } from "~/components/gitManager/merge/GitManagerMergeDialog";
import { CreatePullRequestOptions } from "~/components/gitManager/provider/CreatePullRequestOptions";
import { GitManagerCreatePullRequestDialog } from "~/components/gitManager/provider/GitManagerCreatePullRequestDialog";
import {
  EMPTY_CREATE_OPTIONS,
  type CreateOptionsState,
} from "~/components/gitManager/provider/GitManagerPullRequestPanel.logic";
import { GitManagerMultiCommitOperationDialog } from "~/components/gitManager/rewrite/GitManagerMultiCommitOperationDialog";
import type { GitManagerMultiCommitState } from "~/components/gitManager/rewrite/gitManagerMultiCommitOperation.logic";
import { GitManagerStagingGutter } from "~/components/gitManager/staging/GitManagerStagingGutter";
import {
  createLineSelection,
  withLineSelection,
} from "~/components/gitManager/staging/gitManagerLineSelection";
import { GitManagerStashDiff } from "~/components/gitManager/stash/GitManagerStashDiff";
import { GitManagerStashList } from "~/components/gitManager/stash/GitManagerStashList";
import { GitManagerTagsView } from "~/components/gitManager/tags/GitManagerTagsView";
import { GitManagerOperationBanner } from "~/components/gitManager/toolbar/GitManagerOperationBanner";
import { GitManagerSyncButton } from "~/components/gitManager/toolbar/GitManagerSyncButton";
import { resolveSyncState } from "~/components/gitManager/toolbar/syncButton.logic";
import { PullRequestsBaseBranchPicker } from "~/components/pullRequests/edit/PullRequestsBaseBranchPicker";
import { PullRequestsConfirmAction } from "~/components/pullRequests/edit/PullRequestsConfirmAction";
import { PullRequestsPicker } from "~/components/pullRequests/edit/PullRequestsPicker";
import { context, detail, review } from "~/components/pullRequests/detail/testFixtures";
import { PullRequestsDismissReview } from "~/components/pullRequests/review/PullRequestsDismissReview";
import { PullRequestsReviewPopover } from "~/components/pullRequests/review/PullRequestsReviewPopover";
import { PullRequestsApplySuggestionsButton } from "~/components/pullRequests/shared/PullRequestsSuggestionBlock";
import {
  PullRequestsActionsContext,
  type PullRequestsActions,
} from "~/components/pullRequests/usePullRequestsAction";
import { Button } from "~/components/ui/button";
import { Popover, PopoverTrigger } from "~/components/ui/popover";
import type { Fixture } from "./types";
import {
  environmentId,
  projectRef,
  scope,
  sha,
  otherSha,
  refs,
  remoteRefs,
  commit,
  entries,
  branch,
} from "./git-data";

// Standalone component fixtures. See the coverage file for nonvisual ports and public interactions.
const noop = () => undefined;
const finish = async () => undefined;
const allowed = { allowed: true, reason: null };
const denied = { allowed: false, reason: "Write access is required for this action." };
const actionPorts: PullRequestsActions = {
  requestKind: "pull request",
  pending: false,
  error: null,
  run: async () => {
    throw new Error("Component fixtures do not send repository mutations.");
  },
};
function Actions({ children }: { children: ReactNode }) {
  return <PullRequestsActionsContext value={actionPorts}>{children}</PullRequestsActionsContext>;
}
function Frame({ children }: { children: ReactNode }) {
  return (
    <div className="flex min-h-0 min-w-0 flex-col" style={{ height: 480, width: 920 }}>
      {children}
    </div>
  );
}
// Owned data copied from GitManagerStagingGutter.test.tsx. No renderer or visual child is replaced.
const fileDiff: FileDiffMetadata = {
  name: "file.ts",
  type: "change",
  hunks: [
    {
      collapsedBefore: 0,
      additionStart: 1,
      additionCount: 3,
      additionLines: 2,
      additionLineIndex: 0,
      deletionStart: 1,
      deletionCount: 3,
      deletionLines: 2,
      deletionLineIndex: 0,
      hunkContent: [
        { type: "change", deletions: 1, deletionLineIndex: 0, additions: 1, additionLineIndex: 0 },
        { type: "context", lines: 1, additionLineIndex: 1, deletionLineIndex: 1 },
        { type: "change", deletions: 1, deletionLineIndex: 2, additions: 1, additionLineIndex: 2 },
      ],
      splitLineStart: 0,
      splitLineCount: 3,
      unifiedLineStart: 0,
      unifiedLineCount: 5,
      noEOFCRDeletions: false,
      noEOFCRAdditions: false,
    },
  ],
  splitLineCount: 3,
  unifiedLineCount: 5,
  isPartial: true,
  deletionLines: ["old", "context", "third"],
  additionLines: ["new", "context", "fourth"],
};
function Gutter({
  area = "unstaged",
  disabledReason = null,
  diffKind = "text",
}: Pick<ComponentProps<typeof GitManagerStagingGutter>, "area" | "disabledReason" | "diffKind">) {
  const [selection, setSelection] = useState(() =>
    withLineSelection(createLineSelection("none", [0, 1, 3, 4]), 0, true),
  );
  return (
    <GitManagerStagingGutter
      fileDiff={fileDiff}
      selection={selection}
      onSelectionChange={setSelection}
      area={area}
      disabledReason={disabledReason}
      diffKind={diffKind}
      onApplySelection={noop}
      onRequestDiscard={noop}
    />
  );
}
function Changes({
  conflicted = false,
  submodule = false,
}: {
  conflicted?: boolean;
  submodule?: boolean;
}) {
  return (
    <GitManagerChangeRow
      row={{
        path: submodule ? "vendor/library" : "src/readable.ts",
        status: conflicted ? "conflicted" : "modified",
        area: "unstaged",
        insertions: 12,
        deletions: 4,
        inclusion: "partial",
        conflicted,
        submodule,
        disabledReason: submodule ? "Use whole-submodule inclusion." : null,
      }}
      selected
      onSelect={noop}
      onToggle={noop}
      onContextMenu={noop}
      onOpenExternal={noop}
    />
  );
}
function multiState(patch: Partial<GitManagerMultiCommitState> = {}): GitManagerMultiCommitState {
  return {
    step: "show-progress",
    kind: "rebase",
    selectedShas: [sha, otherSha],
    selectedBranch: "main",
    conflicts: [],
    continueBlocked: null,
    originalBranchTip: sha,
    operationEvent: { _tag: "started", operation: "rebase" },
    operationStartedExternally: false,
    abortRequested: false,
    ...patch,
  };
}
function Rewrite({
  state,
  disabledReason = null,
}: {
  state: GitManagerMultiCommitState;
  disabledReason?: string | null;
}) {
  return (
    <GitManagerMultiCommitOperationDialog
      state={state}
      disabledReason={disabledReason}
      onAdvance={noop}
      onCancel={noop}
      onConfirmAbort={noop}
    />
  );
}
function Stashes({ blocked = false }: { blocked?: boolean }) {
  return (
    <Frame>
      <GitManagerStashList
        scope={scope("stash-list")}
        projectRef={projectRef}
        entries={entries}
        selectedSha={sha}
        onSelectStash={noop}
        onApply={finish}
        onPop={finish}
        onDrop={finish}
        operationInFlight={false}
        blockedReasons={
          blocked
            ? [
                {
                  operation: "stash-apply",
                  code: "dirty-working-tree",
                  message: "Commit or stash your local changes before applying this stash.",
                },
              ]
            : []
        }
      />
    </Frame>
  );
}
function Options({
  providerKind = "github",
  defaultsState = "ready",
  disabled = false,
}: Pick<
  ComponentProps<typeof CreatePullRequestOptions>,
  "providerKind" | "defaultsState" | "disabled"
>) {
  const [value, setValue] = useState<CreateOptionsState>({
    ...EMPTY_CREATE_OPTIONS,
    assignees: [{ id: "local-author", label: "Local Author" }],
    reviewers: [{ id: "local-reviewer", label: "Local Reviewer" }],
    labels: [{ id: "readability", label: "readability" }],
  });
  return (
    <CreatePullRequestOptions
      scope={scope(`options-${providerKind}-${defaultsState}`)}
      providerKind={providerKind}
      defaults={null}
      defaultsState={defaultsState}
      onRetryDefaults={noop}
      value={value}
      onChange={setValue}
      disabled={disabled}
    />
  );
}
const prDetail = {
  ...detail,
  permissions: {
    ...detail.permissions,
    review: allowed,
    approve: allowed,
    requestChanges: allowed,
    revokeApproval: allowed,
    removeOwnChangeRequest: allowed,
    editPullRequest: allowed,
  },
};
function reviewPopover() {
  return (
    <Actions>
      <Popover open>
        <PopoverTrigger render={<Button variant="outline" />}>Review</PopoverTrigger>
        <PullRequestsReviewPopover
          detail={prDetail}
          context={context}
          projectRef={projectRef}
          onClose={noop}
        />
      </Popover>
    </Actions>
  );
}
const modulePath = (suffix: string) => `components/${suffix}.tsx`;
function fixture(
  id: string,
  title: string,
  module: string,
  owners: readonly string[],
  anchors: readonly string[],
  render: () => ReactNode,
): Fixture {
  return { id, title, modules: [modulePath(module)], owners, anchors, render };
}

export const gitFixtures: readonly Fixture[] = [
  fixture(
    "git-change-row",
    "Changed file and statistics",
    "gitManager/changes/GitManagerChangeRow",
    ["GitManagerChangeRow"],
    ["readable.ts", "src", "+12", "-4"],
    () => <Changes />,
  ),
  fixture(
    "git-change-conflict",
    "Conflicted changed file",
    "gitManager/changes/GitManagerChangeRow",
    ["GitManagerChangeRow"],
    ["readable.ts", "Conflict"],
    () => <Changes conflicted />,
  ),
  fixture(
    "git-change-submodule",
    "Submodule changed file",
    "gitManager/changes/GitManagerChangeRow",
    ["GitManagerChangeRow"],
    ["library", "Submodule"],
    () => <Changes submodule />,
  ),
  fixture(
    "git-progress-rebase",
    "Rebase progress",
    "gitManager/GitManagerInProgressStrip",
    ["GitManagerInProgressStrip"],
    ["Rebase underway", "Step 2 of 5"],
    () => (
      <GitManagerInProgressStrip
        operation={{ kind: "rebase", current: 2, total: 5 }}
        blocked={null}
        onContinue={noop}
        onAbort={noop}
      />
    ),
  ),
  fixture(
    "git-progress-conflict",
    "Blocked operation progress",
    "gitManager/GitManagerInProgressStrip",
    ["GitManagerInProgressStrip"],
    ["Resolve all conflicts before continuing."],
    () => (
      <GitManagerInProgressStrip
        operation={{ kind: "cherry-pick", current: 1, total: 3 }}
        blocked={null}
        continueDisabledReason="Resolve all conflicts before continuing."
        onContinue={noop}
        onAbort={noop}
      />
    ),
  ),
  fixture(
    "git-images-two-up",
    "Image captions and missing image",
    "gitManager/diff/GitManagerImageDiff",
    ["ImagePane"],
    ["Before", "After", "Before image unavailable", "After image unavailable"],
    () => (
      <Frame>
        <GitManagerImageDiff before={null} after={null} mode="two-up" onModeChange={noop} />
      </Frame>
    ),
  ),
  fixture(
    "git-staging-partial",
    "Selected line and changed run controls",
    "gitManager/staging/GitManagerStagingGutter",
    ["GutterLineControl", "GutterRunControl", "GitManagerTextStagingGutter"],
    ["Changed lines", "Stage selected lines", "Discard selected lines…", "1", "3"],
    () => (
      <Frame>
        <Gutter disabledReason={null} />
      </Frame>
    ),
  ),
  fixture(
    "git-staging-unstage",
    "Selected staged lines",
    "gitManager/staging/GitManagerStagingGutter",
    ["GutterLineControl", "GutterRunControl", "GitManagerTextStagingGutter"],
    ["Unstage selected lines", "Changed lines"],
    () => (
      <Frame>
        <Gutter area="staged" disabledReason={null} />
      </Frame>
    ),
  ),
  fixture(
    "git-staging-disabled",
    "Staging disabled explanation",
    "gitManager/staging/GitManagerStagingGutter",
    ["GutterLineControl", "GutterRunControl", "GitManagerTextStagingGutter"],
    ["A commit is in progress.", "Changed lines"],
    () => (
      <Frame>
        <Gutter disabledReason="A commit is in progress." />
      </Frame>
    ),
  ),
  fixture(
    "git-staging-binary",
    "Whole-file binary staging explanation",
    "gitManager/staging/GitManagerStagingGutter",
    ["GitManagerTextStagingGutter"],
    ["Binary files support whole-file staging only."],
    () => <Gutter disabledReason={null} diffKind="binary" />,
  ),
  fixture(
    "git-stash-list",
    "Stash row selector and file counts",
    "gitManager/stash/GitManagerStashList",
    ["GitManagerStashRowView"],
    ["stash@{0}", "Typography work before branch switch", "2 files", "stash@{1}", "1 file"],
    () => <Stashes />,
  ),
  fixture(
    "git-stash-list-blocked",
    "Blocked stash row",
    "gitManager/stash/GitManagerStashList",
    ["GitManagerStashRowView"],
    ["stash@{0}", "2 files"],
    () => <Stashes blocked />,
  ),
  fixture(
    "git-history-metadata",
    "Author, committer and changed-file metadata",
    "gitManager/history/GitManagerCommitDetail",
    ["IdentityBadge", "ChangedFileRow", "GitManagerCommitDetail"],
    [
      "Author",
      "Committer",
      "LA",
      "LC",
      "HEAD -> main",
      "src/readable.ts",
      "Select a changed file to view its diff.",
    ],
    () => (
      <Frame>
        <GitManagerCommitDetail
          environmentId={environmentId}
          cwd={scope("history-metadata").cwd}
          commit={commit}
          selectedFilePath={null}
          onSelectFile={noop}
        />
      </Frame>
    ),
  ),
  fixture(
    "git-history-raw",
    "Commit raw-patch explanation",
    "gitManager/history/GitManagerCommitDetail",
    ["IdentityBadge", "ChangedFileRow", "GitManagerCommitDetail"],
    ["Author", "Committer", "src/readable.ts", "component-fixture raw patch"],
    () => (
      <Frame>
        <GitManagerCommitDetail
          environmentId={environmentId}
          cwd={scope("history-raw").cwd}
          commit={commit}
          selectedFilePath="src/readable.ts"
          onSelectFile={noop}
        />
      </Frame>
    ),
  ),
  fixture(
    "git-history-unrenderable",
    "Commit rendering limit",
    "gitManager/history/GitManagerCommitDetail",
    ["GitManagerCommitDetail"],
    ["This diff is too large to render safely."],
    () => (
      <Frame>
        <GitManagerCommitDetail
          environmentId={environmentId}
          cwd={scope("history-unrenderable").cwd}
          commit={commit}
          selectedFilePath="src/readable.ts"
          onSelectFile={noop}
        />
      </Frame>
    ),
  ),
  fixture(
    "git-diff-raw",
    "Working-tree area controls and raw patch",
    "gitManager/changes/GitManagerDiffPane",
    ["GitManagerDiffPane"],
    ["src/readable.ts", "Unstaged", "Staged", "component-fixture raw patch"],
    () => (
      <Frame>
        <GitManagerDiffPane
          scope={scope("diff-raw")}
          projectRef={projectRef}
          path="src/readable.ts"
          availableAreas={["unstaged", "staged"]}
          mutationBusy={false}
          partialStagingDisabledReason={null}
        />
      </Frame>
    ),
  ),
  fixture(
    "git-diff-unrenderable",
    "Working-tree rendering limit",
    "gitManager/changes/GitManagerDiffPane",
    ["GitManagerDiffPane"],
    ["This diff cannot be rendered safely."],
    () => (
      <Frame>
        <GitManagerDiffPane
          scope={scope("diff-unrenderable")}
          projectRef={projectRef}
          path="src/readable.ts"
          availableAreas={["unstaged"]}
          mutationBusy={false}
          partialStagingDisabledReason={null}
        />
      </Frame>
    ),
  ),
  fixture(
    "git-stash-diff-raw",
    "Stash file rows and raw diff",
    "gitManager/stash/GitManagerStashDiff",
    ["StashFileRow", "GitManagerStashDiff"],
    ["src/readable.ts", "+12 −4", "component-fixture raw patch"],
    () => (
      <Frame>
        <GitManagerStashDiff
          scope={scope("stash-diff-raw")}
          projectRef={projectRef}
          entries={entries}
          selectedStashSha={sha}
          selectedPath="src/readable.ts"
          onSelectPath={noop}
          onRefreshStashes={noop}
        />
      </Frame>
    ),
  ),
  fixture(
    "git-stash-diff-large",
    "Large stash diff limit",
    "gitManager/stash/GitManagerStashDiff",
    ["StashFileRow", "GitManagerStashDiff"],
    ["src/readable.ts", "This stash diff is too large to render."],
    () => (
      <Frame>
        <GitManagerStashDiff
          scope={scope("stash-diff-large")}
          projectRef={projectRef}
          entries={entries}
          selectedStashSha={sha}
          selectedPath="src/readable.ts"
          onSelectPath={noop}
          onRefreshStashes={noop}
        />
      </Frame>
    ),
  ),
  fixture(
    "git-tags-local",
    "Local tag names and SHAs",
    "gitManager/tags/GitManagerTagsView",
    ["TagSection", "LocalTagRows"],
    ["Local", "v0.8.2", "aaaaaaa"],
    () => (
      <GitManagerTagsView
        scope={scope("tags-local")}
        tags={[branch("v0.8.2")]}
        remotes={[]}
        tagDisabledReason={null}
        collapsedSections={[]}
        onSectionCollapsedChange={noop}
        onTagAction={noop}
      />
    ),
  ),
  fixture(
    "git-tags-remote",
    "Local and remote tag presence",
    "gitManager/tags/GitManagerTagsView",
    ["TagSection", "LocalTagRows", "RemoteTagsSection"],
    ["Local", "Remote origin", "v0.8.2", "v0.9.0", "not fetched"],
    () => (
      <GitManagerTagsView
        scope={scope("tags-remote")}
        tags={[branch("v0.8.2")]}
        remotes={["origin"]}
        tagDisabledReason={null}
        collapsedSections={[]}
        onSectionCollapsedChange={noop}
        onTagAction={noop}
      />
    ),
  ),
  fixture(
    "git-tags-remote-unavailable",
    "Remote tag availability explanation",
    "gitManager/tags/GitManagerTagsView",
    ["RemoteTagsSection"],
    ["Remote origin", "Remote tags are unavailable in this fixture.", "Retry"],
    () => (
      <GitManagerTagsView
        scope={scope("tags-remote-unavailable")}
        tags={[]}
        remotes={["origin"]}
        tagDisabledReason={null}
        collapsedSections={[]}
        onSectionCollapsedChange={noop}
        onTagAction={noop}
      />
    ),
  ),
  fixture(
    "git-rewrite-branches",
    "Default, recent and other branch groups",
    "gitManager/rewrite/GitManagerMultiCommitOperationDialog",
    ["BranchChoiceGroup", "GitManagerMultiCommitOperationDialog"],
    [
      "Default",
      "Recent",
      "Other",
      "main",
      "feature/typography",
      "release/next",
      "Ahead 3",
      "Behind 2",
    ],
    () => (
      <Rewrite
        state={multiState({
          step: "choose-branch",
          refs,
          recentNames: ["feature/typography"],
          mergePreview: {
            _tag: "clean",
            source: "feature/typography",
            current: "main",
            ahead: 3,
            behind: 2,
          },
        })}
      />
    ),
  ),
  fixture(
    "git-rewrite-warning",
    "History rewrite warning",
    "gitManager/rewrite/GitManagerMultiCommitOperationDialog",
    ["GitManagerMultiCommitOperationDialog"],
    ["History will be rewritten", "Rewrite History"],
    () => <Rewrite state={multiState({ step: "warn-force-push" })} />,
  ),
  fixture(
    "git-rewrite-disabled",
    "Rewrite disabled explanation",
    "gitManager/rewrite/GitManagerMultiCommitOperationDialog",
    ["GitManagerMultiCommitOperationDialog"],
    ["This environment does not support Git Manager rewrite operations."],
    () => (
      <Rewrite
        state={multiState({ step: "choose-branch", refs })}
        disabledReason="This environment does not support Git Manager rewrite operations."
      />
    ),
  ),
  fixture(
    "git-operation-output",
    "Running operation output and help",
    "gitManager/toolbar/GitManagerOperationBanner",
    ["GitManagerOperationBanner"],
    [
      "Running fetch…",
      "Output (1)",
      "Output arrives in chunks after each Git command completes.",
      "Fetched component-fixture refs",
    ],
    () => (
      <GitManagerOperationBanner
        operation={{
          _tag: "output",
          operation: "fetch",
          stream: "stdout",
          text: "Fetched component-fixture refs\n",
        }}
        onCancel={noop}
      />
    ),
  ),
  fixture(
    "git-operation-failure",
    "Failed operation and code",
    "gitManager/toolbar/GitManagerOperationBanner",
    ["GitManagerOperationBanner"],
    ["fetch failed", "Remote denied this fixture operation.", "permission-denied"],
    () => (
      <GitManagerOperationBanner
        operation={{
          _tag: "failed",
          operation: "fetch",
          code: "permission-denied",
          message: "Remote denied this fixture operation.",
          blocked: null,
        }}
        onCancel={noop}
      />
    ),
  ),
  fixture(
    "git-sync-counts",
    "Sync ahead and behind counts",
    "gitManager/toolbar/GitManagerSyncButton",
    ["GitManagerSyncButton"],
    ["Pull origin", "3", "2"],
    () => (
      <GitManagerSyncButton
        state={resolveSyncState({
          isOperationRunning: false,
          hasRemote: true,
          isUnborn: false,
          isDetached: false,
          aheadBehind: { ahead: 3, behind: 2 },
          forcePushRecommended: false,
        })}
        currentBranchName="feature/typography"
        remote="origin"
        blockedReason={null}
        disabledReason={null}
        onOperation={noop}
      />
    ),
  ),
  fixture(
    "git-sync-force-dialog",
    "Force push confirmation help",
    "gitManager/toolbar/GitManagerSyncButton",
    ["GitManagerSyncButton"],
    ["Force Push feature/typography?", "Force push with lease"],
    () => (
      <GitManagerSyncButton
        state={resolveSyncState({
          isOperationRunning: false,
          hasRemote: true,
          isUnborn: false,
          isDetached: false,
          aheadBehind: { ahead: 3, behind: 0 },
          forcePushRecommended: true,
        })}
        currentBranchName="feature/typography"
        remote="origin"
        blockedReason={null}
        disabledReason={null}
        onOperation={noop}
      />
    ),
  ),
  fixture(
    "git-source-control-row",
    "Source Control commit row",
    "SourceControlCommits",
    ["SourceControlCommitRow"],
    ["aaaaaaa", "Keep secondary Git text readable", "Local Author", "2h ago"],
    () => (
      <SourceControlCommitRow commit={commit} nowMs={commit.authoredAtMs + 2 * 60 * 60 * 1000} />
    ),
  ),
  fixture(
    "git-local-comment",
    "Saved local comment range",
    "files/LocalCommentAnnotation",
    ["LocalCommentAnnotation"],
    ["Local comment", "L1–L3", "Keep the range label readable."],
    () => (
      <LocalCommentAnnotation
        kind="comment"
        rangeLabel="L1–L3"
        text="Keep the range label readable."
        onCancel={noop}
        onComment={noop}
        onDelete={noop}
      />
    ),
  ),
  fixture(
    "git-local-comment-draft",
    "Local comment draft range",
    "files/LocalCommentAnnotation",
    ["LocalCommentAnnotation"],
    ["Comment on lines L4–L6", "Comment", "Cancel"],
    () => (
      <LocalCommentAnnotation
        kind="draft"
        rangeLabel="L4–L6"
        text=""
        onCancel={noop}
        onComment={noop}
        onDelete={noop}
      />
    ),
  ),
  fixture(
    "git-merge-preview",
    "Merge preview and source group",
    "gitManager/merge/GitManagerMergeDialog",
    ["GitManagerMergeDialog"],
    ["feature/typography", "Remote", "Ahead 3", "Behind 2"],
    () => (
      <GitManagerMergeDialog
        open
        scope={scope("merge-preview")}
        projectRef={projectRef}
        refs={refs}
        remoteRefs={remoteRefs}
        recentNames={["feature/typography"]}
        mergeIntoAvailable
        onOpenChange={noop}
      />
    ),
  ),
  fixture(
    "git-merge-target-popup",
    "Merge Into picker scroller",
    "gitManager/merge/GitManagerMergeDialog",
    ["GitManagerMergeDialog"],
    ["main", "feature/typography", "release/next"],
    () => (
      <GitManagerMergeDialog
        open
        scope={scope("merge-target-popup")}
        projectRef={projectRef}
        refs={refs}
        remoteRefs={remoteRefs}
        mergeIntoAvailable
        onOpenChange={noop}
      />
    ),
  ),
  fixture(
    "git-create-options-users",
    "Create-request people picker scroller",
    "gitManager/provider/CreatePullRequestOptions",
    ["OptionPicker"],
    ["Local Author", "Local Reviewer"],
    () => <Options providerKind="github" defaultsState="ready" disabled={false} />,
  ),
  fixture(
    "git-create-options-labels",
    "Create-request labels picker scroller",
    "gitManager/provider/CreatePullRequestOptions",
    ["OptionPicker"],
    ["readability", "typography"],
    () => <Options providerKind="github" defaultsState="ready" disabled={false} />,
  ),
  fixture(
    "git-create-options-milestones",
    "Create-request milestone picker scroller",
    "gitManager/provider/CreatePullRequestOptions",
    ["OptionPicker"],
    ["Release 0.9", "Readability pass"],
    () => <Options providerKind="github" defaultsState="ready" disabled={false} />,
  ),
  fixture(
    "git-create-options-unknown",
    "Unknown project merge settings",
    "gitManager/provider/CreatePullRequestOptions",
    ["OptionPicker"],
    ["Assignee", "Reviewer", "Loading the project's merge settings…"],
    () => <Options providerKind="gitlab" defaultsState="loading" disabled={false} />,
  ),
  fixture(
    "git-create-request",
    "Request review scroller and branch help",
    "gitManager/provider/GitManagerCreatePullRequestDialog",
    ["CreatePullRequestReviewDialog"],
    [
      "Source branch",
      "Target branch (required)",
      "Branch publication",
      "Choose the branch on origin that should receive these changes.",
    ],
    () => (
      <GitManagerCreatePullRequestDialog
        open
        scope={scope("create-request")}
        onOpenChange={noop}
        onSettled={noop}
        providerHint={{ kind: "github", baseUrl: "https://github.com" }}
      />
    ),
  ),
  fixture(
    "git-create-request-branches",
    "Request BranchPicker actual list",
    "gitManager/provider/GitManagerCreatePullRequestDialog",
    ["CreatePullRequestReviewDialog", "BranchPicker"],
    ["release/next", "main"],
    () => (
      <GitManagerCreatePullRequestDialog
        open
        scope={scope("create-request-branches")}
        onOpenChange={noop}
        onSettled={noop}
        providerHint={{ kind: "github", baseUrl: "https://github.com" }}
      />
    ),
  ),
  fixture(
    "git-new-branch-popup",
    "New branch source groups",
    "gitManager/dialogs/GitManagerBranchDialogs",
    ["CreateBranchDialog"],
    ["Local", "Remote", "main", "origin/release/next"],
    () => (
      <GitManagerBranchDialogs
        dialog={{ kind: "create", baseBranch: "main", baseCommit: null }}
        refs={refs}
        remoteRefs={remoteRefs}
        busy={false}
        errorMessage={null}
        onClose={noop}
        onSubmit={finish}
      />
    ),
  ),
  fixture(
    "pr-apply-suggestions-popup",
    "Apply suggestion commit message",
    "pullRequests/shared/PullRequestsSuggestionBlock",
    ["PullRequestsApplySuggestionsButton"],
    ["Apply suggested changes", "Commit message (optional)", "Apply 2 suggestions"],
    () => (
      <Actions>
        <PullRequestsApplySuggestionsButton
          suggestionIds={["suggestion-a", "suggestion-b"]}
          permission={allowed}
          label="Apply suggestions"
        />
      </Actions>
    ),
  ),
  fixture(
    "pr-base-branch-popup",
    "Request base branch vocabulary",
    "pullRequests/edit/PullRequestsBaseBranchPicker",
    ["BranchChoices"],
    ["main", "release/next"],
    () => (
      <Actions>
        <PullRequestsBaseBranchPicker
          detail={prDetail}
          scope={scope("pr-base-branch-popup")}
          projectRef={projectRef}
        />
      </Actions>
    ),
  ),
  fixture(
    "pr-confirm-action",
    "Confirm request action",
    "pullRequests/edit/PullRequestsConfirmAction",
    ["PullRequestsConfirmAction"],
    ["Closing retains the conversation and commits.", "Close request", "Cancel"],
    () => (
      <Actions>
        <PullRequestsConfirmAction
          title="Close pull request"
          confirmLabel="Close request"
          permission={allowed}
          onConfirm={finish}
          onClose={noop}
        >
          <p>Closing retains the conversation and commits.</p>
        </PullRequestsConfirmAction>
      </Actions>
    ),
  ),
  fixture(
    "pr-picker-labels",
    "Request label edit scroller",
    "pullRequests/edit/PullRequestsPicker",
    ["PickerDialog"],
    ["Edit Labels", "readability", "typography", "Done"],
    () => (
      <Actions>
        <PullRequestsPicker
          scope={scope("pr-picker-labels")}
          label="Labels"
          kind="labels"
          multiple
          selected={["readability"]}
          permission={allowed}
          onChange={finish}
        />
      </Actions>
    ),
  ),
  fixture(
    "pr-picker-milestone",
    "Request milestone edit scroller",
    "pullRequests/edit/PullRequestsPicker",
    ["PickerDialog"],
    ["Edit Milestone", "Release 0.9", "Readability pass", "Clear"],
    () => (
      <Actions>
        <PullRequestsPicker
          scope={scope("pr-picker-milestone")}
          label="Milestone"
          kind="milestones"
          multiple={false}
          selected={["release-09"]}
          permission={allowed}
          onChange={finish}
        />
      </Actions>
    ),
  ),
  fixture(
    "pr-dismiss-review",
    "Dismiss review message",
    "pullRequests/review/PullRequestsDismissReview",
    ["PullRequestsDismissReview"],
    ["Dismiss review", "Message (required)", "Include a message explaining why."],
    () => (
      <Actions>
        <PullRequestsDismissReview
          review={{ ...review, canDismiss: true }}
          permission={allowed}
          host="github.com"
        />
      </Actions>
    ),
  ),
  fixture(
    "pr-review-popup",
    "Review summary and available actions",
    "pullRequests/review/PullRequestsReviewPopover",
    ["PullRequestsReviewPopover"],
    [
      "Submit review",
      "Review summary",
      "Comment",
      "Approve",
      "Request changes",
      "Revoke approval",
      "Remove my change request",
    ],
    reviewPopover,
  ),
  fixture(
    "pr-review-denied",
    "Review actions without write permission",
    "pullRequests/review/PullRequestsReviewPopover",
    ["PullRequestsReviewPopover"],
    ["Review summary", "Submit review"],
    () => (
      <Actions>
        <Popover open>
          <PopoverTrigger render={<Button variant="outline" />}>Review</PopoverTrigger>
          <PullRequestsReviewPopover
            detail={{
              ...prDetail,
              permissions: { ...prDetail.permissions, approve: denied, requestChanges: denied },
            }}
            context={context}
            projectRef={projectRef}
            onClose={noop}
          />
        </Popover>
      </Actions>
    ),
  ),
];
