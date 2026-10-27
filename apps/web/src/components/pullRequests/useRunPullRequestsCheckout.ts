import type {
  PullRequestsCheckoutInput,
  PullRequestsCheckoutResult,
  PullRequestsPermission,
  ScopedProjectRef,
} from "@coddy/contracts";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@coddy/client-runtime/state/runtime";
import { useNavigate } from "@tanstack/react-router";
import { useCallback, useContext, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useAtomCommand } from "../../state/use-atom-command";
import { pullRequestsEnvironment } from "../../state/pullRequests";
import { useGitManagerStore } from "../../gitManagerStore";
import { toastManager } from "../ui/toast";
import { MutationsDisabledContext } from "../ui/mutationAvailability";
import {
  pullRequestsActionError,
  usePullRequestsActions,
  type PullRequestsScope,
} from "./usePullRequestsAction";
import {
  occupiedCheckout,
  resultMessage,
  type CheckoutWorktree,
} from "./pullRequestsCheckout.logic";

const CHECKOUT_BACKGROUND_MESSAGE =
  "Checkout continues in the background; refresh to see the result";
const CHECKOUT_STOPPED_MESSAGE = "Checkout stopped before any Git changes; refresh to try again";
const CHECKOUT_UNKNOWN_MESSAGE = "Checkout may still be running; refresh to see the result";

function checkoutWaitMessage(cause: unknown): string | null {
  if (cause === null || typeof cause !== "object" || !("_tag" in cause)) return null;
  if (
    cause._tag === "PullRequestsOperationError" &&
    "code" in cause &&
    cause.code === "timeout" &&
    "message" in cause &&
    (cause.message === CHECKOUT_BACKGROUND_MESSAGE || cause.message === CHECKOUT_STOPPED_MESSAGE)
  )
    return cause.message;
  if (
    cause._tag === "RpcClientError" &&
    "reason" in cause &&
    cause.reason !== null &&
    typeof cause.reason === "object" &&
    "_tag" in cause.reason &&
    ["SocketCloseError", "SocketReadError", "SocketWriteError", "SocketOpenError"].includes(
      String(cause.reason._tag),
    )
  )
    return CHECKOUT_UNKNOWN_MESSAGE;
  return null;
}

export function useRunPullRequestsCheckout({
  scope,
  projectRef,
  number,
  headBranch,
  permission,
  worktrees,
  onSuccess,
}: {
  scope: PullRequestsScope;
  projectRef: ScopedProjectRef;
  number: number;
  headBranch: string;
  permission: PullRequestsPermission;
  worktrees: readonly CheckoutWorktree[];
  onSuccess: () => void;
}) {
  const command = useAtomCommand(pullRequestsEnvironment.checkout, {
    reportFailure: false,
    reportDefect: false,
  });
  const navigate = useNavigate();
  const disabledReason = useContext(MutationsDisabledContext);
  const actions = usePullRequestsActions();
  const busy = useRef(false);
  const retry = useRef<
    | ((target: PullRequestsCheckoutInput["target"]) => Promise<PullRequestsCheckoutResult | null>)
    | null
  >(null);
  const mounted = useRef(true);
  const [pending, setPending] = useState(false);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const run = useCallback(
    async function checkout(
      target: PullRequestsCheckoutInput["target"],
    ): Promise<PullRequestsCheckoutResult | null> {
      if (
        busy.current ||
        actions.pending ||
        !mounted.current ||
        !permission.allowed ||
        disabledReason
      )
        return null;
      busy.current = true;
      setPending(true);
      try {
        const result = await command({
          environmentId: scope.environmentId,
          input: { cwd: scope.cwd, number, target },
        });
        if (isAtomCommandInterrupted(result)) {
          toastManager.add({ type: "info", title: CHECKOUT_UNKNOWN_MESSAGE });
          return null;
        }
        if (result._tag === "Failure") throw squashAtomCommandFailure(result);
        const receipt = result.value;
        if (receipt.kind === "blocked") {
          const occupied = occupiedCheckout(receipt, worktrees, headBranch);
          toastManager.add({
            type: "warning",
            title: resultMessage(receipt),
            ...(occupied && mounted.current
              ? {
                  actionProps: {
                    children: "Switch to that worktree",
                    onClick: () => {
                      void retry.current?.({ kind: "checkout", cwd: occupied });
                    },
                  },
                }
              : {}),
          });
        } else {
          if (mounted.current) onSuccess();
          toastManager.add({
            type: "success",
            title: resultMessage(receipt),
            actionProps: {
              children: "Open Git Manager there",
              onClick: () => {
                useGitManagerStore.getState().setSelectedWorktree(projectRef, receipt.cwd);
                void navigate({ to: "/project/$environmentId/$projectId/git", params: projectRef });
              },
            },
          });
        }
        return receipt;
      } catch (cause) {
        const waitMessage = checkoutWaitMessage(cause);
        if (waitMessage) {
          toastManager.add({ type: "info", title: waitMessage });
          return null;
        }
        toastManager.add({
          type: "error",
          title: pullRequestsActionError(cause, actions.requestKind),
          ...(cause !== null &&
          typeof cause === "object" &&
          "hostDetail" in cause &&
          typeof cause.hostDetail === "string"
            ? { description: cause.hostDetail }
            : {}),
        });
        return null;
      } finally {
        busy.current = false;
        if (mounted.current) setPending(false);
      }
    },
    [
      actions.pending,
      actions.requestKind,
      command,
      disabledReason,
      headBranch,
      navigate,
      number,
      onSuccess,
      permission.allowed,
      projectRef,
      scope.cwd,
      scope.environmentId,
      worktrees,
    ],
  );
  useLayoutEffect(() => {
    retry.current = run;
  }, [run]);
  return {
    run,
    pending,
    disabledReason:
      disabledReason ??
      (pending || actions.pending ? "Wait for the current action to finish" : null),
  };
}
