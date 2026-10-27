import type { VcsStatusResult } from "@coddy/contracts";
import { assert, describe, it } from "vite-plus/test";
import {
  buildSourceControlMenuItems,
  resolveSourceControlPrimaryAction,
  type SourceControlMenuItem,
  type SourceControlMenuItemId,
  type SourceControlPrimaryActionInput,
} from "./SourceControlPrimaryAction.logic";

const OPEN_PR: NonNullable<VcsStatusResult["pr"]> = {
  number: 42,
  title: "Open PR",
  url: "https://example.com/pr/42",
  baseRef: "main",
  headRef: "feature/test",
  state: "open",
};

function status(overrides: Partial<VcsStatusResult> = {}): VcsStatusResult {
  return {
    isRepo: true,
    hasPrimaryRemote: true,
    isDefaultRef: false,
    refName: "feature/test",
    hasWorkingTreeChanges: false,
    workingTree: { files: [], insertions: 0, deletions: 0 },
    hasUpstream: true,
    aheadCount: 0,
    behindCount: 0,
    pr: null,
    ...overrides,
  };
}

function makeInput(
  args: {
    status?: Partial<VcsStatusResult> | null;
    isBusy?: boolean;
    isDefaultRef?: boolean;
    hasPrimaryRemote?: boolean;
    stagedCount?: number;
    stageableCount?: number;
    mergeAvailable?: boolean;
    canOpenInPanel?: boolean;
  } = {},
): SourceControlPrimaryActionInput {
  return {
    gitStatus: args.status === null ? null : status(args.status ?? {}),
    isBusy: args.isBusy ?? false,
    isDefaultRef: args.isDefaultRef ?? false,
    hasPrimaryRemote: args.hasPrimaryRemote ?? true,
    stagedCount: args.stagedCount ?? 0,
    stageableCount: args.stageableCount ?? 0,
    mergeAvailable: args.mergeAvailable ?? true,
    canOpenInPanel: args.canOpenInPanel ?? true,
  };
}

function menuIds(items: SourceControlMenuItem[]): SourceControlMenuItemId[] {
  return items.map((item) => item.id);
}

function menuItem(
  items: SourceControlMenuItem[],
  id: SourceControlMenuItemId,
): SourceControlMenuItem {
  const found = items.find((item) => item.id === id);
  assert.isDefined(found, `expected menu item "${id}"`);
  return found;
}

