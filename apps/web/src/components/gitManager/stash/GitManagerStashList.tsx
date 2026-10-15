import type {
  EnvironmentId,
  GitManagerBlockedReason,
  GitManagerChangedFile,
  GitManagerStashEntry,
  ScopedProjectRef,
} from "@bibcode/contracts";
import { LegendList } from "@legendapp/list/react";
import { ArchiveRestoreIcon, PackageOpenIcon, Trash2Icon } from "lucide-react";
import { memo, useCallback, useMemo, useState } from "react";

import { Button } from "~/components/ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPopup,
  DialogTitle,
} from "~/components/ui/dialog";
import { cn } from "~/lib/utils";

import {
  buildStashRows,
  resolveStashActionState,
  resolveStashDiscardDialogCopy,
  type GitManagerStashRow,
} from "./GitManagerStashList.logic";

export const GIT_MANAGER_STASH_ROW_HEIGHT = 29;

const EMPTY_BLOCKED_REASONS: ReadonlyArray<GitManagerBlockedReason> = Object.freeze([]);
const LIST_STYLE = Object.freeze({ height: "100%" });
const fixedItemSize = () => GIT_MANAGER_STASH_ROW_HEIGHT;
const keyExtractor = (row: GitManagerStashRow) => row.sha;

function changedFilesEqual(
  left: ReadonlyArray<GitManagerChangedFile>,
  right: ReadonlyArray<GitManagerChangedFile>,
): boolean {
  if (left === right) return true;
  if (left.length !== right.length) return false;
  return left.every((file, index) => {
    const other = right[index];
    return (
      other !== undefined &&
      file.path === other.path &&
      file.status === other.status &&
      file.insertions === other.insertions &&
      file.deletions === other.deletions
    );
  });
}

function stashRowsEqual(left: GitManagerStashRow, right: GitManagerStashRow): boolean {
  const leftBlocked = left.blocked;
  const rightBlocked = right.blocked;
  return (
    left.index === right.index &&
    left.sha === right.sha &&
    left.message === right.message &&
    left.committedAtMs === right.committedAtMs &&
    left.parents.length === right.parents.length &&
    left.parents.every((parent, index) => parent === right.parents[index]) &&
    changedFilesEqual(left.files, right.files) &&
    (leftBlocked === rightBlocked ||
      (leftBlocked !== null &&
        rightBlocked !== null &&
        leftBlocked.operation === rightBlocked.operation &&
        leftBlocked.code === rightBlocked.code &&
        leftBlocked.message === rightBlocked.message))
  );
}

interface GitManagerStashRowViewProps {
  readonly row: GitManagerStashRow;
  readonly selected: boolean;
  readonly operationInFlight: boolean;
  readonly disabledReason: string | null;
  readonly onSelectStash: (sha: string) => void;
  readonly onApply: (sha: string) => void | Promise<void>;
  readonly onPop: (sha: string) => void | Promise<void>;
  readonly onRequestDrop: (sha: string) => void;
}

function stashRowViewPropsEqual(
  previous: Readonly<GitManagerStashRowViewProps>,
  next: Readonly<GitManagerStashRowViewProps>,
): boolean {
  return (
    stashRowsEqual(previous.row, next.row) &&
    previous.selected === next.selected &&
    previous.operationInFlight === next.operationInFlight &&
    previous.disabledReason === next.disabledReason &&
    previous.onSelectStash === next.onSelectStash &&
    previous.onApply === next.onApply &&
    previous.onPop === next.onPop &&
    previous.onRequestDrop === next.onRequestDrop
  );
}

const GitManagerStashRowView = memo(function GitManagerStashRowView({
  row,
  selected,
  operationInFlight,
  disabledReason,
  onSelectStash,
  onApply,
  onPop,
  onRequestDrop,
}: GitManagerStashRowViewProps) {
  const selector = `stash@{${row.index}}`;
  const actionDisabledReason = disabledReason ?? row.blocked?.message ?? null;
  const descriptionId =
    actionDisabledReason === null ? undefined : `git-manager-stash-${row.sha}-blocked`;
  const actions = resolveStashActionState(row, { operationInFlight });
  const select = useCallback(() => onSelectStash(row.sha), [onSelectStash, row.sha]);
  const apply = useCallback(() => void onApply(row.sha), [onApply, row.sha]);
  const pop = useCallback(() => void onPop(row.sha), [onPop, row.sha]);
  const requestDrop = useCallback(() => onRequestDrop(row.sha), [onRequestDrop, row.sha]);

  return (
    <div
      className={cn(
        "group flex h-[29px] min-w-0 items-center border-b border-border/45",
        selected && "bg-accent text-accent-foreground",
      )}
      data-stash-sha={row.sha}
      role="option"
      aria-selected={selected}
    >
      <Button
        aria-label={`Select stash ${selector}`}
        className="h-full min-w-0 flex-1 justify-start rounded-none border-0 px-2 text-left shadow-none"
        size="xs"
        title={`${selector}: ${row.message}`}
        variant="ghost"
        onClick={select}
      >
        <span className="shrink-0 font-mono text-xs text-muted-foreground">{selector}</span>
        <span className="min-w-0 flex-1 truncate text-xs">{row.message}</span>
        <span className="shrink-0 text-xs text-muted-foreground">
          {row.files.length} {row.files.length === 1 ? "file" : "files"}
        </span>
      </Button>
      <Button
        aria-describedby={descriptionId}
        aria-label={`Apply ${selector}`}
        disabled={disabledReason !== null || !actions.apply.enabled}
        size="icon-xs"
        title={disabledReason ?? actions.apply.reason ?? `Apply ${selector}`}
        variant="ghost"
        onClick={apply}
      >
        <ArchiveRestoreIcon aria-hidden="true" />
      </Button>
      <Button
        aria-describedby={descriptionId}
        aria-label={`Pop ${selector}`}
        disabled={disabledReason !== null || !actions.pop.enabled}
        size="icon-xs"
        title={disabledReason ?? actions.pop.reason ?? `Pop ${selector}`}
        variant="ghost"
        onClick={pop}
      >
        <PackageOpenIcon aria-hidden="true" />
      </Button>
      <Button
        aria-describedby={descriptionId}
        aria-label={`Drop ${selector}`}
        className="text-destructive"
        disabled={disabledReason !== null || !actions.drop.enabled}
        size="icon-xs"
        title={disabledReason ?? actions.drop.reason ?? `Drop ${selector}`}
        variant="ghost"
        onClick={requestDrop}
      >
        <Trash2Icon aria-hidden="true" />
      </Button>
      {actionDisabledReason === null ? null : (
        <span className="sr-only" id={descriptionId}>
          {actionDisabledReason}
        </span>
      )}
    </div>
  );
}, stashRowViewPropsEqual);

