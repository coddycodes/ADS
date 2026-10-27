import {
  getChangeRequestTerminologyForKind,
  formatChangeRequestNumber,
} from "@coddy/shared/sourceControl";
import type {
  PullRequestsContext,
  PullRequestsDetail,
  PullRequestsPermission,
} from "@coddy/contracts";
import { MoreHorizontalIcon } from "lucide-react";
import { useContext, useId, useState, type ReactNode } from "react";
import { writeTextToClipboard } from "../../../hooks/useCopyToClipboard";
import { Button } from "../../ui/button";
import {
  Menu,
  MenuTrigger,
  MenuPopup,
  MenuItem,
  MenuSub,
  MenuSubTrigger,
  MenuSubPopup,
  MenuSeparator,
} from "../../ui/menu";
import { toastManager } from "../../ui/toast";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
import { PullRequestsExternalLink } from "../shared/PullRequestsMarkdown";
import { MutationsDisabledContext } from "../../ui/mutationAvailability";
import { usePullRequestsActions, type PullRequestsAction } from "../usePullRequestsAction";
import { inverseOf, scheduleUndo } from "./undoToast.logic";
import { PullRequestsConfirmAction } from "./PullRequestsConfirmAction";

type Props = {
  detail: PullRequestsDetail;
  context: Extract<PullRequestsContext, { status: "available" }>;
};
function StateConfirmation({
  detail,
  context,
  action,
  onClose,
}: Props & { action: "delete" | "revert"; onClose: () => void }) {
  const { run, pending } = usePullRequestsActions();
  const noun = getChangeRequestTerminologyForKind(context.provider).singular;
  const number = formatChangeRequestNumber(context.provider, detail.number);
  return (
    <PullRequestsConfirmAction
      title={action === "delete" ? `Delete ${noun} ${number}` : `Revert ${number}`}
      confirmLabel={action === "delete" ? `Delete ${noun}` : `Create revert ${noun}`}
      destructive
      permission={constrainPermission(
        detail.permissions[action],
        pending
          ? "Wait for the current action to finish"
          : action === "revert" && detail.state !== "merged"
            ? "Only merged requests can be reverted"
            : null,
      )}
      onClose={onClose}
      onConfirm={() => run({ action })}
    >
      {action === "delete" ? (
        <p>
          Delete {number}. This cannot be undone on {context.host}.
        </p>
      ) : (
        <p>
          This creates a new {noun} that reverts {number}.
        </p>
      )}
    </PullRequestsConfirmAction>
  );
}
export function PullRequestsRevertButton({
  detail,
  live = detail,
  context,
}: Props & {
  /**
   * Reverting only ever acts on a live detail, never a snapshot. Omitting
   * this falls back to `detail`, so callers that have not adopted the split
   * keep acting as before.
   */
  live?: PullRequestsDetail | null;
}) {
  const { pending } = usePullRequestsActions();
  const [open, setOpen] = useState(false);
  return (
    <>
      <PermissionButton
        mutation
        permission={constrainPermission(
          detail.permissions.revert,
          live === null
            ? "Loading…"
            : detail.state !== "merged"
              ? "Only merged requests can be reverted"
              : pending
                ? "Wait for the current action to finish"
                : null,
        )}
        variant="outline"
        size="sm"
        onClick={() => setOpen(true)}
      >
        Revert
      </PermissionButton>
      {open ? (
        <StateConfirmation
          detail={detail}
          context={context}
          action="revert"
          onClose={() => setOpen(false)}
        />
      ) : null}
    </>
  );
}
function ActionItem({
  permission,
  onClick,
  children,
  destructive = false,
}: {
  permission: PullRequestsPermission;
  onClick: () => void;
  children: ReactNode;
  destructive?: boolean;
}) {
  const reasonId = useId();
  const reason = permission.allowed ? undefined : (permission.reason ?? undefined);
  return (
    <div className="w-full" title={reason}>
      <MenuItem
        className="w-full text-left"
        disabled={!permission.allowed}
        title={reason}
        aria-describedby={reason ? reasonId : undefined}
        variant={destructive ? "destructive" : "default"}
        onClick={onClick}
      >
        {children}
      </MenuItem>
      {reason ? (
        <span id={reasonId} className="sr-only">
          {reason}
        </span>
      ) : null}
    </div>
  );
}
export function PullRequestsSecondaryActions({
  detail,
  live = detail,
  context,
}: Props & {
  /**
   * Every action here only ever acts on a live detail, never a snapshot.
   * Omitting this falls back to `detail`, so callers that have not adopted
   * the split keep acting as before.
   */
  live?: PullRequestsDetail | null;
}) {
  const { run, pending } = usePullRequestsActions();
  const [confirmation, setConfirmation] = useState<"delete" | "revert" | null>(null);
  const [copyStatus, setCopyStatus] = useState<string | null>(null);
  const disabledReason = useContext(MutationsDisabledContext);
  const permissions = detail.permissions;
  const available = (permission: PullRequestsPermission) =>
    constrainPermission(
      permission,
      live === null
        ? "Loading…"
        : (disabledReason ?? (pending ? "Wait for the current action to finish" : null)),
    );
  function apply(action: PullRequestsAction, title: string) {
    const inverse = inverseOf(action, { lockReason: detail.lockReason });
    void run(action)
      .then(() => {
        if (inverse) scheduleUndo(run, inverse, toastManager, title);
      })
      .catch(() => undefined);
  }
  const lock = available(permissions.lock);
  return (
    <>
      <Menu>
        <MenuTrigger nativeButton render={<Button variant="ghost" size="sm" />}>
          <MoreHorizontalIcon aria-hidden="true" />
          More actions
        </MenuTrigger>
        <MenuPopup align="end">
          {detail.state === "open" ? (
            <ActionItem
              permission={available(
                detail.isDraft ? permissions.markReady : permissions.convertToDraft,
              )}
              onClick={() =>
                apply(
                  { action: "setDraft", draft: !detail.isDraft },
                  detail.isDraft ? "Marked ready" : "Converted to draft",
                )
              }
            >
              {detail.isDraft ? "Mark ready" : "Convert to draft"}
            </ActionItem>
          ) : null}
          {detail.locked ? (
            <ActionItem
              permission={available(permissions.unlock)}
              onClick={() => apply({ action: "unlock" }, "Unlocked")}
            >
              Unlock
            </ActionItem>
          ) : context.provider === "github" &&
            context.capabilities.lockReasons.length &&
            lock.allowed ? (
            <MenuSub>
              <MenuSubTrigger
                className="w-full text-left"
                disabled={!lock.allowed}
                title={lock.reason ?? undefined}
              >
                Lock
              </MenuSubTrigger>
              <MenuSubPopup>
                {context.capabilities.lockReasons.map((reason) => (
                  <ActionItem
                    key={reason}
                    permission={lock}
                    onClick={() => apply({ action: "lock", reason }, "Locked")}
                  >
                    {reason}
                  </ActionItem>
                ))}
              </MenuSubPopup>
            </MenuSub>
          ) : (
            <ActionItem
              permission={lock}
              onClick={() => apply({ action: "lock", reason: null }, "Locked")}
            >
              Lock
            </ActionItem>
          )}
          {detail.state === "merged" ? (
            <ActionItem
              permission={available(permissions.revert)}
              onClick={() => setConfirmation("revert")}
            >
              Revert
            </ActionItem>
          ) : (
            <ActionItem
              permission={available(
                detail.state === "closed" ? permissions.reopen : permissions.close,
              )}
              onClick={() =>
                apply(
                  { action: detail.state === "closed" ? "reopen" : "close" },
                  detail.state === "closed" ? "Reopened" : "Closed",
                )
              }
            >
              {detail.state === "closed" ? "Reopen" : "Close"}
            </ActionItem>
          )}
          {context.capabilities.deletePullRequest ? (
            <ActionItem
              permission={available(permissions.delete)}
              destructive
              onClick={() => setConfirmation("delete")}
            >
              Delete
            </ActionItem>
          ) : null}
          <MenuSeparator />
          <MenuItem
            className="w-full text-left"
            onClick={() => {
              void writeTextToClipboard(
                detail.url,
                `${getChangeRequestTerminologyForKind(context.provider).singular} URL`,
              ).then(
                () => setCopyStatus("URL copied"),
                () => setCopyStatus("Could not copy the URL. Use Open in browser."),
              );
            }}
          >
            Copy URL
          </MenuItem>
          <MenuItem
            className="w-full text-left"
            render={<PullRequestsExternalLink href={detail.url} />}
          >
            Open in browser
          </MenuItem>
        </MenuPopup>
      </Menu>
      {copyStatus ? (
        <span role="status" className="text-xs text-muted-foreground">
          {copyStatus}
        </span>
      ) : null}
      {confirmation ? (
        <StateConfirmation
          detail={detail}
          context={context}
          action={confirmation}
          onClose={() => setConfirmation(null)}
        />
      ) : null}
    </>
  );
}
