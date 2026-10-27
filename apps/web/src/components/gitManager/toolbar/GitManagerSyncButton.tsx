import type { GitManagerBlockedReason } from "@coddy/contracts";
import {
  CloudDownloadIcon,
  CloudUploadIcon,
  GitPullRequestArrowIcon,
  LoaderCircleIcon,
} from "lucide-react";
import { memo, useCallback, useState } from "react";

import { Button } from "~/components/ui/button";
import { Checkbox } from "~/components/ui/checkbox";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPopup,
  DialogTitle,
} from "~/components/ui/dialog";

import type { SyncState, SyncStateKind } from "./syncButton.logic";

export type SyncOperationKind = Exclude<
  SyncStateKind,
  "running" | "no-remote" | "detached" | "unavailable"
>;

/** Options confirmed in the push dialog; fetch and pull carry the defaults. */
export interface SyncOperationOptions {
  readonly pushTags: boolean;
}

export const DEFAULT_SYNC_OPERATION_OPTIONS: SyncOperationOptions = Object.freeze({
  pushTags: false,
});

type PushOperationKind = Extract<SyncOperationKind, "push" | "publish-branch" | "force-push">;

function isPushOperation(kind: SyncOperationKind): kind is PushOperationKind {
  return kind === "push" || kind === "publish-branch" || kind === "force-push";
}

function pushDialogCopy(
  kind: PushOperationKind,
  branch: string,
  remote: string,
): { readonly title: string; readonly description: string; readonly confirm: string } {
  switch (kind) {
    case "push":
      return {
        title: `Push ${branch} to ${remote}?`,
        description: `Uploads the local commits on ${branch} to ${remote}/${branch}.`,
        confirm: "Push",
      };
    case "publish-branch":
      return {
        title: `Publish ${branch} to ${remote}?`,
        description: `Creates ${remote}/${branch} and sets it as the upstream of ${branch}.`,
        confirm: "Publish branch",
      };
    case "force-push":
      return {
        title: `Force Push ${branch}?`,
        description: `This rewrites ${remote}/${branch} using --force-with-lease. Remote commits that are not present locally will be replaced, and the push stops if the remote changed since the last fetch.`,
        confirm: "Force push with lease",
      };
  }
}

export interface GitManagerSyncButtonProps {
  readonly state: SyncState;
  readonly currentBranchName: string | null;
  readonly remote: string;
  readonly blockedReason: GitManagerBlockedReason | null;
  readonly disabledReason: string | null;
  readonly onOperation: (kind: SyncOperationKind, options: SyncOperationOptions) => void;
}

function SyncIcon({ kind }: { readonly kind: SyncStateKind }) {
  if (kind === "running") {
    return <LoaderCircleIcon aria-hidden="true" className="animate-spin" />;
  }
  if (kind === "push" || kind === "publish-branch" || kind === "force-push") {
    return <CloudUploadIcon aria-hidden="true" />;
  }
  if (kind === "pull") {
    return <GitPullRequestArrowIcon aria-hidden="true" />;
  }
  return <CloudDownloadIcon aria-hidden="true" />;
}

export const GitManagerSyncButton = memo(function GitManagerSyncButton({
  state,
  currentBranchName,
  remote,
  blockedReason,
  disabledReason: capabilityDisabledReason,
  onOperation,
}: GitManagerSyncButtonProps) {
  const [pendingPush, setPendingPush] = useState<PushOperationKind | null>(null);
  const [pushTags, setPushTags] = useState(false);
  const disabledReason = capabilityDisabledReason ?? blockedReason?.message ?? state.disabledReason;
  const disabled = disabledReason !== null;
  const descriptionId = disabled ? "git-manager-sync-disabled-reason" : undefined;
  const activate = useCallback(() => {
    if (disabled) return;
    if (
      state.kind === "running" ||
      state.kind === "no-remote" ||
      state.kind === "detached" ||
      state.kind === "unavailable"
    ) {
      return;
    }
    if (isPushOperation(state.kind)) {
      // Every push kind confirms in one dialog so the tags choice is always
      // offered; force push keeps its destructive wording there.
      setPendingPush(state.kind);
      return;
    }
    onOperation(state.kind, DEFAULT_SYNC_OPERATION_OPTIONS);
  }, [disabled, onOperation, state.kind]);
  const closePushDialog = useCallback((open: boolean) => {
    if (!open) setPendingPush(null);
  }, []);
  const confirm = useCallback(() => {
    if (pendingPush === null) return;
    setPendingPush(null);
    onOperation(pendingPush, { pushTags });
  }, [onOperation, pendingPush, pushTags]);
  const pushCopy =
    pendingPush === null
      ? null
      : pushDialogCopy(pendingPush, currentBranchName ?? "the current branch", remote);

  return (
    <>
      <button
        aria-describedby={descriptionId}
        className="inline-flex min-w-0 flex-1 items-center justify-center gap-2 rounded-md px-2 py-1.5 text-sm outline-none hover:bg-accent focus-visible:ring-2 disabled:opacity-60"
        disabled={disabled}
        title={disabledReason ?? undefined}
        type="button"
        onClick={activate}
      >
        <SyncIcon kind={state.kind} />
        <span className="truncate">{state.label}</span>
        {state.ahead > 0 ? (
          <span
            aria-label={`${state.ahead} ahead`}
            className="rounded bg-muted px-1 font-mono text-xs"
          >
            ↑{state.ahead}
          </span>
        ) : null}
        {state.behind > 0 ? (
          <span
            aria-label={`${state.behind} behind`}
            className="rounded bg-muted px-1 font-mono text-xs"
          >
            ↓{state.behind}
          </span>
        ) : null}
      </button>
      {disabledReason === null ? null : (
        <span className="sr-only" id={descriptionId}>
          {disabledReason}
        </span>
      )}
      <Dialog open={pendingPush !== null} onOpenChange={closePushDialog}>
        <DialogPopup>
          <DialogHeader>
            <DialogTitle>{pushCopy?.title}</DialogTitle>
            <DialogDescription>{pushCopy?.description}</DialogDescription>
          </DialogHeader>
          <div className="px-6 pb-4">
            <label className="flex items-start gap-2 text-xs">
              <Checkbox
                aria-label="Also push tags"
                checked={pushTags}
                className="mt-0.5"
                onCheckedChange={(checked) => setPushTags(checked === true)}
              />
              <span>
                <span className="font-medium">Also push tags</span>
                <span className="block text-muted-foreground">
                  Every local tag goes with the branch in one atomic push. A tag the remote rejects
                  cancels the whole push.
                </span>
              </span>
            </label>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setPendingPush(null)}>
              Cancel
            </Button>
            <Button
              variant={pendingPush === "force-push" ? "destructive" : "default"}
              onClick={confirm}
            >
              {pushCopy?.confirm}
            </Button>
          </DialogFooter>
        </DialogPopup>
      </Dialog>
    </>
  );
});
