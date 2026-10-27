import { projectKey } from "@coddy/client-runtime/state/entities";
import type {
  GitManagerBlockedReason,
  GitManagerRefEntry,
  ScopedProjectRef,
} from "@coddy/contracts";
import { LegendList } from "@legendapp/list/react";
import {
  CheckIcon,
  ChevronDownIcon,
  GitBranchIcon,
  GitMergeIcon,
  PlusIcon,
  SearchIcon,
} from "lucide-react";
import {
  memo,
  type ChangeEvent,
  type CSSProperties,
  useCallback,
  useDeferredValue,
  useMemo,
  useState,
} from "react";

import { Button } from "~/components/ui/button";
import { Input } from "~/components/ui/input";
import { Popover, PopoverPopup, PopoverTrigger } from "~/components/ui/popover";
import { cn } from "~/lib/utils";

import {
  DEFAULT_GIT_MANAGER_TOOLBAR_VIEW_STATE,
  useGitManagerStore,
} from "../../../gitManagerStore";
import { groupBranches } from "./branchGrouping";

const BRANCH_ROW_HEIGHT = 30;
const BRANCH_LIST_STYLE: CSSProperties = Object.freeze({ height: "15rem" });
const KNOWN_BLOCKED_CODES = new Set([
  "worktree-checked-out",
  "dirty-working-tree",
  "operation-in-flight",
  "merge-in-progress",
  "current-branch",
  "default-branch",
  "no-upstream",
  "detached-head",
  "no-remote",
]);

type BranchListItem =
  | { readonly kind: "header"; readonly key: string; readonly label: string }
  | {
      readonly kind: "branch";
      readonly key: string;
      readonly ref: GitManagerRefEntry;
      readonly isRemote: boolean;
    };

function branchListItemKey(item: BranchListItem): string {
  return item.key;
}

function branchListItemType(item: BranchListItem): string {
  return item.kind;
}

function blockedReasonsEqual(
  left: ReadonlyArray<GitManagerBlockedReason>,
  right: ReadonlyArray<GitManagerBlockedReason>,
): boolean {
  if (left === right) return true;
  if (left.length !== right.length) return false;
  return left.every((reason, index) => {
    const other = right[index];
    return (
      other !== undefined &&
      reason.operation === other.operation &&
      reason.code === other.code &&
      reason.message === other.message
    );
  });
}

function branchRowPropsEqual(
  previous: Readonly<GitManagerBranchRowProps>,
  next: Readonly<GitManagerBranchRowProps>,
): boolean {
  const left = previous.refEntry;
  const right = next.refEntry;
  return (
    previous.selectedWorktreeCwd === next.selectedWorktreeCwd &&
    previous.isRemote === next.isRemote &&
    previous.mergeMode === next.mergeMode &&
    previous.branchDisabledReason === next.branchDisabledReason &&
    previous.mergeDisabledReason === next.mergeDisabledReason &&
    previous.onSelectBranch === next.onSelectBranch &&
    previous.onSwitchWorktree === next.onSwitchWorktree &&
    previous.onMergeInto === next.onMergeInto &&
    previous.onRenameBranch === next.onRenameBranch &&
    previous.onDeleteBranch === next.onDeleteBranch &&
    left.name === right.name &&
    left.tipSha === right.tipSha &&
    left.upstream === right.upstream &&
    left.ahead === right.ahead &&
    left.behind === right.behind &&
    left.current === right.current &&
    left.isDefault === right.isDefault &&
    left.worktreePath === right.worktreePath &&
    blockedReasonsEqual(left.blocked, right.blocked)
  );
}

interface GitManagerBranchRowProps {
  readonly refEntry: GitManagerRefEntry;
  readonly isRemote: boolean;
  readonly selectedWorktreeCwd: string;
  readonly mergeMode: boolean;
  readonly branchDisabledReason: string | null;
  readonly mergeDisabledReason: string | null;
  readonly onSelectBranch: (ref: GitManagerRefEntry, isRemote: boolean) => void;
  readonly onSwitchWorktree: (worktreePath: string) => void;
  readonly onMergeInto: (ref: GitManagerRefEntry) => void;
  readonly onRenameBranch: (ref: GitManagerRefEntry) => void;
  readonly onDeleteBranch: (ref: GitManagerRefEntry) => void;
}

