import { MutationsDisabledContext } from "../ui/mutationAvailability";
import type {
  EnvironmentId,
  PullRequestsActionRequest,
  PullRequestsActionResult,
} from "@bibcode/contracts";
import { squashAtomCommandFailure } from "@bibcode/client-runtime/state/runtime";
import {
  createContext,
  createElement,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useAtomCommand } from "../../state/use-atom-command";
import { pullRequestsEnvironment } from "../../state/pullRequests";
import { toastManager } from "../ui/toast";

type WithoutScope<T> = T extends unknown ? Omit<T, "cwd" | "number"> : never;
export type PullRequestsAction = WithoutScope<PullRequestsActionRequest>;
export type PullRequestsActionRunner = (
  action: PullRequestsAction,
  options?: { waitForPending: true },
) => Promise<PullRequestsActionResult>;
export interface PullRequestsActions {
  requestKind: string;
  run: PullRequestsActionRunner;
  pending: boolean;
  error: string | null;
}
export interface PullRequestsRefreshHandles {
  get: () => void;
  timeline: () => void;
  files: () => void;
  commits: () => void;
  checks: () => void;
  navigate: (number: number | null) => void;
  /** Called on delete even after the view unmounted (the panel closes the deleted tab). */
  discardDeleted?: (() => void) | undefined;
}
export type PullRequestsScope = { environmentId: EnvironmentId; cwd: string };
type Scope = PullRequestsScope;
// Successful writes refresh get plus these affected tab atoms. Navigation receipts replace get.
const REFRESH_AFTER_ACTION: Partial<
  Record<
    PullRequestsAction["action"],
    readonly Exclude<keyof PullRequestsRefreshHandles, "navigate" | "discardDeleted">[]
  >
> = {
  comment: ["timeline"],
  editComment: ["timeline"],
  deleteComment: ["timeline"],
  minimizeComment: ["timeline"],
  react: ["timeline"],
  replyThread: ["timeline"],
  resolveThread: ["timeline"],
  submitReview: ["timeline", "files"],
  revokeApproval: ["timeline"],
  removeOwnChangeRequest: ["timeline"],
  dismissReview: ["timeline"],
  rerequestReview: ["timeline"],
  applySuggestions: ["timeline", "files", "commits"],
  editPullRequest: ["timeline"],
  updateBranch: ["commits", "checks", "files"],
  merge: ["timeline", "commits"],
  setDraft: ["timeline"],
  close: ["timeline"],
  reopen: ["timeline"],
};
export function pullRequestsActionError(cause: unknown, requestKind: string): string {
  if (cause !== null && typeof cause === "object") {
    if ("code" in cause && cause.code === "stale_head")
      return `This ${requestKind} changed; reload and try again`;
    if ("message" in cause && typeof cause.message === "string") return cause.message;
  }
  return "The action failed. Refresh and try again.";
}
const RefreshContext = createContext<PullRequestsRefreshHandles | null>(null);
export const PullRequestsActionsContext = createContext<PullRequestsActions | null>(null);
export function usePullRequestsActions(): PullRequestsActions {
  const actions = useContext(PullRequestsActionsContext);
  if (!actions) throw new Error("Pull Requests actions require the detail view provider");
  return actions;
}
export function useRunPullRequestsAction(
  scope: Scope,
  number: number,
  requestKind: string,
): PullRequestsActions {
  const refresh = useContext(RefreshContext);
  const disabledReason = useContext(MutationsDisabledContext);
  if (!refresh) throw new Error("Pull Requests actions require the detail query refresh handles");
  const command = useAtomCommand(pullRequestsEnvironment.runAction, {
    reportFailure: false,
    reportDefect: false,
  });
  const busy = useRef(false);
  const inFlight = useRef<Promise<void> | null>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = useCallback<PullRequestsActionRunner>(
    async (action, options) => {
      // Only an explicitly clicked Undo waits for admission. A dispatched write is never retried.
      if (options?.waitForPending) while (inFlight.current) await inFlight.current;
      if (busy.current) throw new Error("Wait for the current action to finish");
      busy.current = true;
      let finish!: () => void;
      inFlight.current = new Promise<void>((resolve) => {
        finish = resolve;
      });
      setPending(true);
      setError(null);
      try {
        if (disabledReason) throw new Error(disabledReason);
        const result = await command({
          environmentId: scope.environmentId,
          input: { ...action, cwd: scope.cwd, number },
        });
        if (result._tag === "Failure") throw squashAtomCommandFailure(result);
        if (result.value.kind === "deleted") {
          toastManager.add({ type: "success", title: "Request deleted" });
          refresh.discardDeleted?.();
          if (mounted.current) refresh.navigate(null);
        } else if (result.value.kind === "pullRequestCreated") {
          const created = result.value;
          toastManager.add({
            type: "success",
            title: "Revert request created",
            ...(!mounted.current
              ? {
                  actionProps: {
                    children: "Open",
                    onClick: () => refresh.navigate(created.number),
                  },
                }
              : {}),
          });
          if (mounted.current) refresh.navigate(created.number);
        } else {
          refresh.get();
          for (const tab of REFRESH_AFTER_ACTION[action.action] ?? []) refresh[tab]();
        }
        if (result.value.kind === "merged")
          toastManager.add({
            type: "success",
            title: result.value.autoMergeEnabled ? "Auto-merge enabled" : "Merged",
          });
        return result.value;
      } catch (cause) {
        const stale =
          cause !== null &&
          typeof cause === "object" &&
          "code" in cause &&
          cause.code === "stale_head";
        const message = pullRequestsActionError(cause, requestKind);
        setError(message);
        toastManager.add({
          type: "error",
          title: message,
          ...(cause !== null &&
          typeof cause === "object" &&
          "hostDetail" in cause &&
          typeof cause.hostDetail === "string"
            ? { description: cause.hostDetail }
            : {}),
          ...(stale
            ? {
                actionProps: {
                  children: "Refresh",
                  onClick: () => {
                    refresh.get();
                    refresh.timeline();
                    refresh.files();
                    refresh.commits();
                    refresh.checks();
                  },
                },
              }
            : {}),
        });
        throw cause;
      } finally {
        busy.current = false;
        inFlight.current = null;
        finish();
        setPending(false);
      }
    },
    [command, disabledReason, number, refresh, requestKind, scope.cwd, scope.environmentId],
  );
  return useMemo(() => ({ run, pending, error, requestKind }), [run, pending, error, requestKind]);
}
function ActionOwner({
  scope,
  number,
  requestKind,
  children,
}: {
  scope: Scope;
  number: number;
  requestKind: string;
  children?: ReactNode;
}) {
  const actions = useRunPullRequestsAction(scope, number, requestKind);
  return createElement(PullRequestsActionsContext, { value: actions }, children);
}
export function PullRequestsActionProvider({
  scope,
  number,
  requestKind,
  refresh,
  children,
  disabledReason = null,
}: {
  scope: Scope;
  number: number;
  requestKind: string;
  refresh: PullRequestsRefreshHandles;
  children: ReactNode;
  disabledReason?: string | null;
}) {
  return createElement(
    RefreshContext,
    { value: refresh },
    createElement(
      MutationsDisabledContext,
      { value: disabledReason },
      createElement(ActionOwner, { scope, number, requestKind }, children),
    ),
  );
}
