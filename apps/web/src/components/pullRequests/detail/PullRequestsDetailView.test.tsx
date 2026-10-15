// @vitest-environment happy-dom
import {
  PullRequestsOperationError,
  type PullRequestsDetail,
  type ScopedProjectRef,
} from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import { act, useReducer } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { usePullRequestsStore, type PullRequestsDetailTab } from "../../../pullRequestsStore";
import { recordPrefetch, resetPrefetchGates } from "../hoverPrefetch.logic";
import { PullRequestsContextRefresh } from "../pullRequestsContextRefresh";
import { context, detail, gitlabContext } from "./testFixtures";
import { click, allowed } from "../review/testHelpers";
const h = vi.hoisted(() => ({
  command: vi.fn(),
  data: {} as Record<string, unknown>,
  errors: {} as Record<string, PullRequestsOperationError>,
  subscribed: new Set<string>(),
  revalidated: [] as string[],
  pending: false,
  complete: true,
  pendingQueries: new Set<string>(),
  toast: vi.fn().mockReturnValue("undo"),
  closeToast: vi.fn(),
  get: vi.fn((args: unknown) => ({ kind: "get", args })),
  getTimeline: vi.fn((args: unknown) => ({ kind: "getTimeline", args })),
  getCommits: vi.fn((args: unknown) => ({ kind: "getCommits", args })),
  getChecks: vi.fn((args: unknown) => ({ kind: "getChecks", args })),
  getFiles: vi.fn((args: unknown) => ({ kind: "getFiles", args })),
  getVocabulary: vi.fn((args: unknown) => ({ kind: "getVocabulary", args })),
  refresh: vi.fn(),
  navigate: vi.fn(),
  contextRefresh: vi.fn(),
}));
vi.mock("../../../state/use-atom-command", () => ({ useAtomCommand: () => h.command }));
vi.mock("../../ui/toast", () => ({ toastManager: { add: h.toast, close: h.closeToast } }));
vi.mock("@effect/atom-react", async (importOriginal) => ({
  ...(await importOriginal<object>()),
  useAtomRefresh: (atom: { kind: string }) => () => h.refresh(atom.kind),
}));
vi.mock("../../../state/pullRequests", () => ({
  pullRequestsEnvironment: {
    get: h.get,
    getTimeline: h.getTimeline,
    getCommits: h.getCommits,
    getChecks: h.getChecks,
    getFiles: h.getFiles,
    getVocabulary: h.getVocabulary,
    readSnapshot: vi.fn(() => ({ kind: "readSnapshot" })),
    subscribe: vi.fn(() => ({ kind: "subscribe" })),
  },
}));
vi.mock("../../../state/worktrees", () => ({
  worktreeEnvironment: { catalog: () => ({ kind: "catalog" }) },
}));
vi.mock("../../../state/query", () => ({
  useEnvironmentQuery: (atom: { kind: string } | null) => {
    const [, publish] = useReducer((value: number) => value + 1, 0);
    if (atom) h.subscribed.add(atom.kind);
    const error = atom ? h.errors[atom.kind] : null;
    // Automatic re-reads (`revalidate`) record through the same refresh.
    const refresh = () => {
      if (!atom) return;
      h.refresh(atom.kind);
      if (h.complete && h.data[atom.kind]) {
        h.data[atom.kind] = { ...(h.data[atom.kind] as object) };
        publish();
      } else if (!h.complete) {
        h.pendingQueries.add(atom.kind);
        publish();
      }
    };
    return {
      data: atom ? (h.data[atom.kind] ?? null) : null,
      error: error?.message ?? null,
      isPending: atom !== null && (h.pending || h.pendingQueries.has(atom.kind)),
      emission: error ? { _tag: "Failure", cause: Cause.fail(error) } : { _tag: "Initial" },
      refresh,
      revalidate: () => {
        if (atom) h.revalidated.push(atom.kind);
        refresh();
      },
      requiresRetry: false,
    };
  },
}));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => h.navigate,
  useLocation: () => ({ hash: "" }),
  Link: ({ children }: { children: React.ReactNode }) => <a>{children}</a>,
}));
import { PullRequestsDetailView } from "./PullRequestsDetailView";
import { PullRequestsRouteNavigation } from "./pullRequestsNavigation";
function setVisibility(state: DocumentVisibilityState) {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => state });
  document.dispatchEvent(new Event("visibilitychange"));
}
let root: Root;
let container: HTMLDivElement;
const projectRef = { environmentId: "env", projectId: "project" } as ScopedProjectRef;
const scope = { environmentId: projectRef.environmentId, cwd: "Z:\\opaque\\repo" };
async function render(
  tab: PullRequestsDetailTab = "conversation",
  number = 14,
  mutationsDisabledReason: string | null = null,
  host = context,
) {
  await act(async () =>
    root.render(
      <PullRequestsContextRefresh value={h.contextRefresh}>
        <PullRequestsRouteNavigation projectRef={projectRef} number={number}>
          <PullRequestsDetailView
            scope={scope}
            projectRef={projectRef}
            context={host}
            number={number}
            tab={tab}
            mutationsDisabledReason={mutationsDisabledReason}
          />
        </PullRequestsRouteNavigation>
      </PullRequestsContextRefresh>,
    ),
  );
}
beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  h.data = {
    get: detail,
    getTimeline: { items: [], truncated: false },
    getCommits: { commits: [] },
    getChecks: { groups: [], summary: "none", pipelineUrl: null },
    getFiles: {
      files: [],
      diffRefs: { baseSha: "base", startSha: "start", headSha: "head" },
      truncated: false,
    },
  };
  h.errors = {};
  h.subscribed.clear();
  h.revalidated.length = 0;
  h.pending = false;
  h.pendingQueries.clear();
  h.complete = true;
  resetPrefetchGates();
  setVisibility("visible");
  vi.clearAllMocks();
  h.command.mockResolvedValue({ _tag: "Success", value: { kind: "done" } });
  usePullRequestsStore.setState({ byProjectKey: {} });
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});
describe("PullRequestsDetailView", () => {
  it("uses the loaded GitLab vocabulary in the action owner's stale-head toast", async () => {
    h.data.get = { ...detail, permissions: { ...detail.permissions, comment: allowed } };
    usePullRequestsStore.getState().setCommentDraft(projectRef, 14, "Keep this draft");
    h.command.mockResolvedValue({
      _tag: "Failure",
      cause: Cause.fail(
        new PullRequestsOperationError({
          operation: "pullRequests.runAction",
          code: "stale_head",
          message: "Head moved",
          hostDetail: null,
          retryable: true,
        }),
      ),
    });
    await render("conversation", 14, null, gitlabContext);
    await click("Comment");
    expect(h.toast).toHaveBeenCalledWith(
      expect.objectContaining({ title: "This merge request changed; reload and try again" }),
    );
    expect(h.command).toHaveBeenCalledOnce();
    expect(usePullRequestsStore.getState().selectDraft(projectRef, 14).comment).toBe(
      "Keep this draft",
    );
  });
  it.each(["conversation", "checks"] as const)(
    "blocks stale label toggles after Undo until refreshed detail arrives on %s",
    async (tab) => {
      h.data.get = {
        ...detail,
        labels: [],
        permissions: { ...detail.permissions, editLabels: allowed },
      };
      h.data.getVocabulary = {
        kind: "labels",
        entries: [{ id: "bug", label: "bug", color: "ff0000", description: null }],
        truncated: false,
      };
      await render(tab);
      await click("Edit Labels");
      const checkbox = () => document.querySelector<HTMLInputElement>('input[aria-label="bug"]')!;
      await act(async () => checkbox().click());
      expect(h.command).toHaveBeenCalledExactlyOnceWith({
        environmentId: "env",
        input: { cwd: scope.cwd, number: 14, action: "setLabels", add: ["bug"], remove: [] },
      });
      // Settle the add's authoritative detail, then delay the refresh after Undo.
      h.data.get = { ...(h.data.get as PullRequestsDetail), labels: detail.labels };
      await render(tab);
      expect(checkbox().checked).toBe(true);
      h.complete = false;
      await act(async () => h.toast.mock.calls[0]![0].actionProps.onClick());
      expect(h.command).toHaveBeenCalledTimes(2);
      expect(h.command).toHaveBeenLastCalledWith({
        environmentId: "env",
        input: { cwd: scope.cwd, number: 14, action: "setLabels", add: [], remove: ["bug"] },
      });
      // The host write has finished, but the checkbox is still the pre-Undo value.
      await act(async () => checkbox().click());
      expect(h.command).toHaveBeenCalledTimes(2);
      expect(checkbox().disabled).toBe(true);
      expect(document.body.textContent).toContain("Refreshing request details…");
      h.pendingQueries.delete("get");
      h.data.get = { ...(h.data.get as PullRequestsDetail), labels: [] };
      await render(tab);
      expect(checkbox().disabled).toBe(false);
      expect(checkbox().checked).toBe(false);
      await act(async () => checkbox().click());
      expect(h.command).toHaveBeenCalledTimes(3);
      expect(h.command).toHaveBeenLastCalledWith({
        environmentId: "env",
        input: { cwd: scope.cwd, number: 14, action: "setLabels", add: ["bug"], remove: [] },
      });
    },
  );

  it("keeps stale picker controls unavailable when the detail refresh after Undo fails", async () => {
    h.data.get = {
      ...detail,
      labels: [],
      permissions: { ...detail.permissions, editLabels: allowed },
    };
    h.data.getVocabulary = {
      kind: "labels",
      entries: [{ id: "bug", label: "bug", color: "ff0000", description: null }],
      truncated: false,
    };
    await render();
    await click("Edit Labels");
    await act(async () =>
      document.querySelector<HTMLInputElement>('input[aria-label="bug"]')!.click(),
    );
    h.data.get = { ...(h.data.get as PullRequestsDetail), labels: detail.labels };
    await render();
    h.complete = false;
    await act(async () => h.toast.mock.calls[0]![0].actionProps.onClick());
    h.pendingQueries.delete("get");
    h.errors.get = new PullRequestsOperationError({
      operation: "pullRequests.get",
      code: "host_unreachable",
      message: "Could not refresh request details. Try again.",
      hostDetail: null,
      retryable: true,
    });
    await render();
    expect(document.querySelector('input[aria-label="bug"]')).toBeNull();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "Could not refresh request details. Try again.",
    );
    h.refresh.mockClear();
    await click("Retry");
    expect(h.refresh).toHaveBeenCalledExactlyOnceWith("get");
    expect(h.command).toHaveBeenCalledTimes(2);
    delete h.errors.get;
    h.pendingQueries.delete("get");
    h.data.get = { ...(h.data.get as PullRequestsDetail), labels: [] };
    h.complete = true;
    await render();
    await click("Edit Labels");
    const checkbox = document.querySelector<HTMLInputElement>('input[aria-label="bug"]')!;
    expect(checkbox.checked).toBe(false);
    await act(async () => checkbox.click());
    expect(h.command).toHaveBeenLastCalledWith({
      environmentId: "env",
      input: { cwd: scope.cwd, number: 14, action: "setLabels", add: ["bug"], remove: [] },
    });
  });

  it("invalidates commits, checks and files after an update without subscribing inactive tabs", async () => {
    h.data.get = {
      ...detail,
      permissions: { ...detail.permissions, updateBranch: { ...allowed, methods: ["merge"] } },
      readiness: { ...detail.readiness, status: "behind" },
    };
    await render();
    h.refresh.mockClear();
    h.revalidated.length = 0;
    await click("Update branch");
    expect(h.refresh.mock.calls.map(([kind]) => kind)).toEqual([
      "get",
      "getCommits",
      "getChecks",
      "getFiles",
    ]);
    // A finished action re-reads automatically; it never clears a cut-off latch.
    expect(h.revalidated).toEqual(["get"]);
    // The stored copy is read for every host; inactive tabs stay unsubscribed.
    expect([...h.subscribed]).toEqual(["get", "getTimeline", "readSnapshot", "catalog"]);
  });
  it.each(["delete", "revert"] as const)(
    "routes the successful %s receipt within the initiating project",
    async (action) => {
      h.data.get = {
        ...detail,
        state: action === "revert" ? "merged" : "open",
        permissions: { ...detail.permissions, [action]: allowed },
      };
      h.command.mockResolvedValue({
        _tag: "Success",
        value:
          action === "delete"
            ? { kind: "deleted" }
            : {
                kind: "pullRequestCreated",
                number: 29,
                url: "https://example.test/merge_requests/29",
              },
      });
      await render("conversation", 14, null, gitlabContext);
      await click("More actions");
      await click(action === "delete" ? "Delete" : "Revert");
      await click(
        action === "delete" ? "Delete merge request" : "Create revert merge request",
        document.querySelector('[role="alertdialog"]')!,
      );
      expect(h.navigate).toHaveBeenCalledWith(
        action === "delete"
          ? { to: "/project/$environmentId/$projectId/pull-requests", params: projectRef }
          : {
              to: "/project/$environmentId/$projectId/pull-requests/$number",
              params: { ...projectRef, number: "29" },
              search: { tab: "conversation" },
              hash: "",
            },
      );
    },
  );
  it("keeps read Refresh and Retry available when the environment disables mutations", async () => {
    await render("conversation", 14, "Read-only environment");
    const refresh = [...container.querySelectorAll("button")].find(
      (b) => b.textContent === "Refresh",
    )!;
    expect(refresh.disabled).toBe(false);
    h.errors.getTimeline = new PullRequestsOperationError({
      operation: "getTimeline",
      code: "host_unreachable",
      message: "Retry the timeline",
      hostDetail: null,
      retryable: true,
    });
    await render("conversation", 14, "Read-only environment");
    const retry = [...container.querySelectorAll("button")].find((b) => b.textContent === "Retry")!;
    expect(retry.disabled).toBe(false);
    h.refresh.mockClear();
    await act(async () => retry.click());
    expect(h.refresh).toHaveBeenCalledWith("getTimeline");
  });
  it("posts through the command and refreshes the owned detail and timeline without remounting queries", async () => {
    h.data.get = {
      ...detail,
      permissions: { ...detail.permissions, comment: { allowed: true, reason: null } },
    };
    usePullRequestsStore.getState().setCommentDraft(projectRef, 14, "Posted draft");
    await render();
    h.refresh.mockClear();
    await act(async () =>
      [...container.querySelectorAll("button")].find((b) => b.textContent === "Comment")!.click(),
    );
    expect(h.command).toHaveBeenCalledWith({
      environmentId: "env",
      input: { cwd: scope.cwd, number: 14, action: "comment", body: "Posted draft" },
    });
    expect(h.refresh.mock.calls.map(([kind]) => kind)).toEqual(["get", "getTimeline"]);
    expect(h.getTimeline).toHaveBeenCalledOnce();
    expect(usePullRequestsStore.getState().selectDraft(projectRef, 14).comment).toBe("");
  });
  it("paints a snapshot's title but leaves every action control inactive until a live detail succeeds", async () => {
    h.data.get = null;
    const permissiveSnapshot: PullRequestsDetail = {
      ...detail,
      permissions: {
        ...detail.permissions,
        merge: { ...detail.permissions.merge, ...allowed },
        editPullRequest: allowed,
        editReviewers: allowed,
        comment: allowed,
        approve: allowed,
      },
    };
    h.data.readSnapshot = {
      list: null,
      detail: { payload: permissiveSnapshot, observedAt: 1 },
      tab: null,
    };
    await render("conversation", 14, null, gitlabContext);
    // The title paints from the snapshot even with no live detail yet.
    expect(container.textContent).toContain(detail.title);
    // The merge box still renders (not hidden), but inactive until live succeeds.
    const merge = container.querySelector('[aria-label="Merge status"]');
    expect(merge).not.toBeNull();
    const mergeButton = [...merge!.querySelectorAll("button")].find(
      (button) => button.textContent === "Merge",
    )!;
    expect(mergeButton.disabled).toBe(true);
    expect(mergeButton.title).toBe("Loading…");
    // The header's base branch picker stays inactive too, not just the merge box.
    const baseBranch = container.querySelector<HTMLButtonElement>(
      '[aria-label="Change base branch"]',
    )!;
    expect(baseBranch.disabled).toBe(true);
    expect(baseBranch.title).toBe("Loading…");
    // The side column's reviewer picker stays inactive as well.
    const reviewerPicker = container.querySelector<HTMLButtonElement>(
      `[aria-label="Edit ${gitlabContext.capabilities.vocabulary.reviewer}"]`,
    )!;
    expect(reviewerPicker.disabled).toBe(true);
    expect(reviewerPicker.title).toBe("Loading…");
    // The main comment box's submit control stays inactive too.
    const commentButton = [...container.querySelectorAll("button")].find(
      (button) => button.textContent === "Comment",
    )!;
    expect(commentButton.disabled).toBe(true);
    expect(commentButton.title).toBe("Loading…");
    // GitLab Approve is on the overview, and a snapshot that would allow it
    // still cannot run until the live detail succeeds.
    const approval = container.querySelector('[aria-label="Approval"]');
    expect(approval?.textContent).toContain("Approve");
    expect(approval?.textContent).toContain("0 of 1 approvals");
    const approveButton = [...approval!.querySelectorAll("button")].find(
      (button) => button.textContent === "Approve",
    )!;
    expect(approveButton.disabled).toBe(true);
    expect(approveButton.title).toBe("Loading…");
  });
  it("shows a hover prefetch answered within 5 s on open without reading it again", async () => {
    const request = { environmentId: scope.environmentId, input: { cwd: scope.cwd, number: 14 } };
    recordPrefetch("get", request, Date.now());
    recordPrefetch("getTimeline", request, Date.now());
    await render("conversation", 14, null, gitlabContext);
    expect(container.querySelector("h1")?.textContent).toContain(detail.title);
    expect(h.refresh).not.toHaveBeenCalledWith("get");
    expect(h.refresh).not.toHaveBeenCalledWith("getTimeline");
  });
  it("re-reads a hover prefetch older than 5 s on open", async () => {
    const request = { environmentId: scope.environmentId, input: { cwd: scope.cwd, number: 14 } };
    recordPrefetch("get", request, Date.now() - 6_000);
    recordPrefetch("getTimeline", request, Date.now() - 6_000);
    await render("conversation", 14, null, gitlabContext);
    expect(h.refresh).toHaveBeenCalledWith("get");
    expect(h.refresh).toHaveBeenCalledWith("getTimeline");
  });
  it("drops the subscription while the document is hidden and subscribes again when shown", async () => {
    setVisibility("hidden");
    await render("conversation", 14, null, gitlabContext);
    expect(h.subscribed.has("subscribe")).toBe(false);
    await act(async () => setVisibility("visible"));
    expect(h.subscribed.has("subscribe")).toBe(true);
  });
  it("offers a direct path back to the request list", async () => {
    await render();
    const back = [...container.querySelectorAll("button")].find(
      (button) => button.textContent === "Back to pull requests",
    );
    expect(back).toBeDefined();
    await act(async () => back!.click());
    expect(h.navigate).toHaveBeenCalledWith({
      to: "/project/$environmentId/$projectId/pull-requests",
      params: projectRef,
    });
  });
  it("refreshes a reopened cached tab and never dispatches reads on focus", async () => {
    await render("checks");
    await render("commits");
    h.refresh.mockClear();
    await render("checks");
    expect(h.refresh.mock.calls.map(([kind]) => kind)).toEqual(["getChecks"]);
    h.refresh.mockClear();
    await act(async () => window.dispatchEvent(new Event("focus")));
    expect(h.refresh).not.toHaveBeenCalled();
  });
  it.each([
    ["conversation", "getTimeline"],
    ["commits", "getCommits"],
    ["checks", "getChecks"],
    ["files", "getFiles"],
  ] as const)(
    "subscribes only to get and the active %s query while exposing inactive refresh handles",
    async (tab, method) => {
      await render(tab);
      expect(h.get).toHaveBeenCalledWith({
        environmentId: "env",
        input: { cwd: scope.cwd, number: 14 },
      });
      for (const name of ["getTimeline", "getCommits", "getChecks", "getFiles"] as const)
        expect(h.subscribed.has(name)).toBe(
          name === method || (tab === "files" && name === "getTimeline"),
        );
      expect(container.querySelector("h1")?.textContent).toContain(detail.title);
      expect(
        container.querySelectorAll('[aria-label="pull request sections"] [role="tab"]'),
      ).toHaveLength(4);
      expect(container.textContent).toContain("Conversation 4");
      expect(usePullRequestsStore.getState().selectViewState(projectRef).lastNumber).toBe(14);
    },
  );
  it("replaces the search tab on navigation and refreshes only detail and the active tab", async () => {
    await render("checks");
    h.refresh.mockClear();
    await act(async () =>
      [...container.querySelectorAll("button")]
        .find((button) => button.textContent === "Refresh")!
        .click(),
    );
    expect(h.refresh.mock.calls.map(([kind]) => kind)).toEqual(["get", "getChecks"]);
    await act(async () =>
      [...container.querySelectorAll('[aria-label="pull request sections"] [role="tab"]')]
        .find((tab) => tab.textContent === "Files changed 2")!
        .dispatchEvent(new MouseEvent("click", { bubbles: true })),
    );
    expect(h.navigate).toHaveBeenCalledWith(
      expect.objectContaining({ search: { tab: "files" }, replace: true }),
    );
  });
  it("withholds cached detail until the explicit-open refresh finishes", async () => {
    h.complete = false;
    await render();
    expect(container.textContent).not.toContain(detail.title);
    expect(container.querySelector('[role="status"]')?.textContent).toContain("#14");
    expect(h.refresh).toHaveBeenCalledWith("get");
    h.data.get = { ...detail } satisfies PullRequestsDetail;
    await render();
    expect(container.textContent).toContain(detail.title);
  });
  it.each(["get", "getTimeline", "getCommits", "getChecks", "getFiles"] as const)(
    "shows a retryable %s auth failure and refreshes context",
    async (method) => {
      h.data[method] = null;
      h.errors[method] = new PullRequestsOperationError({
        operation: method,
        code: "not_authenticated",
        message: "Sign in again.",
        hostDetail: "Run gh auth login",
        retryable: true,
      });
      const tab =
        method === "getCommits"
          ? "commits"
          : method === "getChecks"
            ? "checks"
            : method === "getFiles"
              ? "files"
              : "conversation";
      await render(tab);
      expect(container.querySelector('[role="alert"]')?.textContent).toContain("Sign in again.");
      expect(container.textContent).toContain("Run gh auth login");
      expect(h.contextRefresh).toHaveBeenCalledOnce();
      h.refresh.mockClear();
      await act(async () =>
        [...container.querySelectorAll("button")]
          .find((button) => button.textContent === "Retry")!
          .click(),
      );
      expect(h.refresh).toHaveBeenCalledWith(method);
    },
  );
});
