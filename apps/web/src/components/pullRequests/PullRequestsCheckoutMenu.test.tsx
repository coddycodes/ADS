// @vitest-environment happy-dom
import { act } from "react";
import * as Cause from "effect/Cause";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type {
  PullRequestsCheckoutResult,
  PullRequestsPermission,
  ScopedProjectRef,
} from "@coddy/contracts";
import { MutationsDisabledContext } from "../ui/mutationAvailability";
import { PullRequestsActionsContext } from "./usePullRequestsAction";
import { projectKey } from "@coddy/client-runtime/state/entities";
import { useGitManagerStore } from "../../gitManagerStore";
const h = vi.hoisted(() => ({
  command: vi.fn(),
  navigate: vi.fn(),
  toast: vi.fn(),
  refresh: vi.fn(),
  revalidate: vi.fn(),
  catalog: vi.fn(),
  error: null as string | null,
  loading: false,
  hasCatalog: true,
  pendingAction: false,
}));
vi.mock("../../state/use-atom-command", () => ({ useAtomCommand: () => h.command }));
vi.mock("../../state/pullRequests", () => ({ pullRequestsEnvironment: { checkout: {} } }));
vi.mock("../../state/worktrees", () => ({ worktreeEnvironment: { catalog: h.catalog } }));
vi.mock("../../state/query", () => ({
  useEnvironmentQuery: () => ({
    data: h.hasCatalog
      ? {
          worktrees: [
            { path: "/main", branch: "main", isBare: false, directoryState: "present" },
            { path: "/selected", branch: "topic", isBare: false, directoryState: "present" },
            { path: "/occupied", branch: "feature", isBare: false, directoryState: "present" },
          ],
        }
      : null,
    refresh: h.refresh,
    revalidate: h.revalidate,
    requiresRetry: false,
    error: h.error,
    isPending: h.loading,
  }),
}));
vi.mock("@tanstack/react-router", () => ({ useNavigate: () => h.navigate }));
vi.mock("../ui/toast", () => ({ toastManager: { add: h.toast } }));
import { PullRequestsCheckoutMenu } from "./PullRequestsCheckoutMenu";
const projectRef = { environmentId: "env", projectId: "project" } as ScopedProjectRef;
const scope = { environmentId: projectRef.environmentId, cwd: "/selected" };
let root: Root, container: HTMLDivElement;
const allowed: PullRequestsPermission = { allowed: true, reason: null };
const receipt: PullRequestsCheckoutResult = {
  kind: "checked_out",
  cwd: "/selected",
  branch: "feature",
};
async function render(
  permission = allowed,
  disabled: string | null = null,
  requestKind = "pull request",
) {
  await act(async () =>
    root.render(
      <PullRequestsActionsContext
        value={{ run: vi.fn(), pending: h.pendingAction, error: null, requestKind }}
      >
        <MutationsDisabledContext value={disabled}>
          <PullRequestsCheckoutMenu
            scope={scope}
            projectRef={projectRef}
            number={7}
            headBranch="feature"
            permission={permission}
          />
        </MutationsDisabledContext>
      </PullRequestsActionsContext>,
    ),
  );
}
function control(label: string): HTMLElement {
  const element = [...document.querySelectorAll<HTMLElement>("button,[role=menuitem]")].find(
    (node) => node.getAttribute("aria-label") === label || node.textContent?.trim() === label,
  );
  if (!element) throw new Error(`Missing ${label}`);
  return element;
}
async function click(label: string) {
  await act(async () => control(label).click());
}
async function settle(value: PullRequestsCheckoutResult) {
  h.command.mockResolvedValue({ _tag: "Success", value });
}
beforeEach(async () => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.clearAllMocks();
  h.error = null;
  h.loading = false;
  h.hasCatalog = true;
  h.pendingAction = false;
  h.command.mockResolvedValue({ _tag: "Success", value: receipt });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await render();
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});
describe("PullRequestsCheckoutMenu", () => {
  it("uses GitLab vocabulary for the checkout group and stale-head error toast", async () => {
    await render(allowed, null, "merge request");
    expect(container.querySelector('[role="group"]')?.getAttribute("aria-label")).toBe(
      "Check out merge request",
    );
    h.command.mockResolvedValue({
      _tag: "Failure",
      cause: Cause.fail({ code: "stale_head", message: "Head moved" }),
    });
    await click("Checkout");
    expect(h.toast).toHaveBeenCalledWith(
      expect.objectContaining({ title: "This merge request changed; reload and try again" }),
    );
  });
  it("shows worktree loading only before the live catalog supplies data", async () => {
    h.loading = true;
    h.hasCatalog = false;
    await render();
    await click("Checkout options");
    expect(document.querySelector('[role="menu"]')?.textContent).toContain("Loading worktrees…");
    h.hasCatalog = true;
    await render();
    expect(document.querySelector('[role="menu"]')?.textContent).not.toContain(
      "Loading worktrees…",
    );
    expect(control("Checkout in /occupied")).toBeDefined();
  });

  it("checks out in the selected worktree and opens Git Manager only on explicit receipt action", async () => {
    await click("Checkout");
    expect(h.command).toHaveBeenCalledWith({
      environmentId: "env",
      input: { cwd: "/selected", number: 7, target: { kind: "checkout", cwd: "/selected" } },
    });
    // A finished checkout re-reads the catalog automatically; Retry stays explicit.
    expect(h.revalidate).toHaveBeenCalledOnce();
    expect(h.refresh).not.toHaveBeenCalled();
    expect(h.navigate).not.toHaveBeenCalled();
    const toast = h.toast.mock.calls.at(-1)![0];
    expect(toast.title).toBe("Checked out feature in /selected");
    expect(toast.actionProps.children).toBe("Open Git Manager there");
    await act(async () => toast.actionProps.onClick());
    expect(
      useGitManagerStore.getState().byProjectKey[projectKey(projectRef)]?.selectedWorktreeCwd,
    ).toBe("/selected");
    expect(h.navigate).toHaveBeenCalledWith({
      to: "/project/$environmentId/$projectId/git",
      params: projectRef,
    });
  });
  it("offers catalog targets with standard full-width menu rows and native trigger", async () => {
    await click("Checkout options");
    expect(h.catalog).toHaveBeenCalledWith({
      environmentId: "env",
      input: { projectId: "project" },
    });
    expect(document.querySelector('[role="menu"]')?.textContent).toContain("Another worktree…");
    const other = control("Checkout in /occupied");
    expect(other.classList.contains("w-full")).toBe(true);
    expect(other.classList.contains("justify-center")).toBe(false);
    expect(other.dataset.slot).toBe("menu-item");
    await click("Checkout in /occupied");
    expect(h.command.mock.calls[0]![0].input.target).toEqual({
      kind: "checkout",
      cwd: "/occupied",
    });
  });
  it("creates a worktree directly when the existing dialog cannot accept a preset", async () => {
    await settle({ kind: "worktree_created", cwd: "/new", branch: "feature", worktreeId: "id" });
    await click("Checkout options");
    await click("New worktree…");
    expect(h.command.mock.calls[0]![0].input.target).toEqual({
      kind: "worktree",
      branchName: null,
    });
    expect(h.toast.mock.calls.at(-1)![0].title).toBe("Created worktree /new on feature");
  });
  it("renders a blocked reason verbatim and explicitly retries at the occupied worktree", async () => {
    await settle({
      kind: "blocked",
      reason: {
        operation: "checkout",
        code: "worktree-checked-out",
        message: "Checkout is blocked: held at /occupied.",
      },
    });
    await click("Checkout");
    const toast = h.toast.mock.calls.at(-1)![0];
    expect(toast.title).toBe("Checkout is blocked: held at /occupied.");
    expect(toast.actionProps.children).toBe("Switch to that worktree");
    expect(h.refresh).not.toHaveBeenCalled();
    expect(h.revalidate).not.toHaveBeenCalled();
    await act(async () => toast.actionProps.onClick());
    expect(h.command.mock.calls[1]![0].input.target).toEqual({
      kind: "checkout",
      cwd: "/occupied",
    });
  });
  it.each(["session", "host", "pending"])(
    "an old redirect toast respects a later %s denial",
    async (kind) => {
      await settle({
        kind: "blocked",
        reason: { operation: "checkout", code: "worktree-checked-out", message: "Held elsewhere" },
      });
      await click("Checkout");
      const retry = h.toast.mock.calls.at(-1)![0].actionProps.onClick;
      h.pendingAction = kind === "pending";
      await render(
        kind === "host" ? { allowed: false, reason: "Checkout unavailable" } : allowed,
        kind === "session" ? "Read-only connection" : null,
      );
      await act(async () => retry());
      expect(h.command).toHaveBeenCalledOnce();
    },
  );
  it("does not invent a redirect for dirty state", async () => {
    await settle({
      kind: "blocked",
      reason: {
        operation: "checkout",
        code: "dirty-working-tree",
        message: "Commit your changes.",
      },
    });
    await click("Checkout");
    expect(h.toast.mock.calls.at(-1)![0]).toMatchObject({ title: "Commit your changes." });
    expect(h.toast.mock.calls.at(-1)![0].actionProps).toBeUndefined();
  });
  it("blocks duplicate clicks while pending and shows progress", async () => {
    let finish!: (value: unknown) => void;
    h.command.mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    await click("Checkout");
    expect(container.textContent).toContain("Checking out…");
    expect(control("Checkout options").hasAttribute("disabled")).toBe(true);
    await act(async () => finish({ _tag: "Success", value: receipt }));
    expect(h.command).toHaveBeenCalledOnce();
  });
  it.each(["host", "session", "pending"])("explains a %s denial on both halves", async (kind) => {
    h.pendingAction = kind === "pending";
    await render(
      kind === "host" ? { allowed: false, reason: "Host checkout unavailable" } : allowed,
      kind === "session" ? "Read-only connection" : null,
    );
    for (const label of ["Checkout", "Checkout options"]) {
      expect(control(label).hasAttribute("disabled")).toBe(true);
      expect(control(label).getAttribute("aria-describedby")).toBeTruthy();
    }
    expect(h.command).not.toHaveBeenCalled();
  });
  it.each(["interrupt", "disconnect", "heartbeat", "background", "prewrite"])(
    "clears pending with a neutral %s wait outcome",
    async (kind) => {
      const failure =
        kind === "interrupt"
          ? { _tag: "Failure", cause: Cause.interrupt() }
          : {
              _tag: "Failure",
              cause: Cause.fail(
                kind === "disconnect" || kind === "heartbeat"
                  ? {
                      _tag: "RpcClientError",
                      reason: {
                        _tag: kind === "heartbeat" ? "SocketOpenError" : "SocketCloseError",
                        kind: "Timeout",
                        cause: new Error("ping timeout"),
                        message: "Connection closed",
                      },
                    }
                  : {
                      _tag: "PullRequestsOperationError",
                      code: "timeout",
                      operation: "pullRequests.checkout",
                      message:
                        kind === "prewrite"
                          ? "Checkout stopped before any Git changes; refresh to try again"
                          : "Checkout continues in the background; refresh to see the result",
                      retryable: false,
                      hostDetail: null,
                    },
              ),
            };
      let finish!: (value: unknown) => void;
      h.command.mockReturnValue(
        new Promise((resolve) => {
          finish = resolve;
        }),
      );
      await click("Checkout");
      expect(container.textContent).toContain("Checking out…");
      await act(async () => finish(failure));
      expect(control("Checkout").hasAttribute("disabled")).toBe(false);
      const toast = h.toast.mock.calls.at(-1)![0];
      expect(toast.type).toBe("info");
      expect(toast.title).toBe(
        kind === "background"
          ? "Checkout continues in the background; refresh to see the result"
          : kind === "prewrite"
            ? "Checkout stopped before any Git changes; refresh to try again"
            : "Checkout may still be running; refresh to see the result",
      );
      expect(h.command).toHaveBeenCalledOnce();
      expect(h.navigate).not.toHaveBeenCalled();
    },
  );
  it("reports a failure without retrying and leaves checkout available", async () => {
    h.command.mockRejectedValue(new Error("Fetch failed. Check the connection."));
    await click("Checkout");
    expect(h.toast.mock.calls.at(-1)![0].title).toBe("Fetch failed. Check the connection.");
    expect(control("Checkout").hasAttribute("disabled")).toBe(false);
    expect(h.command).toHaveBeenCalledOnce();
  });
  it("preserves the server's actionable error detail", async () => {
    h.command.mockRejectedValue({
      message: "Checkout failed",
      hostDetail: "The branch has diverged; inspect it in Git Manager.",
    });
    await click("Checkout");
    expect(h.toast.mock.calls.at(-1)![0]).toMatchObject({
      title: "Checkout failed",
      description: "The branch has diverged; inspect it in Git Manager.",
    });
  });
  it("late success retains an explicit open action without refreshing or navigating another view", async () => {
    let finish!: (value: unknown) => void;
    h.command.mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    await click("Checkout");
    await act(async () => root.unmount());
    await act(async () => finish({ _tag: "Success", value: receipt }));
    expect(h.refresh).not.toHaveBeenCalled();
    expect(h.revalidate).not.toHaveBeenCalled();
    expect(h.navigate).not.toHaveBeenCalled();
    expect(h.toast.mock.calls.at(-1)![0].actionProps.children).toBe("Open Git Manager there");
  });
  it("shows catalog failure with Retry while keeping current/new targets available", async () => {
    h.error = "Could not load worktrees";
    await render();
    await click("Checkout options");
    expect(document.body.textContent).toContain(h.error);
    await click("Retry worktrees");
    expect(h.refresh).toHaveBeenCalledOnce();
    expect(control("Current checkout")).toBeDefined();
    expect(control("New worktree…")).toBeDefined();
  });
});
