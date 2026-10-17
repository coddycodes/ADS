import { splitFilePath, type WorkingTreeFile } from "./SourceControlPanel.logic";

/**
 * One folder's worth of pending changes. `directory` is the repo-relative
 * directory the files live in, or `null` for files at the repository root;
 * `label` is what the folder header renders.
 */
export interface SourceControlFolderGroup {
  readonly directory: string | null;
  readonly label: string;
  readonly files: WorkingTreeFile[];
}

/** Header label for files that sit directly in the repository root. */
export const ROOT_FOLDER_LABEL = "/";

const collator = new Intl.Collator(undefined, { numeric: true });

/**
 * Groups working-tree files by their directory for the folder view. Root files
 * come first, then directories in path order, so a folder header always sits
 * directly above the files it contains. File order inside a group is the order
 * the server reported.
 */
export function groupFilesByDirectory(
  files: readonly WorkingTreeFile[],
): ReadonlyArray<SourceControlFolderGroup> {
  const byDirectory = new Map<string, WorkingTreeFile[]>();
  for (const file of files) {
    const key = splitFilePath(file.path).dir ?? "";
    const existing = byDirectory.get(key);
    if (existing) existing.push(file);
    else byDirectory.set(key, [file]);
  }
  return [...byDirectory.entries()]
    .sort(([left], [right]) => {
      if (left === right) return 0;
      if (left === "") return -1;
      if (right === "") return 1;
      return collator.compare(left, right);
    })
    .map(([directory, groupFiles]) => ({
      directory: directory === "" ? null : directory,
      label: directory === "" ? ROOT_FOLDER_LABEL : directory,
      files: groupFiles,
    }));
}

/** Tri-state of a folder header checkbox over the files it covers. */
export type FolderCheckboxState = "all" | "none" | "mixed";

/**
 * Whether all, none, or some of a folder's files are checked (staged) or
 * selected. An empty folder never renders, so it reports "none".
 */
export function folderCheckboxState(
  files: readonly WorkingTreeFile[],
  isChecked: (file: WorkingTreeFile) => boolean,
): FolderCheckboxState {
  let checked = 0;
  for (const file of files) {
    if (isChecked(file)) checked += 1;
  }
  if (checked === 0) return "none";
  return checked === files.length ? "all" : "mixed";
}

/**
 * How a folder header names its target in accessible labels: the directory
 * itself, or the repository root for files with no directory.
 */
export function folderActionTarget(group: SourceControlFolderGroup): string {
  return group.directory ?? "the repository root";
}

/** "1 file" / "3 files", for a folder header's accessible name. */
export function fileCountLabel(count: number): string {
  return `${count} ${count === 1 ? "file" : "files"}`;
}

/**
 * One row of the virtualized Source Control list. Every section, folder header
 * and file is a row of one flat list, so only the rows in view mount (#119).
 * `sectionIndex` points into the sections the list was built from.
 */
export type SourceControlListRow =
  | {
      readonly kind: "section";
      readonly key: string;
      readonly sectionIndex: number;
      readonly collapsed: boolean;
    }
  | {
      readonly kind: "folder";
      readonly key: string;
      readonly sectionIndex: number;
      readonly group: SourceControlFolderGroup;
      readonly collapsed: boolean;
    }
  | {
      readonly kind: "file";
      readonly key: string;
      readonly sectionIndex: number;
      readonly file: WorkingTreeFile;
      readonly showDirectory: boolean;
    };

/**
 * Identifies a folder within its section. A path can be both staged and
 * unstaged, so folders and files are keyed per section.
 */
export function sectionItemKey(sectionTitle: string, item: string): string {
  return `${sectionTitle}\u0000${item}`;
}

/**
 * Flattens the sections into list rows: a header per non-empty section, then
 * its folder headers and files unless collapsed. Grouping off lists the files
 * flat with their own directories.
 */
export function buildSourceControlRows(
  sections: ReadonlyArray<{ readonly title: string; readonly files: readonly WorkingTreeFile[] }>,
  options: {
    readonly groupByFolder: boolean;
    readonly collapsedSections: ReadonlySet<string>;
    readonly collapsedFolders: ReadonlySet<string>;
  },
): SourceControlListRow[] {
  const rows: SourceControlListRow[] = [];
  sections.forEach((section, sectionIndex) => {
    if (section.files.length === 0) return;
    const collapsed = options.collapsedSections.has(section.title);
    rows.push({ kind: "section", key: `section:${section.title}`, sectionIndex, collapsed });
    if (collapsed) return;
    if (!options.groupByFolder) {
      for (const file of section.files) {
        const key = `file:${sectionItemKey(section.title, file.path)}`;
        rows.push({ kind: "file", key, sectionIndex, file, showDirectory: true });
      }
      return;
    }
    for (const group of groupFilesByDirectory(section.files)) {
      const folderKey = sectionItemKey(section.title, group.label);
      const folderCollapsed = options.collapsedFolders.has(folderKey);
      rows.push({
        kind: "folder",
        key: `folder:${folderKey}`,
        sectionIndex,
        group,
        collapsed: folderCollapsed,
      });
      if (folderCollapsed) continue;
      for (const file of group.files) {
        const key = `file:${sectionItemKey(section.title, file.path)}`;
        rows.push({ kind: "file", key, sectionIndex, file, showDirectory: false });
      }
    }
  });
  return rows;
}