describe("resolveSourceControlPrimaryAction — commit-first ladder", () => {
  it("commits the staged index as-is when there are staged changes (empty message OK)", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({ stagedCount: 3, stageableCount: 2 }),
    );
    assert.deepEqual(action, {
      kind: "commit",
      label: "Commit",
      disabled: false,
      count: 3,
      stackedAction: "commit",
      commitStagedIndexAsIs: true,
    });
  });

  it("stages everything when nothing is staged but unstaged changes exist", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({ stagedCount: 0, stageableCount: 4 }),
    );
    assert.deepEqual(action, {
      kind: "stage_all",
      label: "Stage All Changes",
      disabled: false,
      count: 4,
    });
  });

  it("stages everything when only untracked files exist", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({ stagedCount: 0, stageableCount: 1 }),
    );
    assert.equal(action.kind, "stage_all");
    assert.equal(action.count, 1);
  });

  it("LEDGER GUARD: unstaged-only never resolves to commit", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({ stagedCount: 0, stageableCount: 5 }),
    );
    assert.notEqual(action.kind, "commit");
    assert.equal(action.kind, "stage_all");
  });

  it("pulls when the clean tree is behind upstream", () => {
    const action = resolveSourceControlPrimaryAction(makeInput({ status: { behindCount: 2 } }));
    assert.deepEqual(action, { kind: "pull", label: "Pull", disabled: false, count: 2 });
  });

  it("pushes & creates a PR when clean and ahead on a feature ref", () => {
    const action = resolveSourceControlPrimaryAction(makeInput({ status: { aheadCount: 3 } }));
    assert.deepEqual(action, {
      kind: "create_pr",
      label: "Push & create PR",
      disabled: false,
      stackedAction: "create_pr",
    });
  });

  it("pushes (not create PR) when clean, ahead, and a PR is already open", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({ status: { aheadCount: 3, pr: OPEN_PR } }),
    );
    assert.deepEqual(action, {
      kind: "push",
      label: "Push",
      disabled: false,
      count: 3,
      stackedAction: "push",
    });
  });

  it("pushes (not create PR) when clean, ahead, and on the default ref", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({ status: { aheadCount: 2 }, isDefaultRef: true }),
    );
    assert.deepEqual(action, {
      kind: "push",
      label: "Push",
      disabled: false,
      count: 2,
      stackedAction: "push",
    });
  });

  it("shows a disabled sync hint when clean and diverged", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({ status: { aheadCount: 2, behindCount: 1 } }),
    );
    assert.deepEqual(action, {
      kind: "sync_hint",
      label: "Sync ref",
      disabled: true,
      hint: "Branch has diverged from upstream. Rebase/merge first.",
    });
  });

  it("pushes & creates a PR when there is no upstream but a primary remote and commits are ahead", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({ status: { hasUpstream: false, aheadCount: 2 } }),
    );
    assert.deepEqual(action, {
      kind: "create_pr",
      label: "Push & create PR",
      disabled: false,
      stackedAction: "create_pr",
    });
  });

  it("publishes a branch when there is no upstream even when ahead count is unavailable", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({ status: { hasUpstream: false, aheadCount: 0 } }),
    );
    assert.deepEqual(action, {
      kind: "push",
      label: "Push",
      disabled: false,
      count: 0,
      stackedAction: "push",
    });
  });

  it("enables Push in the menu when a primary remote exists but no upstream does", () => {
    const items = buildSourceControlMenuItems(
      makeInput({ status: { hasUpstream: false, aheadCount: 0 } }),
    );
    assert.deepInclude(menuItem(items, "push"), {
      disabled: false,
      stackedAction: "push",
    });
  });

  it("publishes the repository when there is no primary remote", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({ status: { hasUpstream: false, aheadCount: 2 }, hasPrimaryRemote: false }),
    );
    assert.deepEqual(action, {
      kind: "publish",
      label: "Publish repository",
      disabled: false,
    });
  });

  it("views an open PR when there is no primary remote and nothing is ahead", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({
        status: { hasUpstream: false, aheadCount: 0, pr: OPEN_PR },
        hasPrimaryRemote: false,
      }),
    );
    assert.deepEqual(action, { kind: "open_pr", label: "View PR", disabled: false });
  });

  it("views an open PR when clean, synced, and a PR is open", () => {
    const action = resolveSourceControlPrimaryAction(makeInput({ status: { pr: OPEN_PR } }));
    assert.deepEqual(action, { kind: "open_pr", label: "View PR", disabled: false });
  });

  it("creates a PR when synced with upstream but ahead of the default ref", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({ status: { aheadCount: 0, behindCount: 0, aheadOfDefaultCount: 1 } }),
    );
    assert.deepEqual(action, {
      kind: "create_pr",
      label: "Create PR",
      disabled: false,
      stackedAction: "create_pr",
    });
  });

  it("is a disabled no-op when clean and fully up to date", () => {
    const action = resolveSourceControlPrimaryAction(makeInput());
    assert.deepEqual(action, {
      kind: "none",
      label: "Up to date",
      disabled: true,
      hint: "Branch is up to date. No action needed.",
    });
  });

  it("keeps the resolved action but disables it while busy (previous-kind disabled)", () => {
    const action = resolveSourceControlPrimaryAction(makeInput({ stagedCount: 3, isBusy: true }));
    assert.deepInclude(action, { kind: "commit", label: "Commit", disabled: true });
    assert.equal(action.commitStagedIndexAsIs, true);
  });

  it("reports a disabled no-op when git status is unavailable", () => {
    const action = resolveSourceControlPrimaryAction(makeInput({ status: null }));
    assert.deepEqual(action, {
      kind: "none",
      label: "Commit",
      disabled: true,
      hint: "Git status is unavailable.",
    });
  });

  it("still offers commit on a detached HEAD when there are staged changes", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({ status: { refName: null }, stagedCount: 2 }),
    );
    assert.deepInclude(action, {
      kind: "commit",
      disabled: false,
      commitStagedIndexAsIs: true,
    });
  });

  it("blocks remote actions on a detached HEAD with a clean tree", () => {
    const action = resolveSourceControlPrimaryAction(
      makeInput({ status: { refName: null, hasUpstream: false } }),
    );
    assert.deepEqual(action, {
      kind: "none",
      label: "Commit",
      disabled: true,
      hint: "Create and checkout a ref before pushing or opening a pull request.",
    });
  });
});

