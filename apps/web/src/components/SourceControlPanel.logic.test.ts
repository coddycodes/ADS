import type { VcsStatusResult } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  appendGitignorePattern,
  discardPathsOf,
  groupFilesByArea,
  ignoreFileNamePattern,
  ignoreParentFolderPattern,
  isFileStaged,
  type PendingDiscard,
  resolveDiscardDialogCopy,
  resolveStagingToggleAction,
  resolveVsBaseLabel,
  splitFilePath,
  summarizeChangeSelection,
  type WorkingTreeFile,
} from "./SourceControlPanel.logic";

const FILES: WorkingTreeFile[] = [
  { path: "docs/prps/PFS-1848/master-plan.md", insertions: 218, deletions: 0 },
  { path: "tasks.md", insertions: 139, deletions: 4 },
];

describe("splitFilePath", () => {
  it("splits a nested path into dir and name", () => {
    expect(splitFilePath("docs/prps/PFS-1848/master-plan.md")).toEqual({
      dir: "docs/prps/PFS-1848",
      name: "master-plan.md",
    });
  });

  it("returns a null dir for a top-level file", () => {
    expect(splitFilePath("tasks.md")).toEqual({ dir: null, name: "tasks.md" });
  });
});

describe("gitignore helpers", () => {
  it("derives a file-name pattern from nested and Windows-style paths", () => {
    expect(ignoreFileNamePattern("docs/generated/output.log")).toBe("output.log");
    expect(ignoreFileNamePattern("tmp\\cache.bin")).toBe("cache.bin");
  });

  it("derives a parent-folder pattern when the file is nested", () => {
    expect(ignoreParentFolderPattern("docs/generated/output.log")).toBe("docs/generated/");
    expect(ignoreParentFolderPattern("output.log")).toBeNull();
  });

  it("appends gitignore patterns without duplicating existing entries", () => {
    expect(appendGitignorePattern("", "dist/")).toBe("dist/\n");
    expect(appendGitignorePattern("node_modules/\n", "dist/")).toBe("node_modules/\ndist/\n");
    expect(appendGitignorePattern("dist/\n", "dist/")).toBe("dist/\n");
    expect(appendGitignorePattern("dist/", "cache/")).toBe("dist/\ncache/\n");
  });
});

describe("summarizeChangeSelection", () => {
  it("sums totals across all files", () => {
    const summary = summarizeChangeSelection(FILES);
    expect(summary.totalCount).toBe(2);
    expect(summary.insertions).toBe(357);
    expect(summary.deletions).toBe(4);
  });

  it("returns zeroed totals for an empty change set", () => {
    const summary = summarizeChangeSelection([]);
    expect(summary.totalCount).toBe(0);
    expect(summary.insertions).toBe(0);
    expect(summary.deletions).toBe(0);
  });
});

describe("groupFilesByArea", () => {
  it("buckets files by area, defaulting missing area to unstaged", () => {
    const groups = groupFilesByArea([
      { path: "a.ts", insertions: 1, deletions: 0, area: "staged" },
      { path: "b.ts", insertions: 2, deletions: 0, area: "unstaged" },
      { path: "c.txt", insertions: 0, deletions: 0, area: "untracked" },
      { path: "d.ts", insertions: 1, deletions: 1 },
    ]);
    expect(groups.staged.map((f) => f.path)).toEqual(["a.ts"]);
    expect(groups.unstaged.map((f) => f.path)).toEqual(["b.ts", "d.ts"]);
    expect(groups.untracked.map((f) => f.path)).toEqual(["c.txt"]);
  });

  it("groups conflicted files apart from ordinary changes when asked to", () => {
    const files = [
      { path: "a.txt", insertions: 0, deletions: 0, status: "conflicted", area: "unstaged" },
      { path: "b.txt", insertions: 1, deletions: 0, status: "modified", area: "unstaged" },
    ] as const;
    const separated = groupFilesByArea(files, { separateConflicts: true });
    expect(separated.conflicted.map((file) => file.path)).toEqual(["a.txt"]);
    expect(separated.unstaged.map((file) => file.path)).toEqual(["b.txt"]);

    const ordinary = groupFilesByArea(files);
    expect(ordinary.conflicted).toEqual([]);
    expect(ordinary.unstaged.map((file) => file.path)).toEqual(["a.txt", "b.txt"]);
  });
});

describe("isFileStaged", () => {
  it("is true only for files in the staged area", () => {
    expect(isFileStaged({ path: "a.ts", insertions: 0, deletions: 0, area: "staged" })).toBe(true);
    expect(isFileStaged({ path: "b.ts", insertions: 0, deletions: 0, area: "unstaged" })).toBe(
      false,
    );
    expect(isFileStaged({ path: "c.ts", insertions: 0, deletions: 0, area: "untracked" })).toBe(
      false,
    );
    expect(isFileStaged({ path: "d.ts", insertions: 0, deletions: 0 })).toBe(false);
  });
});

describe("resolveStagingToggleAction", () => {
  it("resolves a staged row to unstage", () => {
    expect(resolveStagingToggleAction("staged")).toBe("unstage");
  });

  it("resolves unstaged, untracked, and arealess rows to stage", () => {
    expect(resolveStagingToggleAction("unstaged")).toBe("stage");
    expect(resolveStagingToggleAction("untracked")).toBe("stage");
    expect(resolveStagingToggleAction(undefined)).toBe("stage");
  });
});