const GitManagerBranchRow = memo(function GitManagerBranchRow({
  refEntry,
  isRemote,
  selectedWorktreeCwd,
  mergeMode,
  branchDisabledReason,
  mergeDisabledReason,
  onSelectBranch,
  onSwitchWorktree,
  onMergeInto,
  onRenameBranch,
  onDeleteBranch,
}: GitManagerBranchRowProps) {
  const redirectPath =
    refEntry.worktreePath !== null && refEntry.worktreePath !== selectedWorktreeCwd
      ? refEntry.worktreePath
      : null;
  const unknownReason = refEntry.blocked.find((reason) => !KNOWN_BLOCKED_CODES.has(reason.code));
  const checkoutBlock = refEntry.blocked.find(
    (reason) =>
      redirectPath === null &&
      !(reason.operation === "branch-checkout" && reason.code === "dirty-working-tree"),
  );
  const blockedReason = unknownReason ?? checkoutBlock ?? null;
  const capabilityDisabledReason = mergeMode
    ? mergeDisabledReason
    : redirectPath === null
      ? branchDisabledReason
      : null;
  const currentMergeReason =
    mergeMode && refEntry.current ? "Choose a different branch to merge." : null;
  const displayReason =
    capabilityDisabledReason ??
    blockedReason?.message ??
    currentMergeReason ??
    refEntry.blocked[0]?.message ??
    null;
  const descriptionId =
    displayReason === null
      ? undefined
      : `git-manager-branch-${isRemote ? "remote" : "local"}-${encodeURIComponent(refEntry.name)}`;
  const branchActionDescriptionId =
    branchDisabledReason === null
      ? undefined
      : `git-manager-branch-${isRemote ? "remote" : "local"}-${encodeURIComponent(refEntry.name)}-actions`;
  const disabled =
    capabilityDisabledReason !== null || blockedReason !== null || currentMergeReason !== null;
  const title = displayReason ?? undefined;
  const activate = useCallback(() => {
    if (disabled) return;
    if (mergeMode) {
      onMergeInto(refEntry);
      return;
    }
    if (redirectPath !== null) {
      onSwitchWorktree(redirectPath);
      return;
    }
    onSelectBranch(refEntry, isRemote);
  }, [
    disabled,
    isRemote,
    mergeMode,
    onMergeInto,
    onSelectBranch,
    onSwitchWorktree,
    redirectPath,
    refEntry,
  ]);

  return (
    <>
      <div className="group flex h-[30px] min-w-0 items-center">
        <button
          aria-label={isRemote ? `Check out remote branch ${refEntry.name}` : undefined}
          aria-describedby={descriptionId}
          className="flex h-full min-w-0 flex-1 items-center gap-2 px-2 text-left text-xs outline-none hover:bg-accent/55 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring disabled:opacity-60"
          disabled={disabled}
          title={title}
          type="button"
          onClick={activate}
        >
          <span className="inline-flex size-4 shrink-0 items-center justify-center">
            {refEntry.current ? (
              <CheckIcon aria-label="Current branch" className="size-3.5" />
            ) : null}
          </span>
          <span className="min-w-0 shrink truncate font-mono">{refEntry.name}</span>
          {displayReason === null ? null : (
            <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
              {displayReason}
            </span>
          )}
          {redirectPath === null ? null : (
            <span className="ml-auto flex shrink-0 items-center gap-1 rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
              <span>Switch to worktree</span>
              <span className="max-w-28 truncate font-mono">{redirectPath}</span>
            </span>
          )}
        </button>
        {isRemote ? null : (
          <>
            <button
              aria-describedby={branchActionDescriptionId}
              aria-label={`Rename ${refEntry.name}`}
              className="pointer-events-none h-full shrink-0 px-1.5 text-xs text-muted-foreground opacity-0 hover:bg-accent group-focus-within:pointer-events-auto group-focus-within:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100 focus:pointer-events-auto focus:opacity-100 pointer-coarse:pointer-events-auto pointer-coarse:opacity-100"
              disabled={branchDisabledReason !== null}
              title={branchDisabledReason ?? undefined}
              type="button"
              onClick={() => onRenameBranch(refEntry)}
            >
              Rename
            </button>
            <button
              aria-describedby={branchActionDescriptionId}
              aria-label={`Delete ${refEntry.name}`}
              className="pointer-events-none h-full shrink-0 px-1.5 text-xs text-destructive opacity-0 hover:bg-destructive/10 group-focus-within:pointer-events-auto group-focus-within:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100 focus:pointer-events-auto focus:opacity-100 pointer-coarse:pointer-events-auto pointer-coarse:opacity-100"
              disabled={branchDisabledReason !== null}
              title={branchDisabledReason ?? undefined}
              type="button"
              onClick={() => onDeleteBranch(refEntry)}
            >
              Delete
            </button>
          </>
        )}
      </div>
      {displayReason === null ? null : (
        <span className="sr-only" id={descriptionId}>
          {displayReason}
        </span>
      )}
      {branchDisabledReason === null ? null : (
        <span className="sr-only" id={branchActionDescriptionId}>
          {branchDisabledReason}
        </span>
      )}
    </>
  );
}, branchRowPropsEqual);

