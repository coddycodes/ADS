import type {
  GitRunStackedActionResult,
  GitStackedAction,
  VcsStatusResult,
} from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";
import {
  buildGitActionProgressStages,
  buildMenuItems,
  type GitActionMenuItem,
  getGitActionDisabledReason,
  requiresDefaultBranchConfirmation,
  resolveDefaultBranchActionDialogCopy,
  resolveLiveThreadBranchUpdate,
  resolveQuickAction,
  resolveThreadBranchUpdate,
} from "./gitActions.ts";

function status(overrides: Partial<VcsStatusResult> = {}): VcsStatusResult {
  return {
    isRepo: true,
    hasPrimaryRemote: true,
    isDefaultRef: false,
    refName: "feature/test",
    hasWorkingTreeChanges: false,
    workingTree: {
      files: [],
      insertions: 0,
      deletions: 0,
    },
    hasUpstream: true,
    aheadCount: 0,
    behindCount: 0,
    pr: null,
    ...overrides,
  };
}

const openPr = {
  number: 7,
  title: "Open PR",
  url: "https://example.com/pr/7",
  baseRef: "main",
  headRef: "feature/test",
  state: "open" as const,
};

function menuItem(overrides: Partial<GitActionMenuItem> = {}): GitActionMenuItem {
  return {
    id: "commit",
    label: "Commit",
    disabled: true,
    icon: "commit",
    kind: "open_dialog",
    dialogAction: "commit",
    ...overrides,
  };
}

describe("buildGitActionProgressStages", () => {
  it("returns only the push stage for a push action, defaulting the label when no target", () => {
    expect(
      buildGitActionProgressStages({
        action: "push",
        hasCustomCommitMessage: false,
        hasWorkingTreeChanges: false,
      }),
    ).toEqual(["Pushing..."]);
  });

  it("names the push target when provided", () => {
    expect(
      buildGitActionProgressStages({
        action: "push",
        hasCustomCommitMessage: false,
        hasWorkingTreeChanges: false,
        pushTarget: "origin/feature/test",
      }),
    ).toEqual(["Pushing to origin/feature/test..."]);
  });

  it("prepends a push stage for create_pr when a push is still needed", () => {
    expect(
      buildGitActionProgressStages({
        action: "create_pr",
        hasCustomCommitMessage: false,
        hasWorkingTreeChanges: false,
        shouldPushBeforePr: true,
      }),
    ).toEqual([
      "Pushing...",
      "Preparing PR...",
      "Generating PR content...",
      "Creating GitHub pull request...",
    ]);
  });

  it("emits only PR stages for create_pr when the push can be skipped", () => {
    expect(
      buildGitActionProgressStages({
        action: "create_pr",
        hasCustomCommitMessage: false,
        hasWorkingTreeChanges: false,
        shouldPushBeforePr: false,
      }),
    ).toEqual(["Preparing PR...", "Generating PR content...", "Creating GitHub pull request..."]);
  });

  it("includes the message-generation stage for a commit without a custom message", () => {
    expect(
      buildGitActionProgressStages({
        action: "commit",
        hasCustomCommitMessage: false,
        hasWorkingTreeChanges: false,
      }),
    ).toEqual(["Generating commit message...", "Committing..."]);
  });

  it("adds a feature-branch stage and skips message generation with a custom message", () => {
    expect(
      buildGitActionProgressStages({
        action: "commit",
        hasCustomCommitMessage: true,
        hasWorkingTreeChanges: false,
        featureBranch: true,
      }),
    ).toEqual(["Preparing feature branch...", "Committing..."]);
  });

  it("skips commit stages for commit_push when the working tree is clean", () => {
    expect(
      buildGitActionProgressStages({
        action: "commit_push",
        hasCustomCommitMessage: false,
        hasWorkingTreeChanges: false,
        pushTarget: "origin/feature/test",
      }),
    ).toEqual(["Pushing to origin/feature/test..."]);
  });

  it("includes commit and push stages for commit_push when the tree is dirty", () => {
    expect(
      buildGitActionProgressStages({
        action: "commit_push",
        hasCustomCommitMessage: true,
        hasWorkingTreeChanges: true,
        pushTarget: "origin/feature/test",
      }),
    ).toEqual(["Committing...", "Pushing to origin/feature/test..."]);
  });

  it("emits the full pipeline for commit_push_pr", () => {
    expect(
      buildGitActionProgressStages({
        action: "commit_push_pr",
        hasCustomCommitMessage: false,
        hasWorkingTreeChanges: true,
        featureBranch: true,
        pushTarget: "origin/feature/test",
      }),
    ).toEqual([
      "Preparing feature branch...",
      "Generating commit message...",
      "Committing...",
      "Pushing to origin/feature/test...",
      "Preparing PR...",
      "Generating PR content...",
      "Creating GitHub pull request...",
    ]);
  });
});