export interface GitManagerStashListProps {
  readonly scope: { readonly environmentId: EnvironmentId; readonly cwd: string };
  readonly projectRef: ScopedProjectRef;
  readonly entries: ReadonlyArray<GitManagerStashEntry>;
  readonly blockedReasons?: ReadonlyArray<GitManagerBlockedReason>;
  readonly disabledReason?: string | null;
  readonly selectedSha: string | null;
  readonly onSelectStash: (sha: string) => void;
  readonly onApply: (sha: string) => void | Promise<void>;
  readonly onPop: (sha: string) => void | Promise<void>;
  readonly onDrop: (sha: string) => void | Promise<void>;
  readonly operationInFlight: boolean;
}

export const GitManagerStashList = memo(function GitManagerStashList({
  scope,
  projectRef,
  entries,
  blockedReasons = EMPTY_BLOCKED_REASONS,
  disabledReason = null,
  selectedSha,
  onSelectStash,
  onApply,
  onPop,
  onDrop,
  operationInFlight,
}: GitManagerStashListProps) {
  // LegendList's itemsAreEqual and the row comparator both skip rebuilt rows that are unchanged.
  const rows = useMemo(() => buildStashRows(entries, blockedReasons), [blockedReasons, entries]);
  const [pendingDropSha, setPendingDropSha] = useState<string | null>(null);
  const [dropPending, setDropPending] = useState(false);
  const pendingDropRow =
    pendingDropSha === null ? null : (rows.find((row) => row.sha === pendingDropSha) ?? null);
  const pendingDropCopy =
    pendingDropRow === null ? null : resolveStashDiscardDialogCopy(pendingDropRow);
  const requestDrop = useCallback((sha: string) => setPendingDropSha(sha), []);
  const changeDropDialogOpen = useCallback((open: boolean) => {
    if (!open) setPendingDropSha(null);
  }, []);
  const confirmDrop = async () => {
    if (pendingDropRow === null || dropPending) return;
    setDropPending(true);
    try {
      await onDrop(pendingDropRow.sha);
      setPendingDropSha(null);
    } finally {
      setDropPending(false);
    }
  };
  const renderItem = useCallback(
    ({ item }: { item: GitManagerStashRow; index: number }) => (
      <GitManagerStashRowView
        disabledReason={disabledReason}
        operationInFlight={operationInFlight}
        row={item}
        selected={item.sha === selectedSha}
        onApply={onApply}
        onPop={onPop}
        onRequestDrop={requestDrop}
        onSelectStash={onSelectStash}
      />
    ),
    [disabledReason, onApply, onPop, onSelectStash, operationInFlight, requestDrop, selectedSha],
  );

  return (
    <section
      aria-label="Stashes"
      className="flex min-h-0 flex-1 flex-col"
      data-environment-id={scope.environmentId}
      data-project-id={projectRef.projectId}
      data-worktree-cwd={scope.cwd}
    >
      {rows.length === 0 ? (
        <p className="p-3 text-xs text-muted-foreground">No stashes in this repository.</p>
      ) : (
        <div aria-label="Repository stashes" className="min-h-0 flex-1" role="listbox">
          <LegendList<GitManagerStashRow>
            className="size-full min-w-0 overflow-x-hidden overscroll-y-contain"
            data={rows}
            drawDistance={GIT_MANAGER_STASH_ROW_HEIGHT * 12}
            estimatedItemSize={GIT_MANAGER_STASH_ROW_HEIGHT}
            getFixedItemSize={fixedItemSize}
            itemsAreEqual={stashRowsEqual}
            keyExtractor={keyExtractor}
            recycleItems
            renderItem={renderItem}
            style={LIST_STYLE}
          />
        </div>
      )}
      <Dialog open={pendingDropRow !== null} onOpenChange={changeDropDialogOpen}>
        <DialogPopup>
          <DialogHeader>
            <DialogTitle>{pendingDropCopy?.title ?? "Drop stash?"}</DialogTitle>
            <DialogDescription>{pendingDropCopy?.body}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              disabled={dropPending}
              variant="outline"
              onClick={() => setPendingDropSha(null)}
            >
              Cancel
            </Button>
            <Button
              disabled={dropPending || pendingDropRow === null}
              variant="destructive"
              onClick={() => void confirmDrop()}
            >
              {dropPending ? "Dropping…" : (pendingDropCopy?.confirmLabel ?? "Drop Stash")}
            </Button>
          </DialogFooter>
        </DialogPopup>
      </Dialog>
    </section>
  );
});