export interface GitManagerBranchDropdownProps {
  readonly projectRef: ScopedProjectRef;
  readonly refs: ReadonlyArray<GitManagerRefEntry>;
  readonly remoteRefs: ReadonlyArray<GitManagerRefEntry>;
  readonly recentNames: ReadonlyArray<string>;
  readonly currentBranchName: string | null;
  /** Shown instead of a branch name when none is checked out. */
  readonly noBranchLabel: string;
  readonly triggerDisabledReason: string | null;
  readonly selectedWorktreeCwd: string;
  readonly branchDisabledReason: string | null;
  readonly mergeDisabledReason: string | null;
  readonly onSelectBranch: (ref: GitManagerRefEntry, isRemote: boolean) => void;
  readonly onSwitchWorktree: (worktreePath: string) => void;
  readonly onCreateBranch: () => void;
  readonly onMergeInto: (ref: GitManagerRefEntry) => void;
  readonly onRenameBranch?: (ref: GitManagerRefEntry) => void;
  readonly onDeleteBranch?: (ref: GitManagerRefEntry) => void;
}

export const GitManagerBranchDropdown = memo(function GitManagerBranchDropdown({
  projectRef,
  refs,
  remoteRefs,
  recentNames,
  currentBranchName,
  noBranchLabel,
  triggerDisabledReason,
  selectedWorktreeCwd,
  branchDisabledReason,
  mergeDisabledReason,
  onSelectBranch,
  onSwitchWorktree,
  onCreateBranch,
  onMergeInto,
  onRenameBranch,
  onDeleteBranch,
}: GitManagerBranchDropdownProps) {
  const storeKey = projectKey(projectRef);
  const selectFilterText = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      (state.toolbarByProjectKey[storeKey] ?? DEFAULT_GIT_MANAGER_TOOLBAR_VIEW_STATE)
        .branchFilterText,
    [storeKey],
  );
  const selectOpenDropdown = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      (state.toolbarByProjectKey[storeKey] ?? DEFAULT_GIT_MANAGER_TOOLBAR_VIEW_STATE).openDropdown,
    [storeKey],
  );
  const filterText = useGitManagerStore(selectFilterText);
  const openDropdown = useGitManagerStore(selectOpenDropdown);
  const setBranchFilterText = useGitManagerStore((state) => state.setBranchFilterText);
  const setOpenDropdown = useGitManagerStore((state) => state.setOpenDropdown);
  const deferredFilterText = useDeferredValue(filterText);
  const [mergeMode, setMergeMode] = useState(false);
  const grouped = useMemo(
    () => groupBranches({ refs, remoteRefs, recentNames, filter: deferredFilterText }),
    [deferredFilterText, recentNames, refs, remoteRefs],
  );
  const listItems = useMemo<ReadonlyArray<BranchListItem>>(() => {
    const items: BranchListItem[] = [];
    const appendGroup = (
      label: string,
      branches: ReadonlyArray<GitManagerRefEntry>,
      isRemote: boolean,
    ) => {
      if (branches.length === 0) return;
      items.push({ kind: "header", key: `header:${label}`, label });
      for (const ref of branches) {
        items.push({
          kind: "branch",
          key: `${isRemote ? "remote" : "local"}:${ref.name}`,
          ref,
          isRemote,
        });
      }
    };
    appendGroup("Default", grouped.default, false);
    appendGroup("Recent", grouped.recent, false);
    appendGroup("Other", grouped.other, false);
    if (!mergeMode) appendGroup("Remote branches", grouped.remote, true);
    return items;
  }, [grouped.default, grouped.other, grouped.recent, grouped.remote, mergeMode]);
  const stableProjectRef = useMemo(
    () =>
      ({
        environmentId: projectRef.environmentId,
        projectId: projectRef.projectId,
      }) as ScopedProjectRef,
    [projectRef.environmentId, projectRef.projectId],
  );
  const handleOpenChange = useCallback(
    (open: boolean) => {
      setOpenDropdown(stableProjectRef, open ? "branch" : null);
      if (!open) setMergeMode(false);
    },
    [setOpenDropdown, stableProjectRef],
  );
  const handleFilterChange = useCallback(
    (event: ChangeEvent<HTMLInputElement>) =>
      setBranchFilterText(stableProjectRef, event.currentTarget.value),
    [setBranchFilterText, stableProjectRef],
  );
  const handleCreateBranch = useCallback(() => {
    onCreateBranch();
    setOpenDropdown(stableProjectRef, null);
  }, [onCreateBranch, setOpenDropdown, stableProjectRef]);
  const handleMergeMode = useCallback(() => setMergeMode(true), []);
  const handleSelectBranch = useCallback(
    (ref: GitManagerRefEntry, isRemote: boolean) => {
      setOpenDropdown(stableProjectRef, null);
      onSelectBranch(ref, isRemote);
    },
    [onSelectBranch, setOpenDropdown, stableProjectRef],
  );
  const handleSwitchWorktree = useCallback(
    (path: string) => {
      setOpenDropdown(stableProjectRef, null);
      onSwitchWorktree(path);
    },
    [onSwitchWorktree, setOpenDropdown, stableProjectRef],
  );
  const handleMergeInto = useCallback(
    (ref: GitManagerRefEntry) => {
      setMergeMode(false);
      setOpenDropdown(stableProjectRef, null);
      onMergeInto(ref);
    },
    [onMergeInto, setOpenDropdown, stableProjectRef],
  );
  const handleRenameBranch = useCallback(
    (ref: GitManagerRefEntry) => {
      setOpenDropdown(stableProjectRef, null);
      if (onRenameBranch) onRenameBranch(ref);
      else onSelectBranch(ref, false);
    },
    [onRenameBranch, onSelectBranch, setOpenDropdown, stableProjectRef],
  );
  const handleDeleteBranch = useCallback(
    (ref: GitManagerRefEntry) => {
      setOpenDropdown(stableProjectRef, null);
      if (onDeleteBranch) onDeleteBranch(ref);
      else onSelectBranch(ref, false);
    },
    [onDeleteBranch, onSelectBranch, setOpenDropdown, stableProjectRef],
  );
  const renderItem = useCallback(
    ({ item }: { item: BranchListItem; index: number }) =>
      item.kind === "header" ? (
        <div className="flex h-[30px] items-end px-2 pb-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">
          {item.label}
        </div>
      ) : (
        <GitManagerBranchRow
          branchDisabledReason={branchDisabledReason}
          mergeMode={mergeMode}
          mergeDisabledReason={mergeDisabledReason}
          refEntry={item.ref}
          isRemote={item.isRemote}
          selectedWorktreeCwd={selectedWorktreeCwd}
          onDeleteBranch={handleDeleteBranch}
          onMergeInto={handleMergeInto}
          onRenameBranch={handleRenameBranch}
          onSelectBranch={handleSelectBranch}
          onSwitchWorktree={handleSwitchWorktree}
        />
      ),
    [
      mergeMode,
      branchDisabledReason,
      handleDeleteBranch,
      handleMergeInto,
      handleRenameBranch,
      handleSelectBranch,
      handleSwitchWorktree,
      mergeDisabledReason,
      selectedWorktreeCwd,
    ],
  );

  return (
    <Popover
      open={triggerDisabledReason === null && openDropdown === "branch"}
      onOpenChange={handleOpenChange}
    >
      <PopoverTrigger
        aria-describedby={
          triggerDisabledReason === null ? undefined : "git-manager-branch-trigger-reason"
        }
        aria-label="Choose branch"
        className="inline-flex min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm outline-none hover:bg-accent focus-visible:ring-2 disabled:opacity-60"
        disabled={triggerDisabledReason !== null}
        title={triggerDisabledReason ?? undefined}
      >
        <GitBranchIcon aria-hidden="true" className="size-4 shrink-0" />
        <span className="min-w-0 flex-1 truncate">{currentBranchName ?? noBranchLabel}</span>
        <ChevronDownIcon aria-hidden="true" className="size-3.5 shrink-0 opacity-60" />
      </PopoverTrigger>
      {triggerDisabledReason === null ? null : (
        <span className="sr-only" id="git-manager-branch-trigger-reason">
          {triggerDisabledReason}
        </span>
      )}
      <PopoverPopup align="start" className="w-[32rem] max-w-[calc(100vw-2rem)] p-0" sideOffset={2}>
        <div className="border-b border-border p-2">
          <label className="relative block">
            <SearchIcon
              aria-hidden="true"
              className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground"
            />
            <span className="sr-only">Filter branches</span>
            <Input
              aria-label="Filter branches"
              className="[&_input]:pl-7"
              placeholder="Filter branches…"
              size="sm"
              type="search"
              value={filterText}
              onChange={handleFilterChange}
            />
          </label>
        </div>
        <div aria-label="Branches">
          {listItems.length === 0 ? (
            <p className="p-4 text-center text-xs text-muted-foreground">No branches found.</p>
          ) : (
            <LegendList<BranchListItem>
              data={listItems}
              drawDistance={BRANCH_ROW_HEIGHT * 12}
              estimatedItemSize={BRANCH_ROW_HEIGHT}
              getItemType={branchListItemType}
              keyExtractor={branchListItemKey}
              renderItem={renderItem}
              style={BRANCH_LIST_STYLE}
            />
          )}
        </div>
        <div className="flex items-center gap-2 border-t border-border p-2">
          <Button
            aria-describedby={
              branchDisabledReason === null ? undefined : "git-manager-create-branch-reason"
            }
            className="min-w-0 flex-1 justify-start"
            disabled={branchDisabledReason !== null}
            size="sm"
            title={branchDisabledReason ?? undefined}
            variant="ghost"
            onClick={handleCreateBranch}
          >
            <PlusIcon aria-hidden="true" />
            <span className="min-w-0 flex-1 truncate">New branch</span>
          </Button>
          <Button
            aria-describedby={
              mergeDisabledReason === null && currentBranchName !== null
                ? undefined
                : "git-manager-branch-merge-reason"
            }
            className={cn("min-w-0 flex-1 justify-start", mergeMode && "bg-accent")}
            disabled={mergeDisabledReason !== null || currentBranchName === null}
            size="sm"
            title={
              mergeDisabledReason ??
              (currentBranchName === null ? "Check out a branch before merging." : undefined)
            }
            variant="ghost"
            onClick={handleMergeMode}
          >
            <GitMergeIcon aria-hidden="true" />
            <span className="min-w-0 flex-1 truncate">
              {mergeMode
                ? "Select a branch above"
                : `Choose a branch to merge into ${currentBranchName ?? "HEAD"}`}
            </span>
          </Button>
          {branchDisabledReason === null ? null : (
            <span className="sr-only" id="git-manager-create-branch-reason">
              {branchDisabledReason}
            </span>
          )}
          {mergeDisabledReason === null && currentBranchName !== null ? null : (
            <span className="sr-only" id="git-manager-branch-merge-reason">
              {mergeDisabledReason ?? "Check out a branch before merging."}
            </span>
          )}
        </div>
      </PopoverPopup>
    </Popover>
  );
});