describe("buildMenuItems", () => {
  it("returns no items when git status is unavailable", () => {
    expect(buildMenuItems(null, false)).toEqual([]);
  });

  it("disables every action while busy", () => {
    const items = buildMenuItems(status({ hasWorkingTreeChanges: true, aheadCount: 3 }), true);
    expect(items.every((item) => item.disabled)).toBe(true);
  });

  it("enables commit when there are working-tree changes", () => {
    const items = buildMenuItems(status({ hasWorkingTreeChanges: true }), false);
    expect(items.find((item) => item.id === "commit")?.disabled).toBe(false);
  });

  it("enables push and create PR for a clean, ahead branch with an upstream", () => {
    const items = buildMenuItems(status({ aheadCount: 2 }), false);
    expect(items.find((item) => item.id === "push")?.disabled).toBe(false);
    const pr = items.find((item) => item.id === "pr");
    expect(pr).toMatchObject({ label: "Create PR", disabled: false, kind: "open_dialog" });
  });

  it("shows a View PR item when a PR is open", () => {
    const items = buildMenuItems(status({ aheadCount: 2, pr: openPr }), false);
    expect(items.find((item) => item.id === "pr")).toMatchObject({
      label: "View PR",
      disabled: false,
      kind: "open_pr",
    });
  });

  it("allows pushing without an upstream when an origin remote exists", () => {
    const items = buildMenuItems(status({ hasUpstream: false, aheadCount: 1 }), false, true);
    expect(items.find((item) => item.id === "push")?.disabled).toBe(false);
  });

  it("blocks pushing without an upstream when there is no origin remote", () => {
    const items = buildMenuItems(status({ hasUpstream: false, aheadCount: 1 }), false, false);
    expect(items.find((item) => item.id === "push")?.disabled).toBe(true);
  });

  it("blocks push and create PR when the branch is behind", () => {
    const items = buildMenuItems(status({ aheadCount: 1, behindCount: 1 }), false);
    expect(items.find((item) => item.id === "push")?.disabled).toBe(true);
    expect(items.find((item) => item.id === "pr")?.disabled).toBe(true);
  });

  it("keeps create PR disabled when the branch has no commits ahead", () => {
    const items = buildMenuItems(status(), false);
    expect(items.find((item) => item.id === "pr")?.disabled).toBe(true);
  });
});

