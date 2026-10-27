import type {
  GitManagerBlockedReason,
  GitManagerConflictState,
  GitManagerInProgressOperation,
  GitManagerMergePreview,
  GitManagerOperationEvent,
  GitManagerRefEntry,
} from "@coddy/contracts";

import { isConflictResolved } from "./GitManagerConflictList.logic";

export type GitManagerMultiCommitStep =
  | "choose-branch"
  | "warn-force-push"
  | "show-progress"
  | "show-conflicts"
  | "hide-conflicts"
  | "confirm-abort"
  | "create-branch";

export type GitManagerMultiCommitKind = "merge" | "rebase" | "cherry-pick" | "squash" | "reorder";

export interface GitManagerMultiCommitState {
  readonly step: GitManagerMultiCommitStep | null;
  readonly kind: GitManagerMultiCommitKind;
  readonly selectedShas: ReadonlyArray<string>;
  readonly selectedBranch: string | null;
  readonly conflicts: ReadonlyArray<GitManagerConflictState>;
  readonly continueBlocked: GitManagerBlockedReason | null;
  readonly originalBranchTip: string | null;
  readonly operationEvent: GitManagerOperationEvent | null;
  readonly operationStartedExternally: boolean;
  readonly abortRequested: boolean;
  readonly inProgressOperation?: GitManagerInProgressOperation | null;
  readonly refs?: ReadonlyArray<GitManagerRefEntry>;
  readonly recentNames?: ReadonlyArray<string>;
  readonly mergePreview?: GitManagerMergePreview | null;
  readonly commitsArePushed?: boolean;
}

export type GitManagerMultiCommitEvent =
  | {
      readonly _tag: "branch-chosen";
      readonly branch: string;
      readonly commitsArePushed: boolean;
    }
  | (Extract<GitManagerOperationEvent, { readonly _tag: "failed" }> & {
      readonly conflicts?: ReadonlyArray<GitManagerConflictState>;
    })
  | Extract<GitManagerOperationEvent, { readonly _tag: "output" }>
  | { readonly _tag: "continue-requested" }
  | { readonly _tag: "dismiss-conflicts" }
  | { readonly _tag: "view-conflicts" }
  | { readonly _tag: "abort-requested" }
  | { readonly _tag: "abort-confirmed" }
  | { readonly _tag: "force-push-confirmed" }
  | {
      readonly _tag: "resolve-conflict-requested";
      readonly path: string;
      readonly side: "ours" | "theirs";
    }
  | { readonly _tag: "undo-conflict-resolution-requested"; readonly path: string }
  | { readonly _tag: "cancelled" }
  | (Extract<GitManagerOperationEvent, { readonly _tag: "started" }> & {
      readonly originalBranchTip?: string | null;
    })
  | Extract<GitManagerOperationEvent, { readonly _tag: "finished" }>;

export interface GitManagerMultiCommitConflictPresentation {
  readonly dialogOpen: boolean;
  readonly bannerVisible: boolean;
  readonly bannerDismissable: false;
}

export function multiCommitConflictPresentation(
  state: GitManagerMultiCommitState,
): GitManagerMultiCommitConflictPresentation {
  return {
    dialogOpen: state.step === "show-conflicts",
    bannerVisible: state.step === "show-conflicts" || state.step === "hide-conflicts",
    bannerDismissable: false,
  };
}

export function canContinueMultiCommitOperation(state: GitManagerMultiCommitState): boolean {
  return (
    state.step === "show-conflicts" &&
    state.continueBlocked === null &&
    state.conflicts.every(isConflictResolved)
  );
}

function multiCommitKindFromOperation(operation: string): GitManagerMultiCommitKind | null {
  switch (operation) {
    case "merge":
    case "rebase":
    case "cherry-pick":
    case "squash":
    case "reorder":
      return operation;
    case "squash-merge":
      return "squash";
    default:
      return null;
  }
}

export function advanceMultiCommitOperation(
  state: GitManagerMultiCommitState,
  event: GitManagerMultiCommitEvent,
): GitManagerMultiCommitState {
  switch (event._tag) {
    case "branch-chosen":
      if (state.step !== "choose-branch") return state;
      return {
        ...state,
        step: event.commitsArePushed ? "warn-force-push" : "show-progress",
        selectedBranch: event.branch,
      };
    case "failed":
      if (event.code !== "conflicts" && event.code !== "conflicts-encountered") {
        return { ...state, step: null, operationEvent: event };
      }
      return {
        ...state,
        step: "show-conflicts",
        conflicts: event.conflicts ?? state.conflicts,
        operationEvent: event,
      };
    case "output":
      return { ...state, operationEvent: event };
    case "continue-requested":
      return canContinueMultiCommitOperation(state) ? { ...state, step: "show-progress" } : state;
    case "dismiss-conflicts":
      return state.step === "show-conflicts" ? { ...state, step: "hide-conflicts" } : state;
    case "view-conflicts":
      return state.step === "hide-conflicts" ? { ...state, step: "show-conflicts" } : state;
    case "abort-requested":
      return state.step === null ? state : { ...state, step: "confirm-abort" };
    case "abort-confirmed":
      return state.step === "confirm-abort"
        ? { ...state, step: "show-progress", abortRequested: true }
        : state;
    case "force-push-confirmed":
      return state.step === "warn-force-push" ? { ...state, step: "show-progress" } : state;
    case "resolve-conflict-requested":
    case "undo-conflict-resolution-requested":
      return state;
    case "cancelled":
      return { ...state, step: null, abortRequested: false };
    case "started":
      return {
        ...state,
        step: "show-progress",
        kind: multiCommitKindFromOperation(event.operation) ?? state.kind,
        originalBranchTip: state.originalBranchTip ?? event.originalBranchTip ?? null,
        operationEvent: event,
        operationStartedExternally: state.step === null || state.operationStartedExternally,
      };
    case "finished":
      return {
        ...state,
        step: null,
        operationEvent: event,
        abortRequested: false,
      };
  }
}
