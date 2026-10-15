import { LegendList } from "@legendapp/list/react";
import { memo, useCallback, useState } from "react";

import { GitManagerChangeRow } from "./GitManagerChangeRow";
import type { ChangeRow } from "./changesList.logic";

export const GIT_MANAGER_CHANGE_ROW_HEIGHT = 29;

export interface GitManagerChangesListProps {
  readonly rows: ReadonlyArray<ChangeRow>;
  readonly selectedPath: string | null;
  readonly onSelect: (path: string) => void;
  readonly onToggle: (path: string) => void;
  readonly onContextMenu: (path: string, position: { x: number; y: number }) => void;
  readonly onOpenExternal: (path: string) => void;
}

const LIST_STYLE = Object.freeze({ height: "100%" });
const keyExtractor = (row: ChangeRow) => row.path;
const fixedItemSize = () => GIT_MANAGER_CHANGE_ROW_HEIGHT;

function changeRowsEqual(left: ChangeRow, right: ChangeRow): boolean {
  return (
    left.path === right.path &&
    left.status === right.status &&
    left.area === right.area &&
    left.insertions === right.insertions &&
    left.deletions === right.deletions &&
    left.inclusion === right.inclusion &&
    left.conflicted === right.conflicted &&
    left.submodule === right.submodule &&
    left.disabledReason === right.disabledReason
  );
}

interface StableChangeRows {
  readonly source: ReadonlyArray<ChangeRow>;
  readonly rows: ReadonlyArray<ChangeRow>;
  readonly byPath: ReadonlyMap<string, ChangeRow>;
}

// Reuses the previous row object for every path whose row is unchanged.
function stabilizeChangeRows(
  rows: ReadonlyArray<ChangeRow>,
  previousByPath: ReadonlyMap<string, ChangeRow>,
): StableChangeRows {
  const byPath = new Map<string, ChangeRow>();
  const stableRows = rows.map((row) => {
    const previous = previousByPath.get(row.path);
    const stable = previous !== undefined && changeRowsEqual(previous, row) ? previous : row;
    byPath.set(row.path, stable);
    return stable;
  });
  return { source: rows, rows: stableRows, byPath };
}

export const GitManagerChangesList = memo(function GitManagerChangesList({
  rows,
  selectedPath,
  onSelect,
  onToggle,
  onContextMenu,
  onOpenExternal,
}: GitManagerChangesListProps) {
  // React's "store information from previous renders" pattern: a new rows array
  // restarts this render with rows that keep the previous objects where unchanged.
  const [storedRows, setStoredRows] = useState(() => stabilizeChangeRows(rows, new Map()));
  let stable = storedRows;
  if (stable.source !== rows) {
    stable = stabilizeChangeRows(rows, stable.byPath);
    setStoredRows(stable);
  }
  const stableRows = stable.rows;
  const renderItem = useCallback(
    ({ item }: { item: ChangeRow }) => (
      <GitManagerChangeRow
        row={item}
        selected={item.path === selectedPath}
        onSelect={onSelect}
        onToggle={onToggle}
        onContextMenu={onContextMenu}
        onOpenExternal={onOpenExternal}
      />
    ),
    [onContextMenu, onOpenExternal, onSelect, onToggle, selectedPath],
  );

  return (
    <div className="min-h-0 flex-1" role="listbox" aria-label="Changed files">
      <LegendList<ChangeRow>
        className="size-full min-w-0 overflow-x-hidden overscroll-y-contain"
        data={stableRows}
        drawDistance={GIT_MANAGER_CHANGE_ROW_HEIGHT * 12}
        estimatedItemSize={GIT_MANAGER_CHANGE_ROW_HEIGHT}
        extraData={selectedPath}
        getFixedItemSize={fixedItemSize}
        itemsAreEqual={changeRowsEqual}
        keyExtractor={keyExtractor}
        recycleItems
        renderItem={renderItem}
        style={LIST_STYLE}
      />
    </div>
  );
});
