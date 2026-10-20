import { Schema } from "effect";
import { describe, expect, it } from "vite-plus/test";

import {
  GitManagerBlockedReason,
  GitManagerCommitEntry,
  GitManagerCommitPage,
  GitManagerConflictState,
  GitManagerDiff,
  GitManagerDiffSource,
  GitManagerMergePreview,
  GitManagerOperationError,
  GitManagerOperationEvent,
  GitManagerOperationRequest,
  GitManagerPreviewMergeInput,
  GitManagerRefEntry,
  GitManagerRefsSnapshot,
  GitManagerSignalEvent,
  GitManagerStashEntry,
  GitManagerWorktreeEntry,
} from "./gitManager.ts";

const decodeGitManagerBlockedReason = Schema.decodeUnknownSync(GitManagerBlockedReason);
const decodeGitManagerPreviewMergeInput = Schema.decodeUnknownSync(GitManagerPreviewMergeInput);
const decodeGitManagerWorktreeEntry = Schema.decodeUnknownSync(GitManagerWorktreeEntry);
const decodeGitManagerRefEntry = Schema.decodeUnknownSync(GitManagerRefEntry);
const decodeGitManagerRefsSnapshot = Schema.decodeUnknownSync(GitManagerRefsSnapshot);
const decodeGitManagerCommitEntry = Schema.decodeUnknownSync(GitManagerCommitEntry);
const decodeGitManagerCommitPage = Schema.decodeUnknownSync(GitManagerCommitPage);
const decodeGitManagerDiffSource = Schema.decodeUnknownSync(GitManagerDiffSource);
const decodeGitManagerDiff = Schema.decodeUnknownSync(GitManagerDiff);
const decodeGitManagerStashEntry = Schema.decodeUnknownSync(GitManagerStashEntry);
const decodeGitManagerConflictState = Schema.decodeUnknownSync(GitManagerConflictState);
const decodeGitManagerMergePreview = Schema.decodeUnknownSync(GitManagerMergePreview);
const decodeGitManagerOperationRequest = Schema.decodeUnknownSync(GitManagerOperationRequest);
const decodeGitManagerOperationEvent = Schema.decodeUnknownSync(GitManagerOperationEvent);
const decodeGitManagerSignalEvent = Schema.decodeUnknownSync(GitManagerSignalEvent);
const decodeGitManagerOperationError = Schema.decodeUnknownSync(GitManagerOperationError);

describe("GitManagerBlockedReason", () => {
  it("round-trips a server-authored blocked reason verbatim", () => {
    const decoded = decodeGitManagerBlockedReason({
      operation: "checkout",
      code: "worktree-checked-out",
      message: "Checkout is blocked: this branch is already checked out in another worktree.",
    });
    expect(decoded.message).toBe(
      "Checkout is blocked: this branch is already checked out in another worktree.",
    );
  });
});

