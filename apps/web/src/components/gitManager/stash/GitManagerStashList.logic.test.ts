import type { GitManagerBlockedReason, GitManagerStashEntry } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  buildStashRows,
  resolveStashActionState,
  resolveStashDiscardDialogCopy,
  resolveStashIndex,
} from "./GitManagerStashList.logic";

const operationBlocked: GitManagerBlockedReason = {
  operation: "stash-apply",
  code: "operation-in-flight",
  message: "A repository operation is already running.",
};

function stash(index: number, sha: string, message: string): GitManagerStashEntry {
  return {
    index,
    sha,
    message,
    committedAtMs: index + 1,
    parents: [`parent-${index}`],
    files: [
      {
        path: `worktree-${index}/file.ts`,
        status: "modified",
        insertions: index + 2,
        deletions: index,
      },
    ],
  };
}

describe("buildStashRows", () => {
  it("preserves the server's repository-wide LIFO order and blocked reason verbatim", () => {
    const entries = [
      stash(0, "newest-sha", "On feature: newest"),
      stash(1, "older-sha", "On main: older"),
    ];

    const rows = buildStashRows(entries, [operationBlocked]);

    expect(rows).toEqual([
      { ...entries[0], blocked: operationBlocked },
      { ...entries[1], blocked: operationBlocked },
    ]);
    expect(rows.map((row) => row.sha)).toEqual(["newest-sha", "older-sha"]);
    expect(rows.flatMap((row) => row.files.map((file) => file.path))).toEqual([
      "worktree-0/file.ts",
      "worktree-1/file.ts",
    ]);
    expect(rows[0]?.blocked).toBe(operationBlocked);
  });
});

describe("resolveStashIndex", () => {
  it("resolves the current selector index by sha and returns null once the sha is gone", () => {
    const entries = [stash(0, "newest-sha", "newest"), stash(4, "target-sha", "target")];

    expect(resolveStashIndex(entries, "target-sha")).toBe(4);
    expect(resolveStashIndex(entries, "dropped-sha")).toBeNull();
  });
});

describe("resolveStashActionState", () => {
  it("disables apply, pop and drop with the server's operation-in-flight message", () => {
    const [row] = buildStashRows([stash(0, "sha", "busy")], [operationBlocked]);
    if (row === undefined) throw new Error("Missing stash row");

    expect(resolveStashActionState(row, { operationInFlight: true })).toEqual({
      apply: { enabled: false, reason: operationBlocked.message },
      pop: { enabled: false, reason: operationBlocked.message },
      drop: { enabled: false, reason: operationBlocked.message },
    });
  });

  it("enables all actions when the server reports no block", () => {
    const [row] = buildStashRows([stash(0, "sha", "ready")], []);
    if (row === undefined) throw new Error("Missing stash row");

    expect(resolveStashActionState(row, { operationInFlight: false })).toEqual({
      apply: { enabled: true, reason: null },
      pop: { enabled: true, reason: null },
      drop: { enabled: true, reason: null },
    });
  });
});

describe("resolveStashDiscardDialogCopy", () => {
  it("names the current stash selector and makes irreversibility explicit", () => {
    const [row] = buildStashRows([stash(3, "sha", "checkpoint")], []);
    if (row === undefined) throw new Error("Missing stash row");

    expect(resolveStashDiscardDialogCopy(row)).toEqual({
      title: "Drop stash@{3}?",
      body: "Drop stash@{3} (checkpoint)? This entry cannot be recovered.",
      confirmLabel: "Drop Stash",
      destructive: true,
    });
  });
});