const BASE_STATUS: VcsStatusResult = {
  isRepo: true,
  hasPrimaryRemote: true,
  isDefaultRef: false,
  refName: "feature/demo",
  hasWorkingTreeChanges: false,
  workingTree: { files: [], insertions: 0, deletions: 0 },
  hasUpstream: true,
  aheadCount: 0,
  behindCount: 0,
  pr: null,
};

describe("resolveVsBaseLabel", () => {
  it("returns null when status is missing", () => {
    expect(resolveVsBaseLabel(null)).toBeNull();
    expect(resolveVsBaseLabel(undefined)).toBeNull();
  });

  it("returns null when refName is null", () => {
    expect(
      resolveVsBaseLabel({ ...BASE_STATUS, refName: null, defaultRefName: "main" }),
    ).toBeNull();
  });

  it("returns null when defaultRefName is absent", () => {
    expect(resolveVsBaseLabel(BASE_STATUS)).toBeNull();
  });

  it("returns null when the current ref is already the default ref", () => {
    expect(
      resolveVsBaseLabel({ ...BASE_STATUS, defaultRefName: "main", isDefaultRef: true }),
    ).toBeNull();
  });

  it('renders "vs <defaultRefName>" when on a non-default branch', () => {
    expect(resolveVsBaseLabel({ ...BASE_STATUS, defaultRefName: "main" })).toBe("vs main");
  });

  it("appends the ahead-of-default count when present and positive", () => {
    expect(
      resolveVsBaseLabel({ ...BASE_STATUS, defaultRefName: "main", aheadOfDefaultCount: 3 }),
    ).toBe("vs main ↑3");
  });

  it("omits the count when it is zero", () => {
    expect(
      resolveVsBaseLabel({ ...BASE_STATUS, defaultRefName: "main", aheadOfDefaultCount: 0 }),
    ).toBe("vs main");
  });
});

describe("resolveDiscardDialogCopy", () => {
  it("deletes an untracked entry (destructive, cannot be undone)", () => {
    const pending: PendingDiscard = {
      kind: "entry",
      file: { path: "src/new.ts", insertions: 0, deletions: 0, area: "untracked" },
    };
    expect(resolveDiscardDialogCopy(pending)).toEqual({
      title: "Delete untracked file?",
      description: "Delete untracked file new.ts? This can't be undone.",
      confirmLabel: "Delete",
      destructive: true,
    });
  });

  it("restores a deleted tracked entry (non-destructive)", () => {
    const pending: PendingDiscard = {
      kind: "entry",
      file: {
        path: "src/gone.ts",
        insertions: 0,
        deletions: 5,
        area: "unstaged",
        status: "deleted",
      },
    };
    expect(resolveDiscardDialogCopy(pending)).toEqual({
      title: "Restore file?",
      description: "Restore gone.ts?",
      confirmLabel: "Restore",
      destructive: false,
    });
  });

  it("discards a modified tracked entry", () => {
    const pending: PendingDiscard = {
      kind: "entry",
      file: { path: "a/b/c.ts", insertions: 1, deletions: 1, area: "unstaged", status: "modified" },
    };
    expect(resolveDiscardDialogCopy(pending)).toEqual({
      title: "Discard changes?",
      description: "Discard changes to c.ts? This can't be undone.",
      confirmLabel: "Discard",
      destructive: true,
    });
  });

  it("prefers delete over restore for an untracked deletion (area wins)", () => {
    const pending: PendingDiscard = {
      kind: "entry",
      file: { path: "tmp.txt", insertions: 0, deletions: 0, area: "untracked", status: "deleted" },
    };
    expect(resolveDiscardDialogCopy(pending).confirmLabel).toBe("Delete");
  });

  it("keeps the verbatim bulk-discard copy (pluralized, 'cannot be undone')", () => {
    expect(
      resolveDiscardDialogCopy({ kind: "bulk", paths: ["a.ts", "b.ts"], variant: "discard" }),
    ).toEqual({
      title: "Discard changes?",
      description: "Discard 2 files? This cannot be undone.",
      confirmLabel: "Discard",
      destructive: true,
    });
    expect(
      resolveDiscardDialogCopy({ kind: "bulk", paths: ["a.ts"], variant: "discard" }).description,
    ).toBe("Discard 1 file? This cannot be undone.");
  });

  it("labels a bulk untracked delete as delete (can't be undone)", () => {
    expect(
      resolveDiscardDialogCopy({
        kind: "bulk",
        paths: ["x", "y", "z"],
        variant: "delete-untracked",
      }),
    ).toEqual({
      title: "Delete untracked files?",
      description: "Delete 3 untracked files? This can't be undone.",
      confirmLabel: "Delete",
      destructive: true,
    });
  });
});

describe("discardPathsOf", () => {
  it("returns the single path for an entry", () => {
    expect(
      discardPathsOf({
        kind: "entry",
        file: { path: "src/x.ts", insertions: 0, deletions: 0, area: "unstaged" },
      }),
    ).toEqual(["src/x.ts"]);
  });

  it("returns all paths for a bulk set", () => {
    expect(
      discardPathsOf({ kind: "bulk", paths: ["a", "b"], variant: "delete-untracked" }),
    ).toEqual(["a", "b"]);
  });
});
