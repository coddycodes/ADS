import type { GitManagerCommitEntry } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  buildCommitMenuItems,
  resolveCommitSelectionPresentation,
} from "./GitManagerCommitContextMenu.logic";

function commit(sha: string, parents: ReadonlyArray<string> = ["parent"]): GitManagerCommitEntry {
  return {
    sha,
    shortSha: sha.slice(0, 7),
    parents,
    decorations: [],
    subject: sha,
    body: "",
    authorName: "Author",
    authorEmail: "author@example.com",
    authoredAtMs: 1,
    committerName: "Committer",
    committerEmail: "committer@example.com",
    committedAtMs: 1,
    changedFiles: [],
  };
}

const loadedCommits = [commit("a"), commit("b"), commit("c"), commit("d")];
const availableCapabilities = {
  branchSyncDisabledReason: null,
  rewriteDisabledReason: null,
  tagDisabledReason: null,
} as const;

describe("buildCommitMenuItems", () => {
  it("builds the reference single- and multi-commit action sets", () => {
    const single = buildCommitMenuItems(["b"], {
      loadedCommits,
      blockedReasons: [],
      ...availableCapabilities,
    });
    const multiple = buildCommitMenuItems(["b", "c"], {
      loadedCommits,
      blockedReasons: [],
      ...availableCapabilities,
    });

    expect(single.map((item) => item.label)).toEqual([
      "Reset to Commit",
      "Revert",
      "Cherry-Pick",
      "Reorder",
      "Create Branch from Commit",
      "Create Tag",
      "Copy SHA",
    ]);
    expect(multiple.map((item) => item.label)).toEqual(["Cherry-Pick 2", "Squash 2", "Reorder 2"]);
  });

  it("requires contiguous non-merge commits for squash and reorder", () => {
    const nonContiguous = buildCommitMenuItems(["b", "d"], {
      loadedCommits,
      blockedReasons: [],
      ...availableCapabilities,
    });
    const withMerge = buildCommitMenuItems(["b", "c"], {
      loadedCommits: [commit("a"), commit("b", ["left", "right"]), commit("c"), commit("d")],
      blockedReasons: [],
      ...availableCapabilities,
    });

    for (const items of [nonContiguous, withMerge]) {
      expect(items.find((item) => item.id === "cherry-pick")?.enabled).toBe(true);
      expect(items.find((item) => item.id === "squash")?.enabled).toBe(false);
      expect(items.find((item) => item.id === "reorder")?.enabled).toBe(false);
    }
    expect(nonContiguous.find((item) => item.id === "squash")?.disabledReason).toBe(
      "Select a contiguous range of commits.",
    );
    expect(withMerge.find((item) => item.id === "reorder")?.disabledReason).toBe(
      "Merge commits cannot be squashed or reordered.",
    );
  });

  it("keeps server-blocked actions present with the verbatim server message", () => {
    const serverMessage = "Server says another repository operation owns the mutation lane.";
    const items = buildCommitMenuItems(["b", "c"], {
      loadedCommits,
      blockedReasons: [
        { operation: "squash", code: "operation-in-flight", message: serverMessage },
      ],
      ...availableCapabilities,
    });

    expect(items.find((item) => item.id === "squash")).toEqual({
      id: "squash",
      label: "Squash 2",
      enabled: false,
      disabledReason: serverMessage,
    });
  });

  it("suppresses an arbitrary diff for a non-contiguous selection", () => {
    expect(resolveCommitSelectionPresentation(["b", "d"], loadedCommits)).toEqual({
      kind: "non-contiguous",
      suppressDiff: true,
      message: "Select a contiguous range to compare multiple commits.",
    });
    expect(resolveCommitSelectionPresentation(["b", "c"], loadedCommits)).toEqual({
      kind: "range",
      suppressDiff: false,
      message: null,
    });
  });

  it("applies each capability reason only to its own commit-menu actions", () => {
    const rewriteReason = "Rewrite unavailable.";
    const branchReason = "Branch unavailable.";
    const tagReason = "Tag unavailable.";
    const items = buildCommitMenuItems(["b"], {
      loadedCommits,
      blockedReasons: [],
      rewriteDisabledReason: rewriteReason,
      branchSyncDisabledReason: branchReason,
      tagDisabledReason: tagReason,
    });

    for (const id of ["reset", "revert", "cherry-pick", "reorder"] as const) {
      expect(items.find((item) => item.id === id)).toMatchObject({
        enabled: false,
        disabledReason: rewriteReason,
      });
    }
    expect(items.find((item) => item.id === "create-branch")).toMatchObject({
      enabled: false,
      disabledReason: branchReason,
    });
    expect(items.find((item) => item.id === "create-tag")).toMatchObject({
      enabled: false,
      disabledReason: tagReason,
    });
    expect(items.find((item) => item.id === "copy-sha")).toMatchObject({
      enabled: true,
      disabledReason: null,
    });
  });
});