describe("resolveQuickAction", () => {
  it("reports an in-progress action while busy", () => {
    expect(resolveQuickAction(status(), true)).toEqual({
      label: "Commit",
      disabled: true,
      kind: "show_hint",
      hint: "Git action in progress.",
    });
  });

  it("reports unavailable status when there is no git status", () => {
    expect(resolveQuickAction(null, false)).toEqual({
      label: "Commit",
      disabled: true,
      kind: "show_hint",
      hint: "Git status is unavailable.",
    });
  });

  it("asks to create a branch when HEAD is detached", () => {
    expect(resolveQuickAction(status({ refName: null }), false)).toMatchObject({
      kind: "show_hint",
      hint: "Create and checkout a branch before pushing or opening a PR.",
    });
  });

  it("commits only when dirty without an upstream or origin remote", () => {
    expect(
      resolveQuickAction(
        status({ hasWorkingTreeChanges: true, hasUpstream: false }),
        false,
        false,
        false,
      ),
    ).toMatchObject({ kind: "run_action", action: "commit", disabled: false });
  });

  it("commits and pushes when dirty with an open PR", () => {
    expect(
      resolveQuickAction(status({ hasWorkingTreeChanges: true, pr: openPr }), false),
    ).toMatchObject({ kind: "run_action", action: "commit_push", label: "Commit & push" });
  });

  it("commits and pushes when dirty on the default branch", () => {
    expect(resolveQuickAction(status({ hasWorkingTreeChanges: true }), false, true)).toMatchObject({
      kind: "run_action",
      action: "commit_push",
    });
  });

  it("commits, pushes and creates a PR when dirty on a feature branch", () => {
    expect(resolveQuickAction(status({ hasWorkingTreeChanges: true }), false)).toMatchObject({
      kind: "run_action",
      action: "commit_push_pr",
      label: "Commit, push & PR",
    });
  });

  it("opens the PR when clean without origin, not ahead, and a PR is open", () => {
    expect(
      resolveQuickAction(
        status({ hasUpstream: false, aheadCount: 0, pr: openPr }),
        false,
        false,
        false,
      ),
    ).toEqual({ label: "View PR", disabled: false, kind: "open_pr" });
  });

  it("hints to add an origin remote when clean without an upstream or origin", () => {
    expect(
      resolveQuickAction(status({ hasUpstream: false, aheadCount: 0 }), false, false, false),
    ).toMatchObject({
      kind: "show_hint",
      hint: 'Add an "origin" remote before pushing or creating a PR.',
    });
  });

  it("opens the PR when clean without an upstream, not ahead, with an origin and a PR", () => {
    expect(
      resolveQuickAction(status({ hasUpstream: false, aheadCount: 0, pr: openPr }), false),
    ).toEqual({ label: "View PR", disabled: false, kind: "open_pr" });
  });

  it("hints there is nothing to push when clean without an upstream and not ahead", () => {
    expect(resolveQuickAction(status({ hasUpstream: false, aheadCount: 0 }), false)).toMatchObject({
      kind: "show_hint",
      hint: "No local commits to push.",
    });
  });

  it("pushes (not commit_push) without an upstream when ahead with an open PR on a feature branch", () => {
    expect(
      resolveQuickAction(status({ hasUpstream: false, aheadCount: 2, pr: openPr }), false),
    ).toMatchObject({ label: "Push", kind: "run_action", action: "push" });
  });

  it("uses commit_push without an upstream when ahead on the default branch", () => {
    expect(
      resolveQuickAction(status({ hasUpstream: false, aheadCount: 2 }), false, true),
    ).toMatchObject({ label: "Push", kind: "run_action", action: "commit_push" });
  });

  it("pushes and creates a PR without an upstream when ahead on a feature branch", () => {
    expect(resolveQuickAction(status({ hasUpstream: false, aheadCount: 2 }), false)).toMatchObject({
      label: "Push & create PR",
      kind: "run_action",
      action: "create_pr",
    });
  });

  it("shows a diverged sync hint when both ahead and behind", () => {
    expect(resolveQuickAction(status({ aheadCount: 1, behindCount: 1 }), false)).toEqual({
      label: "Sync branch",
      disabled: true,
      kind: "show_hint",
      hint: "Branch has diverged from upstream. Rebase/merge first.",
    });
  });

  it("pulls when purely behind upstream", () => {
    expect(resolveQuickAction(status({ behindCount: 2 }), false)).toEqual({
      label: "Pull",
      disabled: false,
      kind: "run_pull",
    });
  });

  it("pushes when ahead with an open PR (feature branch)", () => {
    expect(resolveQuickAction(status({ aheadCount: 2, pr: openPr }), false)).toMatchObject({
      label: "Push",
      kind: "run_action",
      action: "push",
    });
  });

  it("uses commit_push when ahead on the default branch", () => {
    expect(resolveQuickAction(status({ aheadCount: 2 }), false, true)).toMatchObject({
      label: "Push",
      kind: "run_action",
      action: "commit_push",
    });
  });

  it("pushes and creates a PR when ahead on a feature branch without a PR", () => {
    expect(resolveQuickAction(status({ aheadCount: 2 }), false)).toMatchObject({
      label: "Push & create PR",
      kind: "run_action",
      action: "create_pr",
    });
  });

  it("views the PR when clean, up to date, with an upstream and an open PR", () => {
    expect(resolveQuickAction(status({ pr: openPr }), false)).toEqual({
      label: "View PR",
      disabled: false,
      kind: "open_pr",
    });
  });

  it("hints the branch is up to date when clean with nothing to do", () => {
    expect(resolveQuickAction(status(), false)).toEqual({
      label: "Commit",
      disabled: true,
      kind: "show_hint",
      hint: "Branch is up to date. No action needed.",
    });
  });
});

