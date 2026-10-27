import type { VcsStagingArea, VcsWorkingTreeFileStatus } from "@coddy/contracts";

export type ChangeInclusion = "all" | "partial" | "none";

export function nextChangeInclusion(current: ChangeInclusion): Exclude<ChangeInclusion, "partial"> {
  return current === "none" ? "all" : "none";
}

export interface ChangeFile {
  readonly path: string;
  readonly insertions: number;
  readonly deletions: number;
  readonly status?: VcsWorkingTreeFileStatus | undefined;
  readonly area?: VcsStagingArea | undefined;
}

export interface ChangeRow {
  readonly path: string;
  readonly status: VcsWorkingTreeFileStatus | undefined;
  readonly area: VcsStagingArea | undefined;
  readonly insertions: number;
  readonly deletions: number;
  readonly inclusion: ChangeInclusion;
  readonly conflicted: boolean;
  readonly submodule: boolean;
  readonly disabledReason: string | null;
}

export interface ChangeFilters {
  readonly included: boolean;
  readonly excluded: boolean;
  readonly new: boolean;
  readonly modified: boolean;
  readonly deleted: boolean;
}

export interface ChangeSubmoduleState {
  readonly path: string;
  readonly inclusion: Extract<ChangeInclusion, "partial" | "none">;
  /** Server-authored explanation, rendered verbatim by the row. */
  readonly disabledReason: string;
}

export type ChangeRows = ChangeRow[] & {
  readonly filterActive: boolean;
  readonly hiddenIncludedCount: number;
  readonly totalCount: number;
};

export interface ChangeRowsHeader {
  readonly inclusion: ChangeInclusion;
  readonly label: string;
}

export interface BuildChangeRowsInput {
  readonly files: ReadonlyArray<ChangeFile>;
  readonly conflictedPaths: ReadonlyArray<string>;
  readonly submodulePaths: ReadonlyArray<string | ChangeSubmoduleState>;
  readonly filterText: string;
  readonly filters?: ChangeFilters;
  readonly excludedPaths: ReadonlySet<string>;
}

export const DEFAULT_CHANGE_FILTERS: ChangeFilters = Object.freeze({
  included: false,
  excluded: false,
  new: false,
  modified: false,
  deleted: false,
});

function matchesStatusFilter(status: VcsWorkingTreeFileStatus | undefined, filters: ChangeFilters) {
  if (filters.new && status !== "added" && status !== "untracked") return false;
  // A conflicted path is modified on both sides of the merge.
  if (
    filters.modified &&
    status !== "modified" &&
    status !== "renamed" &&
    status !== "copied" &&
    status !== "conflicted"
  ) {
    return false;
  }
  return !filters.deleted || status === "deleted";
}

function collapseFilesByPath(files: ReadonlyArray<ChangeFile>): ChangeFile[] {
  const byPath = new Map<string, ChangeFile>();
  for (const file of files) {
    const previous = byPath.get(file.path);
    if (previous === undefined) {
      byPath.set(file.path, file);
      continue;
    }
    const status =
      previous.status === file.status ? previous.status : (file.status ?? previous.status);
    const area = previous.area === file.area ? previous.area : undefined;
    byPath.set(file.path, {
      path: file.path,
      insertions: previous.insertions + file.insertions,
      deletions: previous.deletions + file.deletions,
      ...(status === undefined ? {} : { status }),
      ...(area === undefined ? {} : { area }),
    });
  }
  return [...byPath.values()];
}

export function changeRowsHeader(rows: ChangeRows): ChangeRowsHeader {
  const eligibleRows = rows.filter((row) => !row.conflicted && row.disabledReason === null);
  const includedCount = eligibleRows.reduce(
    (count, row) => count + (row.inclusion === "all" ? 1 : 0),
    0,
  );
  const hasPartial = eligibleRows.some((row) => row.inclusion === "partial");
  const inclusion: ChangeInclusion =
    hasPartial || (includedCount > 0 && includedCount < eligibleRows.length)
      ? "partial"
      : includedCount === eligibleRows.length && eligibleRows.length > 0
        ? "all"
        : "none";
  return {
    inclusion,
    label: rows.filterActive
      ? `${rows.length} of ${rows.totalCount} changed files`
      : `${rows.totalCount} changed files`,
  };
}

export function buildChangeRows(input: BuildChangeRowsInput): ChangeRows {
  const conflictedPaths = new Set(input.conflictedPaths);
  const submodulePaths = new Map(
    input.submodulePaths.map((submodule) =>
      typeof submodule === "string"
        ? ([submodule, null] as const)
        : ([submodule.path, submodule] as const),
    ),
  );
  const filters = input.filters ?? DEFAULT_CHANGE_FILTERS;
  const normalizedText = input.filterText.trim().toLowerCase();
  const filterActive =
    normalizedText.length > 0 || Object.values(filters).some((enabled) => enabled);
  const allRows: ChangeRow[] = collapseFilesByPath(input.files).map((file) => {
    const submoduleState = submodulePaths.get(file.path);
    const conflicted = conflictedPaths.has(file.path);
    return {
      path: file.path,
      status: file.status,
      area: file.area,
      insertions: file.insertions,
      deletions: file.deletions,
      inclusion: conflicted
        ? "none"
        : (submoduleState?.inclusion ?? (input.excludedPaths.has(file.path) ? "none" : "all")),
      conflicted,
      submodule: submodulePaths.has(file.path),
      disabledReason: submoduleState?.disabledReason ?? null,
    };
  });
  const visibleRows = allRows.filter((row) => {
    if (normalizedText.length > 0 && !row.path.toLowerCase().includes(normalizedText)) {
      return false;
    }
    if (filters.included && row.inclusion === "none") return false;
    if (filters.excluded && row.inclusion !== "none") return false;
    return matchesStatusFilter(row.status, filters);
  });
  const visibleIncludedCount = visibleRows.reduce(
    (count, row) => count + (row.inclusion === "none" ? 0 : 1),
    0,
  );
  const totalIncludedCount = allRows.reduce(
    (count, row) => count + (row.inclusion === "none" ? 0 : 1),
    0,
  );

  return Object.assign(visibleRows, {
    filterActive,
    hiddenIncludedCount: totalIncludedCount - visibleIncludedCount,
    totalCount: allRows.length,
  });
}