describe("buildSourceControlMenuItems — always-rendered, disabled-with-reason", () => {
  it("returns no items when git status is unavailable", () => {
    assert.deepEqual(buildSourceControlMenuItems(makeInput({ status: null })), []);
  });

  it("renders the full fixed order (no publish) when a primary remote exists", () => {
    const items = buildSourceControlMenuItems(makeInput());
    assert.deepEqual(menuIds(items), [
      "commit",
      "commit_push",
      "commit_push_pr",
      "push",
      "pull",
      "merge",
      "create_pr",
    ]);
    assert.deepEqual(
      items.map((item) => item.group),
      ["commit", "commit", "commit", "remote", "remote", "remote", "remote"],
    );
  });

  it("disables every commit-family item with a reason on a clean tree (ledger guard)", () => {
    const items = buildSourceControlMenuItems(makeInput());
    for (const id of ["commit", "commit_push", "commit_push_pr"] as const) {
      const item = menuItem(items, id);
      assert.isTrue(item.disabled, `${id} should be disabled`);
      assert.equal(item.reason, "No staged changes to commit.");
      assert.equal(item.commitStagedIndexAsIs, true);
    }
  });

  it("gates the remote items on a clean, up-to-date feature ref", () => {
    const items = buildSourceControlMenuItems(makeInput());
    assert.deepInclude(menuItem(items, "push"), {
      disabled: true,
      reason: "No local commits to push.",
    });
    assert.deepInclude(menuItem(items, "pull"), {
      disabled: true,
      reason: "Already up to date.",
    });
    assert.deepInclude(menuItem(items, "create_pr"), {
      disabled: true,
      reason: "No commits to open a pull request for.",
    });
  });

  it("enables the commit family and gates the remote family on a staged dirty tree", () => {
    const items = buildSourceControlMenuItems(makeInput({ stagedCount: 3 }));
    for (const id of ["commit", "commit_push", "commit_push_pr"] as const) {
      assert.isFalse(menuItem(items, id).disabled, `${id} should be enabled`);
    }
    assert.deepInclude(menuItem(items, "commit"), {
      kind: "run_stacked",
      stackedAction: "commit",
      commitStagedIndexAsIs: true,
    });
    assert.deepInclude(menuItem(items, "commit_push"), { stackedAction: "commit_push" });
    assert.deepInclude(menuItem(items, "commit_push_pr"), { stackedAction: "commit_push_pr" });
    assert.isTrue(menuItem(items, "push").disabled);
    assert.isTrue(menuItem(items, "create_pr").disabled);
  });

  it("does NOT gate create_pr on the default ref (allowed-but-confirmed by the caller)", () => {
    const items = buildSourceControlMenuItems(
      makeInput({ status: { aheadOfDefaultCount: 1 }, isDefaultRef: true }),
    );
    assert.deepInclude(menuItem(items, "create_pr"), {
      disabled: false,
      stackedAction: "create_pr",
    });
  });

  it("adds an enabled Publish item and blocks push/pull when there is no primary remote", () => {
    const items = buildSourceControlMenuItems(
      makeInput({ status: { hasUpstream: false }, hasPrimaryRemote: false, stagedCount: 3 }),
    );
    assert.deepEqual(menuIds(items), [
      "commit",
      "commit_push",
      "commit_push_pr",
      "push",
      "pull",
      "merge",
      "create_pr",
      "publish",
    ]);
    assert.isFalse(menuItem(items, "commit").disabled);
    assert.deepInclude(menuItem(items, "commit_push"), {
      disabled: true,
      reason: "No remote configured to push to.",
    });
    assert.deepInclude(menuItem(items, "push"), {
      disabled: true,
      reason: "No remote configured to push to.",
    });
    assert.deepInclude(menuItem(items, "pull"), {
      disabled: true,
      reason: "No upstream to pull from.",
    });
    assert.deepInclude(menuItem(items, "create_pr"), {
      disabled: true,
      reason: "No remote configured to push to.",
    });
    assert.deepInclude(menuItem(items, "publish"), { disabled: false, kind: "open_publish" });
  });

  it("swaps the PR slot to a View action when a PR is open", () => {
    const items = buildSourceControlMenuItems(
      makeInput({ status: { aheadCount: 2, pr: OPEN_PR } }),
    );
    assert.deepEqual(menuIds(items), [
      "commit",
      "commit_push",
      "commit_push_pr",
      "push",
      "pull",
      "merge",
      "open_pr",
      "open_pr_panel",
    ]);
    assert.deepInclude(menuItem(items, "open_pr"), {
      label: "View PR",
      disabled: false,
      kind: "open_pr",
    });
    assert.deepInclude(menuItem(items, "open_pr_panel"), {
      label: "Open PR in panel",
      disabled: false,
      kind: "open_pr_panel",
      group: "remote",
    });
  });

  it("omits the in-panel action where the panel cannot show the request", () => {
    const items = buildSourceControlMenuItems(
      makeInput({ status: { aheadCount: 2, pr: OPEN_PR }, canOpenInPanel: false }),
    );
    assert.notInclude(menuIds(items), "open_pr_panel");
    assert.include(menuIds(items), "open_pr");
  });

  it("offers no in-panel action without an open request", () => {
    const items = buildSourceControlMenuItems(makeInput({ status: { aheadCount: 2 } }));
    assert.notInclude(menuIds(items), "open_pr_panel");
  });

  it("disables the in-panel action while an action is in flight", () => {
    const items = buildSourceControlMenuItems(
      makeInput({ status: { aheadCount: 2, pr: OPEN_PR }, isBusy: true }),
    );
    assert.deepInclude(menuItem(items, "open_pr_panel"), {
      disabled: true,
      reason: "Git action in progress.",
    });
  });

  it("disables every item with the busy reason while an action is in flight", () => {
    const items = buildSourceControlMenuItems(makeInput({ isBusy: true, stagedCount: 3 }));
    for (const item of items) {
      assert.isTrue(item.disabled, `${item.id} should be disabled while busy`);
      assert.equal(item.reason, "Git action in progress.");
    }
  });

  it("keeps Pull available in the menu when diverged even though the primary is a sync hint", () => {
    const input = makeInput({ status: { aheadCount: 2, behindCount: 1 } });
    assert.equal(resolveSourceControlPrimaryAction(input).kind, "sync_hint");
    const items = buildSourceControlMenuItems(input);
    assert.isFalse(menuItem(items, "pull").disabled);
    assert.deepInclude(menuItem(items, "push"), {
      disabled: true,
      reason: "Branch has diverged from upstream. Rebase/merge first.",
    });
  });

  it("uses provider terminology (GitLab merge requests) in labels", () => {
    const items = buildSourceControlMenuItems(
      makeInput({
        status: {
          aheadCount: 2,
          sourceControlProvider: { kind: "gitlab", name: "GitLab", baseUrl: "https://gitlab.com" },
        },
        stagedCount: 3,
      }),
    );
    assert.equal(menuItem(items, "commit_push_pr").label, "Commit, Push & MR");
    assert.equal(menuItem(items, "create_pr").label, "Create MR");
  });

  it("does not offer change-request actions for a generic git remote", () => {
    const genericStatus = {
      aheadCount: 2,
      sourceControlProvider: {
        kind: "unknown" as const,
        name: "git.example.test",
        baseUrl: "https://git.example.test",
      },
    };
    assert.deepInclude(resolveSourceControlPrimaryAction(makeInput({ status: genericStatus })), {
      kind: "push",
      label: "Push",
    });
    const input = makeInput({
      status: {
        ...genericStatus,
      },
      stagedCount: 3,
    });
    assert.deepEqual(menuIds(buildSourceControlMenuItems(input)), [
      "commit",
      "commit_push",
      "push",
      "pull",
      "merge",
    ]);
  });

  it("hides push-family menu actions on generated worktree refs", () => {
    const input = makeInput({
      status: { refName: "coddy-studio/deadbeef", aheadCount: 2 },
      stagedCount: 3,
    });
    assert.deepEqual(menuIds(buildSourceControlMenuItems(input)), [
      "commit",
      "pull",
      "merge",
      "create_pr",
    ]);
    assert.equal(
      resolveSourceControlPrimaryAction(
        makeInput({ status: { refName: "coddy-studio/deadbeef", aheadCount: 2 } }),
      ).kind,
      "none",
    );
  });
});

describe("buildSourceControlMenuItems — Merge into current branch", () => {
  it.each([
    [{ refName: null }, "Check out a branch to merge into."],
    [{ operationInProgress: "merge" as const }, "Finish or abort the current merge first."],
    [{ hasWorkingTreeChanges: true }, "Commit your changes before merging."],
  ])("gates Merge into current branch (%o)", (overrides, reason) => {
    const items = buildSourceControlMenuItems(makeInput({ status: overrides }));
    assert.deepInclude(menuItem(items, "merge"), {
      label: "Merge into current branch…",
      disabled: true,
      reason,
      kind: "open_merge",
      group: "remote",
    });
  });

  it("enables Merge into current branch on a clean checked-out branch", () => {
    const items = buildSourceControlMenuItems(makeInput());
    assert.deepInclude(menuItem(items, "merge"), { disabled: false, kind: "open_merge" });
  });

  it("explains a missing merge capability first", () => {
    const items = buildSourceControlMenuItems(
      makeInput({ status: { refName: null }, mergeAvailable: false }),
    );
    assert.equal(
      menuItem(items, "merge").reason,
      "This environment does not support Git Manager stash and merge operations.",
    );
  });
});