describe("getGitActionDisabledReason", () => {
  it("returns null for an enabled item", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ disabled: false }),
        gitStatus: status(),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBeNull();
  });

  it("reports the in-progress reason while busy", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem(),
        gitStatus: status(),
        isBusy: true,
        hasOriginRemote: true,
      }),
    ).toBe("Git action in progress.");
  });

  it("reports the unavailable reason without git status", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem(),
        gitStatus: null,
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("Git status is unavailable.");
  });

  it("explains a clean worktree for the commit item", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "commit" }),
        gitStatus: status({ hasWorkingTreeChanges: false }),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("Worktree is clean. Make changes before committing.");
  });

  it("falls back to a generic commit reason when changes exist", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "commit" }),
        gitStatus: status({ hasWorkingTreeChanges: true }),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("Commit is currently unavailable.");
  });

  it("explains a detached HEAD for the push item", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "push" }),
        gitStatus: status({ refName: null }),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("Detached HEAD: checkout a branch before pushing.");
  });

  it("asks to commit or stash before pushing dirty changes", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "push" }),
        gitStatus: status({ hasWorkingTreeChanges: true }),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("Commit or stash local changes before pushing.");
  });

  it("asks to pull before pushing when behind", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "push" }),
        gitStatus: status({ behindCount: 1 }),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("Branch is behind upstream. Pull/rebase before pushing.");
  });

  it("asks to add an origin remote before pushing", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "push" }),
        gitStatus: status({ hasUpstream: false, aheadCount: 1 }),
        isBusy: false,
        hasOriginRemote: false,
      }),
    ).toBe('Add an "origin" remote before pushing.');
  });

  it("reports no local commits to push", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "push" }),
        gitStatus: status({ aheadCount: 0 }),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("No local commits to push.");
  });

  it("falls back to a generic push reason", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "push" }),
        gitStatus: status({ aheadCount: 1 }),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("Push is currently unavailable.");
  });

  it("reports an unavailable open PR for the pr item", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "pr" }),
        gitStatus: status({ pr: openPr }),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("View PR is currently unavailable.");
  });

  it("explains a detached HEAD for the pr item", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "pr" }),
        gitStatus: status({ refName: null }),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("Detached HEAD: checkout a branch before creating a PR.");
  });

  it("asks to commit before creating a PR when dirty", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "pr" }),
        gitStatus: status({ hasWorkingTreeChanges: true }),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("Commit local changes before creating a PR.");
  });

  it("asks to add an origin remote before creating a PR", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "pr" }),
        gitStatus: status({ hasUpstream: false, aheadCount: 1 }),
        isBusy: false,
        hasOriginRemote: false,
      }),
    ).toBe('Add an "origin" remote before creating a PR.');
  });

  it("reports no local commits for a PR", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "pr" }),
        gitStatus: status({ aheadCount: 0 }),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("No local commits to include in a PR.");
  });

  it("asks to pull before creating a PR when behind", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "pr" }),
        gitStatus: status({ aheadCount: 1, behindCount: 1 }),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("Branch is behind upstream. Pull/rebase before creating a PR.");
  });

  it("falls back to a generic create-PR reason", () => {
    expect(
      getGitActionDisabledReason({
        item: menuItem({ id: "pr" }),
        gitStatus: status({ aheadCount: 1 }),
        isBusy: false,
        hasOriginRemote: true,
      }),
    ).toBe("Create PR is currently unavailable.");
  });
});

describe("requiresDefaultBranchConfirmation", () => {
  it("never requires confirmation off the default branch", () => {
    expect(requiresDefaultBranchConfirmation("push", false)).toBe(false);
  });

  it("does not require confirmation for a plain commit on the default branch", () => {
    expect(requiresDefaultBranchConfirmation("commit", true)).toBe(false);
  });

  it("requires confirmation for remote-affecting actions on the default branch", () => {
    const actions: GitStackedAction[] = ["push", "create_pr", "commit_push", "commit_push_pr"];
    for (const action of actions) {
      expect(requiresDefaultBranchConfirmation(action, true)).toBe(true);
    }
  });
});

