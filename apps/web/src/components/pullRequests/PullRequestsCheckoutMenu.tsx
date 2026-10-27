import type { PullRequestsPermission, ScopedProjectRef } from "@coddy/contracts";
import { ChevronDownIcon } from "lucide-react";
import { useId, useMemo } from "react";
import { useEnvironmentQuery } from "../../state/query";
import { worktreeEnvironment } from "../../state/worktrees";
import { Button } from "../ui/button";
import {
  Menu,
  MenuTrigger,
  MenuPopup,
  MenuItem,
  MenuSeparator,
  MenuGroup,
  MenuGroupLabel,
} from "../ui/menu";
import { PermissionButton, constrainPermission } from "../ui/permission-button";
import { checkoutTargets, type CheckoutWorktree } from "./pullRequestsCheckout.logic";
import { usePullRequestsActions, type PullRequestsScope } from "./usePullRequestsAction";
import { useRunPullRequestsCheckout } from "./useRunPullRequestsCheckout";
const EMPTY_WORKTREES: readonly CheckoutWorktree[] = [];
export function PullRequestsCheckoutMenu({
  scope,
  projectRef,
  number,
  headBranch,
  permission,
  live = true,
}: {
  scope: PullRequestsScope;
  projectRef: ScopedProjectRef;
  number: number;
  headBranch: string;
  permission: PullRequestsPermission;
  /**
   * Checking out only ever acts on a live detail, never a snapshot.
   * Defaults to `true` so callers that have not adopted the split keep
   * acting as before.
   */
  live?: boolean;
}) {
  const reasonId = useId();
  const { requestKind } = usePullRequestsActions();
  const catalogAtom = useMemo(
    () =>
      worktreeEnvironment.catalog({
        environmentId: projectRef.environmentId,
        input: { projectId: projectRef.projectId },
      }),
    [projectRef.environmentId, projectRef.projectId],
  );
  const catalog = useEnvironmentQuery(catalogAtom);
  const worktrees = catalog.data?.worktrees ?? EMPTY_WORKTREES;
  const { run, pending, disabledReason } = useRunPullRequestsCheckout({
    scope,
    projectRef,
    number,
    headBranch,
    permission,
    worktrees,
    onSuccess: catalog.revalidate,
  });
  const available = constrainPermission(permission, !live ? "Loading…" : disabledReason);
  const reason = available.allowed ? undefined : (available.reason ?? undefined);
  const targets = checkoutTargets(worktrees, scope.cwd);
  const others = targets.filter((target) => target.kind === "checkout" && target.cwd !== scope.cwd);
  return (
    <div className="inline-flex" role="group" aria-label={`Check out ${requestKind}`}>
      <PermissionButton
        mutation
        permission={available}
        variant="outline"
        size="sm"
        className="rounded-r-none"
        onClick={() => {
          void run({ kind: "checkout", cwd: scope.cwd });
        }}
      >
        {pending ? "Checking out…" : "Checkout"}
      </PermissionButton>
      <Menu>
        <span className="inline-flex" title={reason}>
          <MenuTrigger
            nativeButton
            render={
              <Button variant="outline" size="icon-sm" className="rounded-l-none border-l-0" />
            }
            aria-label="Checkout options"
            disabled={!available.allowed}
            title={reason}
            aria-describedby={reason ? reasonId : undefined}
          >
            <ChevronDownIcon aria-hidden="true" />
          </MenuTrigger>
          {reason ? (
            <span id={reasonId} className="sr-only">
              {reason}
            </span>
          ) : null}
        </span>
        <MenuPopup align="start" className="max-w-[min(32rem,calc(100vw-2rem))]" data-text-surface>
          <MenuItem
            className="w-full text-left"
            aria-label="Current checkout"
            disabled={!available.allowed}
            onClick={() => {
              void run({ kind: "checkout", cwd: scope.cwd });
            }}
          >
            <span className="min-w-0">
              <span className="block">Current checkout</span>
              <span className="block break-all text-xs text-muted-foreground">{scope.cwd}</span>
            </span>
          </MenuItem>
          {others.length ? (
            <MenuGroup>
              <MenuGroupLabel>Another worktree…</MenuGroupLabel>
              {others.map((target) =>
                target.kind === "checkout" ? (
                  <MenuItem
                    key={target.cwd}
                    className="w-full text-left"
                    aria-label={`Checkout in ${target.cwd}`}
                    disabled={!available.allowed}
                    onClick={() => {
                      void run({ kind: "checkout", cwd: target.cwd });
                    }}
                  >
                    <span className="min-w-0">
                      <span className="block">{target.branch ?? "Detached checkout"}</span>
                      <span className="block break-all text-xs text-muted-foreground">
                        {target.cwd}
                      </span>
                    </span>
                  </MenuItem>
                ) : null,
              )}
            </MenuGroup>
          ) : null}
          {catalog.error ? (
            <>
              <p role="status" className="px-2 py-1 text-xs text-muted-foreground">
                {catalog.error}
              </p>
              <MenuItem className="w-full text-left" closeOnClick={false} onClick={catalog.refresh}>
                Retry worktrees
              </MenuItem>
            </>
          ) : catalog.isPending && catalog.data === null ? (
            <p role="status" className="px-2 py-1 text-xs text-muted-foreground">
              Loading worktrees…
            </p>
          ) : null}
          <MenuSeparator />
          <MenuItem
            className="w-full text-left"
            disabled={!available.allowed}
            onClick={() => {
              void run({ kind: "worktree", branchName: null });
            }}
          >
            New worktree…
          </MenuItem>
        </MenuPopup>
      </Menu>
    </div>
  );
}
