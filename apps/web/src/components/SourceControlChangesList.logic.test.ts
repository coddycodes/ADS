import { describe, expect, it } from "vite-plus/test";

import {
  buildSourceControlRows,
  fileCountLabel,
  folderActionTarget,
  folderCheckboxState,
  groupFilesByDirectory,
  ROOT_FOLDER_LABEL,
  sectionItemKey,
  type SourceControlListRow,
} from "./SourceControlChangesList.logic";
import type { WorkingTreeFile } from "./SourceControlPanel.logic";

function file(path: string): WorkingTreeFile {
  return { path, insertions: 1, deletions: 0 };
}

describe("groupFilesByDirectory", () => {
  it("returns no groups for no files", () => {
    expect(groupFilesByDirectory([])).toEqual([]);
  });

  it("puts root files in a leading group labeled /", () => {
    const groups = groupFilesByDirectory([file("src/app.ts"), file("README.md")]);
    expect(groups.map((group) => group.label)).toEqual([ROOT_FOLDER_LABEL, "src"]);
    expect(groups[0]?.directory).toBeNull();
    expect(groups[0]?.files.map((entry) => entry.path)).toEqual(["README.md"]);
    expect(groups[1]?.directory).toBe("src");
  });

  it("sorts directories in path order so a parent precedes its deeper siblings", () => {
    const groups = groupFilesByDirectory([
      file("src/b/c/deep.ts"),
      file("src/a/one.ts"),
      file("docs/guide.md"),
    ]);
    expect(groups.map((group) => group.label)).toEqual(["docs", "src/a", "src/b/c"]);
  });

  it("sorts numbered directories numerically", () => {
    const groups = groupFilesByDirectory([
      file("run/10/log.txt"),
      file("run/2/log.txt"),
      file("run/1/log.txt"),
    ]);
    expect(groups.map((group) => group.label)).toEqual(["run/1", "run/2", "run/10"]);
  });

  it("keeps every file of a directory in one group, in server order", () => {
    const groups = groupFilesByDirectory([file("src/z.ts"), file("docs/a.md"), file("src/a.ts")]);
    expect(groups.map((group) => group.files.map((entry) => entry.path))).toEqual([
      ["docs/a.md"],
      ["src/z.ts", "src/a.ts"],
    ]);
  });
});

describe("folderCheckboxState", () => {
  const files = [file("src/a.ts"), file("src/b.ts")];

  it("reports none when no file is checked", () => {
    expect(folderCheckboxState(files, () => false)).toBe("none");
  });

  it("reports all when every file is checked", () => {
    expect(folderCheckboxState(files, () => true)).toBe("all");
  });

  it("reports mixed when only some files are checked", () => {
    expect(folderCheckboxState(files, (entry) => entry.path === "src/a.ts")).toBe("mixed");
  });
});

describe("folderActionTarget", () => {
  it("names the directory", () => {
    expect(folderActionTarget({ directory: "src/app", label: "src/app", files: [] })).toBe(
      "src/app",
    );
  });

  it("names the repository root for rootless files", () => {
    expect(folderActionTarget({ directory: null, label: ROOT_FOLDER_LABEL, files: [] })).toBe(
      "the repository root",
    );
  });
});

describe("fileCountLabel", () => {
  it("keeps the count singular for one file", () => {
    expect(fileCountLabel(1)).toBe("1 file");
  });

  it("pluralizes every other count", () => {
    expect(fileCountLabel(0)).toBe("0 files");
    expect(fileCountLabel(4)).toBe("4 files");
  });
});

describe("buildSourceControlRows", () => {
  const NONE: ReadonlySet<string> = new Set();
  const sections = [
    { title: "Staged Changes", files: [file("src/app.ts")] },
    { title: "Changes", files: [file("src/app.ts"), file("README.md")] },
    { title: "Untracked Files", files: [] },
  ];
  const describeRow = (row: SourceControlListRow) =>
    row.kind === "section"
      ? `section ${row.sectionIndex}${row.collapsed ? " collapsed" : ""}`
      : row.kind === "folder"
        ? `folder ${row.group.label}${row.collapsed ? " collapsed" : ""}`
        : `file ${row.file.path}${row.showDirectory ? " flat" : ""}`;

  it("flattens non-empty sections into headers, folders and files", () => {
    const rows = buildSourceControlRows(sections, {
      groupByFolder: true,
      collapsedSections: NONE,
      collapsedFolders: NONE,
    });
    expect(rows.map(describeRow)).toEqual([
      "section 0",
      "folder src",
      "file src/app.ts",
      "section 1",
      "folder /",
      "file README.md",
      "folder src",
      "file src/app.ts",
    ]);
  });

  it("keys rows per section so a partially staged path stays unique", () => {
    const rows = buildSourceControlRows(sections, {
      groupByFolder: true,
      collapsedSections: NONE,
      collapsedFolders: NONE,
    });
    const keys = rows.map((row) => row.key);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("lists files flat with their directories when grouping is off", () => {
    const rows = buildSourceControlRows(sections, {
      groupByFolder: false,
      collapsedSections: NONE,
      collapsedFolders: NONE,
    });
    expect(rows.map(describeRow)).toEqual([
      "section 0",
      "file src/app.ts flat",
      "section 1",
      "file src/app.ts flat",
      "file README.md flat",
    ]);
  });

  it("keeps only the header of a collapsed section or folder", () => {
    const rows = buildSourceControlRows(sections, {
      groupByFolder: true,
      collapsedSections: new Set(["Staged Changes"]),
      collapsedFolders: new Set([sectionItemKey("Changes", "src")]),
    });
    expect(rows.map(describeRow)).toEqual([
      "section 0 collapsed",
      "section 1",
      "folder /",
      "file README.md",
      "folder src collapsed",
    ]);
  });
});