describe("resolveDefaultBranchActionDialogCopy", () => {
  it("uses commit-and-push copy when a commit is included", () => {
    expect(
      resolveDefaultBranchActionDialogCopy({
        action: "commit_push",
        branchName: "main",
        includesCommit: true,
      }),
    ).toEqual({
      title: "Commit & push to default branch?",
      description:
        'This action will commit and push changes on "main". You can continue on this branch or create a feature branch and run the same action there.',
      continueLabel: "Commit & push to main",
    });
  });

  it("uses push-only copy when no commit is included", () => {
    expect(
      resolveDefaultBranchActionDialogCopy({
        action: "push",
        branchName: "main",
        includesCommit: false,
      }),
    ).toEqual({
      title: "Push to default branch?",
      description:
        'This action will push local commits on "main". You can continue on this branch or create a feature branch and run the same action there.',
      continueLabel: "Push to main",
    });
  });

  it("uses commit-push-PR copy when the PR action includes a commit", () => {
    expect(
      resolveDefaultBranchActionDialogCopy({
        action: "commit_push_pr",
        branchName: "main",
        includesCommit: true,
      }),
    ).toEqual({
      title: "Commit, push & create PR from default branch?",
      description:
        'This action will commit, push, and create a PR on "main". You can continue on this branch or create a feature branch and run the same action there.',
      continueLabel: "Commit, push & create PR",
    });
  });

  it("uses push-and-PR copy when the PR action has no commit", () => {
    expect(
      resolveDefaultBranchActionDialogCopy({
        action: "create_pr",
        branchName: "main",
        includesCommit: false,
      }),
    ).toEqual({
      title: "Push & create PR from default branch?",
      description:
        'This action will push local commits and create a PR on "main". You can continue on this branch or create a feature branch and run the same action there.',
      continueLabel: "Push & create PR",
    });
  });
});

function runResult(overrides: Partial<GitRunStackedActionResult> = {}): GitRunStackedActionResult {
  return {
    action: "commit_push_pr",
    branch: { status: "skipped_not_requested" },
    commit: { status: "created", commitSha: "abc123", subject: "Test commit" },
    push: { status: "pushed", branch: "feature/test" },
    pr: { status: "skipped_not_requested" },
    toast: { title: "Done", cta: { kind: "none" } },
    ...overrides,
  };
}

describe("resolveThreadBranchUpdate", () => {
  it("returns the created branch name", () => {
    expect(
      resolveThreadBranchUpdate(runResult({ branch: { status: "created", name: "feature/new" } })),
    ).toEqual({ branch: "feature/new" });
  });

  it("returns null when the branch step was skipped", () => {
    expect(resolveThreadBranchUpdate(runResult())).toBeNull();
  });

  it("returns null when the created branch has no name", () => {
    expect(resolveThreadBranchUpdate(runResult({ branch: { status: "created" } }))).toBeNull();
  });
});

describe("resolveLiveThreadBranchUpdate", () => {
  it("returns null when git status is unavailable", () => {
    expect(
      resolveLiveThreadBranchUpdate({ threadBranch: "feature/old", gitStatus: null }),
    ).toBeNull();
  });

  it("returns null when git status is detached but the thread already has a branch", () => {
    expect(
      resolveLiveThreadBranchUpdate({
        threadBranch: "feature/old",
        gitStatus: status({ refName: null }),
      }),
    ).toBeNull();
  });

  it("returns null when the stored branch already matches the live ref", () => {
    expect(
      resolveLiveThreadBranchUpdate({
        threadBranch: "feature/test",
        gitStatus: status({ refName: "feature/test" }),
      }),
    ).toBeNull();
  });

  it("does not regress a semantic branch back to a temporary worktree ref", () => {
    expect(
      resolveLiveThreadBranchUpdate({
        threadBranch: "feature/keep",
        gitStatus: status({ refName: "coddy-studio/bda76797" }),
      }),
    ).toBeNull();
  });

  it("reconciles a temporary worktree ref to a semantic branch", () => {
    expect(
      resolveLiveThreadBranchUpdate({
        threadBranch: "coddy-studio/a9628676",
        gitStatus: status({ refName: "feature/real" }),
      }),
    ).toEqual({ branch: "feature/real" });
  });

  it("adopts the live ref when the thread has no branch yet", () => {
    expect(
      resolveLiveThreadBranchUpdate({
        threadBranch: null,
        gitStatus: status({ refName: "feature/adopted" }),
      }),
    ).toEqual({ branch: "feature/adopted" });
  });
});
