// @vitest-environment happy-dom
import { PullRequestsOperationError, type EnvironmentId } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import { act, useLayoutEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
const h = vi.hoisted(() => ({ command: vi.fn(), toast: vi.fn(), atom: {} }));
vi.mock("../../state/use-atom-command", () => ({ useAtomCommand: () => h.command }));
vi.mock("../../state/pullRequests", () => ({ pullRequestsEnvironment: { runAction: h.atom } }));
vi.mock("../ui/toast", () => ({ toastManager: { add: h.toast } }));
import {
  PullRequestsActionProvider,
  usePullRequestsActions,
  type PullRequestsActions,
  type PullRequestsAction,
} from "./usePullRequestsAction";
import { PermissionButton } from "../ui/permission-button";
let root: Root;
let container: HTMLDivElement;
let actions: PullRequestsActions;
const refresh = {
  get: vi.fn(),
  timeline: vi.fn(),
  files: vi.fn(),
  commits: vi.fn(),
  checks: vi.fn(),
  navigate: vi.fn(),
};
function Probe() {
  const value = usePullRequestsActions();
  useLayoutEffect(() => {
    actions = value;
  }, [value]);
  return (
    <span>
      {String(value.pending)} {value.error}
    </span>
  );
}
beforeEach(async () => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.clearAllMocks();
  h.command.mockResolvedValue({ _tag: "Success", value: { kind: "done" } });
  container = document.createElement("div");
  root = createRoot(container);
  await act(async () =>
    root.render(
      <PullRequestsActionProvider
        requestKind="pull request"
        scope={{ environmentId: "env" as EnvironmentId, cwd: "Z:\\repo" }}
        number={14}
        refresh={refresh}
      >
        <Probe />
      </PullRequestsActionProvider>,
    ),
  );
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});
describe("useRunPullRequestsAction", () => {
  it("admits an explicitly clicked Undo once after a pending mutation settles", async () => {
    let finish!: (result: unknown) => void;
    h.command.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    let current!: Promise<unknown>;
    let undo!: Promise<unknown>;
    act(() => {
      current = actions.run({ action: "setLabels", add: ["bug"], remove: [] });
    });
    act(() => {
      undo = actions.run(
        { action: "setAssignees", add: [], remove: ["me"] },
        { waitForPending: true },
      );
    });
    // Attach before settling to detect premature admission errors without an unhandled rejection.
    const outcome = undo.then(
      () => "done",
      () => "rejected",
    );
    expect(h.command).toHaveBeenCalledOnce();
    await act(async () => {
      finish({ _tag: "Success", value: { kind: "done" } });
      await current;
      await outcome;
    });
    expect(await outcome).toBe("done");
    expect(h.command).toHaveBeenCalledTimes(2);
    expect(h.command).toHaveBeenLastCalledWith({
      environmentId: "env",
      input: { cwd: "Z:\\repo", number: 14, action: "setAssignees", add: [], remove: ["me"] },
    });
  });
  it.each([
    { kind: "deleted" },
    { kind: "pullRequestCreated", number: 27, url: "https://github.com/team/repo/pull/27" },
  ] as const)("does not redirect another view after late $kind completion", async (value) => {
    let finish!: (result: unknown) => void;
    h.command.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    let current!: Promise<unknown>;
    act(() => {
      current = actions.run({ action: value.kind === "deleted" ? "delete" : "revert" });
    });
    await act(async () => root.unmount());
    await act(async () => {
      finish({ _tag: "Success", value });
      await current;
    });
    expect(refresh.navigate).not.toHaveBeenCalled();
    expect(h.toast).toHaveBeenCalledWith(expect.objectContaining({ type: "success" }));
  });
  it("lets the panel discard a request deleted after its view unmounted", async () => {
    const discardDeleted = vi.fn();
    await act(async () =>
      root.render(
        <PullRequestsActionProvider
          requestKind="pull request"
          scope={{ environmentId: "env" as EnvironmentId, cwd: "Z:\\repo" }}
          number={14}
          refresh={{ ...refresh, discardDeleted }}
        >
          <Probe />
        </PullRequestsActionProvider>,
      ),
    );
    let finish!: (result: unknown) => void;
    h.command.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    let current!: Promise<unknown>;
    act(() => {
      current = actions.run({ action: "delete" });
    });
    await act(async () => root.unmount());
    await act(async () => {
      finish({ _tag: "Success", value: { kind: "deleted" } });
      await current;
    });
    expect(discardDeleted).toHaveBeenCalledOnce();
    expect(refresh.navigate).not.toHaveBeenCalled();
  });
  it("disables controls and refuses dispatch when the environment does not support mutations", async () => {
    await act(async () =>
      root.render(
        <PullRequestsActionProvider
          requestKind="pull request"
          scope={{ environmentId: "env" as EnvironmentId, cwd: "Z:\\repo" }}
          number={14}
          refresh={refresh}
          disabledReason="This environment does not support Pull Requests mutations"
        >
          <Probe />
          <PermissionButton mutation permission={{ allowed: true, reason: null }}>
            Comment
          </PermissionButton>
        </PullRequestsActionProvider>,
      ),
    );
    expect(container.querySelector("button")?.disabled).toBe(true);
    expect(container.querySelector("button")?.title).toBe(
      "This environment does not support Pull Requests mutations",
    );
    await act(async () => {
      await expect(actions.run({ action: "comment", body: "Keep draft" })).rejects.toThrow(
        "This environment does not support Pull Requests mutations",
      );
    });
    expect(h.command).not.toHaveBeenCalled();
  });
  it("runs comment with the scoped command input, then refreshes get and timeline", async () => {
    await act(async () => {
      expect(await actions.run({ action: "comment", body: "draft" })).toEqual({ kind: "done" });
    });
    expect(h.command).toHaveBeenCalledWith({
      environmentId: "env",
      input: { cwd: "Z:\\repo", number: 14, action: "comment", body: "draft" },
    });
    expect(refresh.get).toHaveBeenCalledOnce();
    expect(refresh.timeline).toHaveBeenCalledOnce();
    expect(refresh.files).not.toHaveBeenCalled();
    expect(refresh.commits).not.toHaveBeenCalled();
    expect(h.command.mock.invocationCallOrder[0]).toBeLessThan(
      refresh.get.mock.invocationCallOrder[0]!,
    );
  });
  it.each([
    "editComment",
    "deleteComment",
    "minimizeComment",
    "react",
    "replyThread",
    "resolveThread",
    "revokeApproval",
    "removeOwnChangeRequest",
    "dismissReview",
    "rerequestReview",
    "submitReview",
    "applySuggestions",
  ])("refreshes exactly the binding policy for %s", async (action) => {
    await act(async () => {
      await actions.run({ action } as PullRequestsAction);
    });
    expect(refresh.get).toHaveBeenCalledOnce();
    expect(refresh.timeline).toHaveBeenCalledOnce();
    expect(refresh.files.mock.calls).toHaveLength(
      action === "submitReview" || action === "applySuggestions" ? 1 : 0,
    );
    expect(refresh.commits.mock.calls).toHaveLength(action === "applySuggestions" ? 1 : 0);
  });
  it("toasts the operation message and rethrows the original error so drafts stay", async () => {
    const error = new PullRequestsOperationError({
      operation: "comment",
      code: "forbidden",
      message: "Sign in with write access",
      hostDetail: null,
      retryable: false,
    });
    h.command.mockResolvedValue({ _tag: "Failure", cause: Cause.fail(error) });
    await act(async () => {
      await expect(actions.run({ action: "comment", body: "keep me" })).rejects.toBe(error);
    });
    expect(h.toast).toHaveBeenCalledWith(
      expect.objectContaining({ title: error.message, type: "error" }),
    );
    expect(actions.error).toBe(error.message);
    expect(actions.pending).toBe(false);
    for (const fn of Object.values(refresh)) expect(fn).not.toHaveBeenCalled();
  });
  it("reports stale_head with an explicit Refresh action and never retries the write", async () => {
    h.command.mockResolvedValue({
      _tag: "Failure",
      cause: Cause.fail(
        new PullRequestsOperationError({
          operation: "submitReview",
          code: "stale_head",
          message: "Head moved",
          hostDetail: null,
          retryable: true,
        }),
      ),
    });
    await act(async () => {
      await expect(
        actions.run({ action: "submitReview" } as PullRequestsAction),
      ).rejects.toBeDefined();
    });
    const toast = h.toast.mock.calls[0]![0];
    expect(toast.title).toBe("This pull request changed; reload and try again");
    expect(toast.actionProps.children).toBe("Refresh");
    act(() => toast.actionProps.onClick());
    expect(refresh.get).toHaveBeenCalledOnce();
    expect(refresh.files).toHaveBeenCalledOnce();
    expect(refresh.timeline).toHaveBeenCalledOnce();
    expect(refresh.commits).toHaveBeenCalledOnce();
    expect(refresh.checks).toHaveBeenCalledOnce();
    expect(h.command).toHaveBeenCalledOnce();
  });
  it.each([
    ["editPullRequest", ["get", "timeline"]],
    ["setReviewers", ["get"]],
    ["setAssignees", ["get"]],
    ["setLabels", ["get"]],
    ["setMilestone", ["get"]],
    ["lock", ["get"]],
    ["unlock", ["get"]],
    ["updateBranch", ["get", "commits", "checks", "files"]],
    ["merge", ["get", "timeline", "commits"]],
    ["disableAutoMerge", ["get"]],
    ["setDraft", ["get", "timeline"]],
    ["close", ["get", "timeline"]],
    ["reopen", ["get", "timeline"]],
  ] as const)("refreshes Phase 08 action %s exactly", async (action, expected) => {
    await act(async () => {
      await actions.run({ action } as PullRequestsAction);
    });
    for (const [key, fn] of Object.entries(refresh))
      expect(fn.mock.calls).toHaveLength(expected.includes(key as never) ? 1 : 0);
  });
  it.each([
    ["delete", { kind: "deleted" }, null],
    [
      "revert",
      { kind: "pullRequestCreated", number: 27, url: "https://github.com/team/repo/pull/27" },
      27,
    ],
  ] as const)(
    "navigates after %s without reloading a deleted or previous detail",
    async (action, result, target) => {
      h.command.mockResolvedValue({ _tag: "Success", value: result });
      await act(async () => {
        await actions.run({ action });
      });
      expect(refresh.navigate).toHaveBeenCalledExactlyOnceWith(target);
      expect(refresh.get).not.toHaveBeenCalled();
    },
  );
  it.each([false, true])(
    "reports merged autoMergeEnabled=%s truthfully",
    async (autoMergeEnabled) => {
      h.command.mockResolvedValue({
        _tag: "Success",
        value: { kind: "merged", mergedSha: null, autoMergeEnabled },
      });
      await act(async () => {
        await actions.run({ action: "merge" } as PullRequestsAction);
      });
      expect(h.toast).toHaveBeenCalledWith(
        expect.objectContaining({ title: autoMergeEnabled ? "Auto-merge enabled" : "Merged" }),
      );
      expect(refresh.get).toHaveBeenCalledOnce();
    },
  );
  it("shows structured partial-progress messages verbatim with host detail and no automatic retry", async () => {
    const error = {
      code: "host_error",
      message: "Override applied, but merge failed. Refresh before another attempt.",
      hostDetail: "Branch protection changed",
      retryable: true,
    };
    h.command.mockRejectedValue(error);
    await act(async () => {
      await expect(actions.run({ action: "merge" } as PullRequestsAction)).rejects.toBe(error);
    });
    expect(h.toast).toHaveBeenCalledWith(
      expect.objectContaining({ title: error.message, description: error.hostDetail }),
    );
    expect(actions.error).toBe(error.message);
    expect(h.command).toHaveBeenCalledOnce();
    for (const fn of Object.values(refresh)) expect(fn).not.toHaveBeenCalled();
  });
  it("keeps pending true until the command settles and blocks duplicate dispatch", async () => {
    let resolve!: (value: unknown) => void;
    h.command.mockReturnValue(
      new Promise((r) => {
        resolve = r;
      }),
    );
    let running!: Promise<unknown>;
    act(() => {
      running = actions.run({ action: "comment", body: "one" });
    });
    expect(actions.pending).toBe(true);
    expect(refresh.get).not.toHaveBeenCalled();
    await expect(actions.run({ action: "comment", body: "two" })).rejects.toThrow(
      "Wait for the current action",
    );
    await act(async () => {
      resolve({ _tag: "Success", value: { kind: "done" } });
      await running;
    });
    expect(actions.pending).toBe(false);
    expect(h.command).toHaveBeenCalledOnce();
  });
});