describe("Git Manager wire schemas", () => {
  it("decodes a registered worktree including a missing directory", () => {
    const decoded = decodeGitManagerWorktreeEntry({
      path: "/repo-worktrees/topic",
      headSha: "abc123",
      branch: "topic",
      isPrimary: false,
      isBare: false,
      isDetached: false,
      locked: false,
      lockReason: null,
      prunable: true,
    });
    expect(decoded.prunable).toBe(true);
  });

  it("keeps server-authored blocked reasons on refs", () => {
    const decoded = decodeGitManagerRefEntry({
      name: "topic",
      tipSha: "abc123",
      upstream: "origin/topic",
      ahead: 2,
      behind: 1,
      current: false,
      isDefault: false,
      worktreePath: "/repo-worktrees/topic",
      blocked: [
        {
          operation: "delete",
          code: "worktree-checked-out",
          message: "Delete is blocked while the branch is checked out elsewhere.",
        },
      ],
    });
    expect(decoded.blocked[0]?.message).toBe(
      "Delete is blocked while the branch is checked out elsewhere.",
    );
  });

  it("decodes the complete repository refs snapshot", () => {
    const decoded = decodeGitManagerRefsSnapshot({
      generation: 7,
      headRef: "main",
      detachedSha: null,
      isDirty: true,
      defaultBranch: "main",
      remotes: ["origin"],
      localBranches: [],
      remoteBranches: [],
      tags: [],
      worktrees: [],
      inProgressOperation: null,
      conflictedPaths: ["src/conflicted.ts"],
    });
    expect(decoded.conflictedPaths).toEqual(["src/conflicted.ts"]);
  });

  it("preserves commit author and committer identities independently", () => {
    const decoded = decodeGitManagerCommitEntry({
      sha: "abcdef123456",
      shortSha: "abcdef1",
      parents: ["parent1", "parent2"],
      decorations: ["HEAD -> main", "origin/main"],
      subject: "Subject",
      body: "Body",
      authorName: "Ann Author",
      authorEmail: "ann@example.test",
      authoredAtMs: 1_735_689_600_000,
      committerName: "Cara Committer",
      committerEmail: "cara@example.test",
      committedAtMs: 1_735_689_660_000,
      changedFiles: ["src/file.ts"],
    });
    expect(decoded.authorEmail).toBe("ann@example.test");
    expect(decoded.committerEmail).toBe("cara@example.test");
  });

  it("keeps pinned tips and degraded paging explicit", () => {
    const decoded = decodeGitManagerCommitPage({
      generation: 8,
      pinnedTips: ["abcdef123456"],
      commits: [],
      nextOffset: 100,
      exhausted: false,
      degradedToAllPaging: true,
    });
    expect(decoded.pinnedTips).toEqual(["abcdef123456"]);
    expect(decoded.degradedToAllPaging).toBe(true);
  });

  it("uses a stash sha rather than a shifting index as a diff identity", () => {
    const decoded = decodeGitManagerDiffSource({
      _tag: "stash",
      sha: "stash-sha",
      path: "src/file.ts",
    });
    expect(decoded._tag).toBe("stash");
    if (decoded._tag === "stash") expect(decoded.sha).toBe("stash-sha");
  });

  it("decodes an explicit large-text diff marker without patch content", () => {
    const decoded = decodeGitManagerDiff({
      _tag: "large-text",
      generation: 9,
      source: { _tag: "commit", sha: "abcdef123456", path: "src/file.ts" },
      byteLength: 5_000_000,
      longestLineLength: 120,
    });
    expect(decoded._tag).toBe("large-text");
  });

  it("keeps a stash sha stable while its list index remains presentation data", () => {
    const decoded = decodeGitManagerStashEntry({
      index: 0,
      sha: "stash-sha",
      message: "WIP on main",
      committedAtMs: 1_735_689_600_000,
      parents: ["parent1", "parent2", "parent3"],
      files: [{ path: "src/file.ts", status: "modified", insertions: 2, deletions: 1 }],
    });
    expect(decoded.sha).toBe("stash-sha");
    expect(decoded.index).toBe(0);
  });

  it("decodes conflict kind, marker count, and nullable resolution", () => {
    const decoded = decodeGitManagerConflictState({
      path: "src/conflicted.ts",
      kind: "text",
      markerCount: 3,
      resolution: null,
    });
    expect(decoded.markerCount).toBe(3);
  });

  it("keeps merge conflict counts server-authored", () => {
    const decoded = decodeGitManagerMergePreview({
      _tag: "conflicted",
      source: "topic",
      current: "main",
      ahead: 2,
      behind: 0,
      fileCount: 3,
    });
    expect(decoded._tag).toBe("conflicted");
    if (decoded._tag === "conflicted") expect(decoded.fileCount).toBe(3);
  });

  it("decodes branch, sync, stash, merge, rewrite, conflict, and tag operations", () => {
    const decode = decodeGitManagerOperationRequest;
    const base = { cwd: "/repo", projectId: "project-1" };
    const operations = [
      { ...base, _tag: "branch-create", name: "topic", startPoint: null, checkout: true },
      { ...base, _tag: "branch-checkout", name: "topic", strategy: "bring" },
      { ...base, _tag: "fetch", remote: "origin" },
      { ...base, _tag: "stash-push", message: "WIP", paths: ["src/file.ts"] },
      { ...base, _tag: "merge", source: "topic", noVerify: false },
      { ...base, _tag: "merge-into", source: "refs/heads/topic", target: "main" },
      { ...base, _tag: "rebase", base: "main", target: "topic" },
      { ...base, _tag: "resolve-conflict", path: "src/file.ts", side: "ours" },
      { ...base, _tag: "tag-create", name: "v1.0.0", sha: "abcdef123456" },
    ];
    expect(operations.map((operation) => decode(operation)._tag)).toEqual([
      "branch-create",
      "branch-checkout",
      "fetch",
      "stash-push",
      "merge",
      "merge-into",
      "rebase",
      "resolve-conflict",
      "tag-create",
    ]);
  });

  it("decodes a stash push that also saves untracked files, and one from an older client", () => {
    const decode = decodeGitManagerOperationRequest;
    const base = { cwd: "/repo", projectId: "project-1", _tag: "stash-push", message: "WIP" };
    expect(decode({ ...base, paths: [], includeUntracked: true })).toEqual({
      ...base,
      paths: [],
      includeUntracked: true,
    });
    expect(decode({ ...base, paths: [] })).toEqual({ ...base, paths: [] });
    expect(() => decode({ ...base, paths: [], includeUntracked: "yes" })).toThrow();
  });

  it("decodes a merge preview input with and without a target", () => {
    const decode = decodeGitManagerPreviewMergeInput;
    expect(decode({ cwd: "/repo", source: "topic" })).toEqual({ cwd: "/repo", source: "topic" });
    expect(decode({ cwd: "/repo", source: "topic", target: "release" })).toEqual({
      cwd: "/repo",
      source: "topic",
      target: "release",
    });
  });

  it("decodes the four operation stream event kinds", () => {
    const decode = decodeGitManagerOperationEvent;
    expect(
      [
        { _tag: "started", operation: "fetch" },
        { _tag: "output", operation: "fetch", stream: "stdout", text: "done" },
        { _tag: "finished", operation: "fetch", message: "Fetch completed." },
        {
          _tag: "failed",
          operation: "fetch",
          code: "authentication",
          message: "Authentication failed.",
          blocked: null,
        },
      ].map((event) => decode(event)._tag),
    ).toEqual(["started", "output", "finished", "failed"]);
  });

  it("decodes the live repository generation signal", () => {
    expect(decodeGitManagerSignalEvent({ cwd: "/repo", generation: 10 })).toEqual({
      cwd: "/repo",
      generation: 10,
    });
  });

  it("preserves degraded watcher health without requiring it from older servers", () => {
    expect(
      decodeGitManagerSignalEvent({ cwd: "/repo", generation: 10, watcherDegraded: true }),
    ).toEqual({ cwd: "/repo", generation: 10, watcherDegraded: true });
    expect(
      decodeGitManagerSignalEvent({ cwd: "/repo", generation: 10 }).watcherDegraded,
    ).toBeUndefined();
  });

  it("preserves operation error messages and nullable blocked reasons", () => {
    const decoded = decodeGitManagerOperationError({
      _tag: "GitManagerOperationError",
      operation: "checkout",
      code: "not-implemented",
      message: "This Git Manager operation is not implemented yet.",
      blocked: null,
    });
    expect(decoded.message).toBe("This Git Manager operation is not implemented yet.");
    expect(decoded.blocked).toBeNull();
  });
});
