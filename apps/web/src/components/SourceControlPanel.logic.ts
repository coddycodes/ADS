import type { VcsStagingArea, VcsStatusResult, VcsWorkingTreeFileStatus } from "@coddy/contracts";

export interface WorkingTreeFile {
  readonly path: string;
  readonly insertions: number;
  readonly deletions: number;
  readonly status?: VcsWorkingTreeFileStatus;
  readonly area?: VcsStagingArea;
}

export interface WorkingTreeGroups {
  /** Unmerged paths of a merge in progress; staging one marks it resolved. */
  readonly conflicted: WorkingTreeFile[];
  readonly staged: WorkingTreeFile[];
  readonly unstaged: WorkingTreeFile[];
  readonly untracked: WorkingTreeFile[];
}

/**
 * `separateConflicts` moves conflicted paths into their own group; only do so while a
 * dedicated conflict section renders, or they would vanish from the panel.
 */
export function groupFilesByArea(
  files: readonly WorkingTreeFile[],
  options: { readonly separateConflicts?: boolean } = {},
): WorkingTreeGroups {
  const groups: WorkingTreeGroups = { conflicted: [], staged: [], unstaged: [], untracked: [] };
  for (const file of files) {
    if (options.separateConflicts && file.status === "conflicted") groups.conflicted.push(file);
    else if (file.area === "staged") groups.staged.push(file);
    else if (file.area === "untracked") groups.untracked.push(file);
    else groups.unstaged.push(file);
  }
  return groups;
}

export interface FilePathParts {
  readonly dir: string | null;
  readonly name: string;
}

export function splitFilePath(path: string): FilePathParts {
  const index = path.lastIndexOf("/");
  if (index < 0) return { dir: null, name: path };
  return { dir: path.slice(0, index), name: path.slice(index + 1) };
}

export function ignoreFileNamePattern(path: string): string {
  return splitFilePath(path.replaceAll("\\", "/")).name;
}

export function ignoreParentFolderPattern(path: string): string | null {
  const { dir } = splitFilePath(path.replaceAll("\\", "/"));
  return dir ? `${dir}/` : null;
}

export function appendGitignorePattern(contents: string, pattern: string): string {
  const normalizedPattern = pattern.trim();
  if (normalizedPattern.length === 0) return contents;
  const lines = contents.split(/\r?\n/g).map((line) => line.trim());
  if (lines.includes(normalizedPattern)) return contents;
  const prefix = contents.length === 0 || contents.endsWith("\n") ? contents : `${contents}\n`;
  return `${prefix}${normalizedPattern}\n`;
}

export interface ChangeSelectionSummary {
  readonly totalCount: number;
  readonly insertions: number;
  readonly deletions: number;
}

export function summarizeChangeSelection(
  files: readonly WorkingTreeFile[],
): ChangeSelectionSummary {
  const insertions = files.reduce((sum, file) => sum + file.insertions, 0);
  const deletions = files.reduce((sum, file) => sum + file.deletions, 0);
  return {
    totalCount: files.length,
    insertions,
    deletions,
  };
}

export function workingTreeFiles(status: VcsStatusResult | null | undefined): WorkingTreeFile[] {
  return status
    ? status.workingTree.files.map((file) => ({
        path: file.path,
        insertions: file.insertions,
        deletions: file.deletions,
        ...(file.status ? { status: file.status } : {}),
        ...(file.area ? { area: file.area } : {}),
      }))
    : [];
}

/** True when a working-tree file currently sits in the staged area. */
export function isFileStaged(file: WorkingTreeFile): boolean {
  return file.area === "staged";
}

export type StagingToggleAction = "stage" | "unstage";

/**
 * VS Code-style per-file staging: a staged row toggles to unstage; an
 * unstaged or untracked row (or one with no area, i.e. a legacy server)
 * toggles to stage. Pure so the wiring rule can be unit tested without
 * rendering React or simulating a checkbox click.
 */
export function resolveStagingToggleAction(area: VcsStagingArea | undefined): StagingToggleAction {
  return area === "staged" ? "unstage" : "stage";
}

/**
 * Muted "vs <defaultRefName>" context label rendered next to the branch
 * name in the panel header. `null` when the ref is the default ref, or the
 * server hasn't populated the default-ref fields (legacy servers).
 */
export function resolveVsBaseLabel(status: VcsStatusResult | null | undefined): string | null {
  if (!status?.refName || !status.defaultRefName || status.isDefaultRef) {
    return null;
  }
  const aheadOfDefault = status.aheadOfDefaultCount;
  return aheadOfDefault !== undefined && aheadOfDefault > 0
    ? `vs ${status.defaultRefName} ↑${aheadOfDefault}`
    : `vs ${status.defaultRefName}`;
}

/**
 * A pending destructive change awaiting the panel's confirm dialog. A single
 * row ("entry") derives its copy from the file's area/status; a section header
 * ("bulk") carries the whole set and a variant selecting discard vs. delete.
 */
export type PendingDiscard =
  | { readonly kind: "entry"; readonly file: WorkingTreeFile }
  | {
      readonly kind: "bulk";
      readonly paths: readonly string[];
      readonly variant: "discard" | "delete-untracked";
    };

export interface DiscardDialogCopy {
  readonly title: string;
  readonly description: string;
  readonly confirmLabel: string;
  /** Discard / delete are irreversible; restore is not. Styles the button. */
  readonly destructive: boolean;
}

/**
 * Copy for the discard/delete/restore confirm dialog. For a single entry the
 * precedence is untracked -> delete (an untracked file is never "restored"),
 * then a tracked deletion -> restore, else discard. Per-entry copy reads
 * "can't be undone"; the bulk-discard string is kept verbatim ("cannot be
 * undone") so the pre-existing section behavior is unchanged.
 */
export function resolveDiscardDialogCopy(pending: PendingDiscard): DiscardDialogCopy {
  if (pending.kind === "entry") {
    const name = splitFilePath(pending.file.path).name;
    if (pending.file.area === "untracked") {
      return {
        title: "Delete untracked file?",
        description: `Delete untracked file ${name}? This can't be undone.`,
        confirmLabel: "Delete",
        destructive: true,
      };
    }
    if (pending.file.status === "deleted") {
      return {
        title: "Restore file?",
        description: `Restore ${name}?`,
        confirmLabel: "Restore",
        destructive: false,
      };
    }
    return {
      title: "Discard changes?",
      description: `Discard changes to ${name}? This can't be undone.`,
      confirmLabel: "Discard",
      destructive: true,
    };
  }

  const count = pending.paths.length;
  const plural = count === 1 ? "" : "s";
  if (pending.variant === "delete-untracked") {
    return {
      title: "Delete untracked files?",
      description: `Delete ${count} untracked file${plural}? This can't be undone.`,
      confirmLabel: "Delete",
      destructive: true,
    };
  }
  return {
    title: "Discard changes?",
    description: `Discard ${count} file${plural}? This cannot be undone.`,
    confirmLabel: "Discard",
    destructive: true,
  };
}

/** The concrete paths the panel hands to `discardFiles` on confirm. */
export function discardPathsOf(pending: PendingDiscard): readonly string[] {
  return pending.kind === "entry" ? [pending.file.path] : pending.paths;
}
