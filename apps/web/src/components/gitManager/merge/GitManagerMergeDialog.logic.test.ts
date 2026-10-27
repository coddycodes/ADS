import type { GitManagerMergePreview, GitManagerRefEntry } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  hasMergeSource,
  resolveMergeConfirmCopy,
  shortRefName,
  summarizeMergePreview,
} from "./GitManagerMergeDialog.logic";

function preview(
  value: Partial<GitManagerMergePreview> & Pick<GitManagerMergePreview, "_tag">,
): GitManagerMergePreview {
  return {
    source: "feature",
    current: "main",
    ahead: 7,
    behind: 3,
    ...value,
  } as GitManagerMergePreview;
}

describe("summarizeMergePreview", () => {
  it("uses the server's clean ahead and behind counts without recomputing them", () => {
    expect(summarizeMergePreview(preview({ _tag: "clean" }))).toEqual({
      kind: "clean",
      message: "This will merge 7 commits from `feature` into `main`.",
      mergeEnabled: true,
      ahead: 7,
      behind: 3,
    });
  });

  it("disables a clean preview whose source has nothing to merge", () => {
    expect(summarizeMergePreview(preview({ _tag: "clean", ahead: 0 }))).toEqual({
      kind: "clean",
      message: "Nothing to merge: `feature` has no commits that `main` lacks.",
      mergeEnabled: false,
      ahead: 0,
      behind: 3,
    });
  });

  it("presents the server's conflicted file count", () => {
    expect(summarizeMergePreview(preview({ _tag: "conflicted", fileCount: 4 }))).toEqual({
      kind: "conflicted",
      message: "There will be 4 conflicted files.",
      mergeEnabled: true,
      ahead: 7,
      behind: 3,
    });
  });

  it("names full-ref sources by their short branch name", () => {
    expect(
      summarizeMergePreview(preview({ _tag: "clean", source: "refs/heads/feature" })).message,
    ).toBe("This will merge 7 commits from `feature` into `main`.");
    expect(
      summarizeMergePreview(preview({ _tag: "clean", source: "refs/remotes/origin/topic" }))
        .message,
    ).toBe("This will merge 7 commits from `origin/topic` into `main`.");
  });

  it("disables a conflicted merge into another branch with the check-out hint", () => {
    const summary = summarizeMergePreview(
      preview({
        _tag: "conflicted",
        source: "refs/heads/feature",
        current: "release",
        ahead: 1,
        behind: 0,
        fileCount: 2,
      }),
      { intoOtherBranch: true },
    );
    expect(summary.mergeEnabled).toBe(false);
    expect(summary.message).toBe(
      "2 files would conflict. Check out `release` to merge and resolve them.",
    );
  });

  it("disables a server-classified unrelated-histories merge", () => {
    expect(summarizeMergePreview(preview({ _tag: "unrelated-histories" }))).toEqual({
      kind: "unrelated-histories",
      message: "These branches have unrelated histories and cannot be merged.",
      mergeEnabled: false,
      ahead: 7,
      behind: 3,
    });
  });
});

describe("resolveMergeConfirmCopy", () => {
  it("distinguishes merge commits from squash merges", () => {
    expect(resolveMergeConfirmCopy("merge")).toEqual({
      title: "Merge into current branch",
      confirmLabel: "Merge",
    });
    expect(resolveMergeConfirmCopy("squash")).toEqual({
      title: "Squash and merge into current branch",
      confirmLabel: "Squash and Merge",
    });
  });

  it("names the target branch when merging into another branch", () => {
    expect(resolveMergeConfirmCopy("merge", "release")).toEqual({
      title: "Merge into release",
      confirmLabel: "Merge",
    });
  });
});

describe("shortRefName", () => {
  it("strips local and remote-tracking ref prefixes only", () => {
    expect(shortRefName("refs/heads/feature")).toBe("feature");
    expect(shortRefName("refs/remotes/origin/x")).toBe("origin/x");
    expect(shortRefName("main")).toBe("main");
    expect(shortRefName("refs/tags/v1")).toBe("refs/tags/v1");
  });
});

describe("hasMergeSource", () => {
  function ref(name: string, current = false): GitManagerRefEntry {
    return {
      name,
      tipSha: `${name}-sha`,
      upstream: null,
      ahead: 0,
      behind: 0,
      current,
      isDefault: false,
      worktreePath: null,
      blocked: [],
    };
  }

  it("needs a local branch other than the current one, or any remote branch", () => {
    expect(hasMergeSource([ref("main", true)], [])).toBe(false);
    expect(hasMergeSource([ref("main", true)], [ref("origin/topic")])).toBe(true);
    expect(hasMergeSource([ref("main", true), ref("feature")], [])).toBe(true);
  });
});
