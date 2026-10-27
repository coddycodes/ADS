// @vitest-environment happy-dom

import {
  createMemoryHistory,
  createRootRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import type { GitManagerPullRequestsResult, SourceControlProviderInfo } from "@coddy/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  result: null as GitManagerPullRequestsResult | null,
  pending: false,
  error: null as string | null,
  listRequests: vi.fn(() => ({ kind: "provider-query" })),
  refreshRequests: vi.fn(),
  createPr: vi.fn(async () => ({ _tag: "Success" })),
  dialogProps: [] as Array<Record<string, unknown>>,
}));

vi.mock("../../../state/gitManager", () => ({
  gitManagerEnvironment: { listPullRequests: h.listRequests },
}));

vi.mock("../../../state/query", () => ({
  useEnvironmentQuery: () => ({
    data: h.result,
    isPending: h.pending,
    error: h.error,
    refresh: h.refreshRequests,
  }),
}));

vi.mock("../../../state/sourceControlActions", () => ({
  useGitStackedAction: () => ({ run: h.createPr, isPending: false, error: null }),
}));

vi.mock("./GitManagerCreatePullRequestDialog", () => ({
  GitManagerCreatePullRequestDialog: (props: Record<string, unknown>) => {
    h.dialogProps.push(props);
    return <div data-testid="create-pr-dialog" role="dialog" />;
  },
}));

import { usePullRequestsStore } from "../../../pullRequestsStore";
import { GitManagerPullRequestPanel } from "./GitManagerPullRequestPanel";

let container: HTMLDivElement;
let root: Root;

/** Renders the pane as the Git Manager does; status has answered unless a test says not. */
async function renderPanel(
  onRefresh = vi.fn(),
  disabledReason: string | null = null,
  provider: SourceControlProviderInfo | null = null,
  statusLoaded = true,
) {
  await act(async () =>
    root.render(
      <GitManagerPullRequestPanel
        disabledReason={disabledReason}
        provider={provider}
        statusLoaded={statusLoaded}
        scope={{ environmentId: "env-a" as never, cwd: "/repo" }}
        onRefresh={onRefresh}
      />,
    ),
  );
  return onRefresh;
}

function button(text: string): HTMLButtonElement {
  const result = [...container.querySelectorAll<HTMLButtonElement>("button")].find((candidate) =>
    candidate.textContent?.includes(text),
  );
  if (!(result instanceof HTMLButtonElement)) throw new Error(`Missing button: ${text}`);
  return result;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  h.result = null;
  h.pending = false;
  h.error = null;
  h.listRequests.mockClear();
  h.refreshRequests.mockClear();
  h.createPr.mockClear();
  h.dialogProps = [];
  vi.useFakeTimers();
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
});

describe("GitManagerPullRequestPanel", () => {
  it.each([
    ["gitlab", "merge request", "Merge requests", "!14"],
    ["github", "pull request", "Pull requests", "#14"],
    ["unknown", "change request", "Change requests", "#14"],
  ] as const)(
    "uses %s terminology and number references from the known provider",
    async (kind, noun, title, number) => {
      h.result = {
        status: "available",
        pullRequests: [
          {
            number: 14,
            title: "Review this branch",
            url: "https://forge.invalid/team/repo/requests/14",
            baseBranch: "main",
            headBranch: "feature",
            state: "open",
          },
        ],
        checks: [],
      };
      await renderPanel(vi.fn(), null, { kind, name: "Forge", baseUrl: "https://forge.invalid" });
      expect(button(`Create ${noun}`).disabled).toBe(false);
      expect(container.querySelector("section")?.getAttribute("aria-label")).toBe(
        `${title} and checks`,
      );
      expect(container.querySelector("h2")?.textContent).toBe(`${title} and checks`);
      expect(container.textContent).toContain(
        `${title} and checks load only when you choose Refresh.`,
      );
      expect(h.listRequests).not.toHaveBeenCalled();
      await act(async () => button("Refresh").click());
      expect(container.textContent).toContain(`${number} · feature → main`);
      expect(container.textContent).toContain(`Current ${noun}`);
      expect(container.textContent).toContain(`${title} and checks loaded.`);
      expect(h.listRequests).toHaveBeenCalledOnce();
    },
  );

  it("uses neutral change-request wording until status answers", async () => {
    await renderPanel(vi.fn(), null, null, false);

    expect(container.querySelector("section")?.getAttribute("aria-label")).toBe(
      "Change requests and checks",
    );
    expect(container.querySelector("h2")?.textContent).toBe("Change requests and checks");
    expect(button("Create change request").disabled).toBe(false);
    expect(container.textContent).toContain(
      "Change requests and checks load only when you choose Refresh.",
    );
    expect(container.textContent).not.toContain("ull request");
    expect(h.listRequests).not.toHaveBeenCalled();

    // Once status names a GitLab host, the pane says merge request throughout.
    const gitlab = { kind: "gitlab", name: "GitLab", baseUrl: "https://gitlab.invalid" } as const;
    await renderPanel(vi.fn(), null, gitlab, true);
    expect(container.querySelector("h2")?.textContent).toBe("Merge requests and checks");
    expect(button("Create merge request").disabled).toBe(false);
  });

  it("keeps pull-request wording once status reports no provider", async () => {
    await renderPanel();
    expect(container.querySelector("h2")?.textContent).toBe("Pull requests and checks");
    expect(button("Create pull request").disabled).toBe(false);
    expect(container.textContent).toContain(
      "Pull requests and checks load only when you choose Refresh.",
    );
  });

  it("names the host from a known provider before status answers", async () => {
    await renderPanel(
      vi.fn(),
      null,
      { kind: "gitlab", name: "GitLab", baseUrl: "https://gitlab.invalid" },
      false,
    );
    expect(container.querySelector("h2")?.textContent).toBe("Merge requests and checks");
  });

  it("issues no provider request on mount or after an idle hour", async () => {
    await renderPanel();
    expect(container.textContent).toContain("load only when you choose Refresh");
    expect(h.listRequests).not.toHaveBeenCalled();
    await act(async () => vi.advanceTimersByTime(60 * 60 * 1_000));
    expect(h.listRequests).not.toHaveBeenCalled();
    expect(h.refreshRequests).not.toHaveBeenCalled();
  });

  it("issues exactly one request per Refresh press", async () => {
    const onRefresh = await renderPanel();
    await act(async () => button("Refresh").click());
    expect(h.listRequests).toHaveBeenCalledOnce();
    expect(h.refreshRequests).not.toHaveBeenCalled();
    expect(onRefresh).toHaveBeenCalledOnce();

    await act(async () => button("Refresh").click());
    expect(h.listRequests).toHaveBeenCalledOnce();
    expect(h.refreshRequests).toHaveBeenCalledOnce();
    expect(onRefresh).toHaveBeenCalledTimes(2);
  });

  it("disables provider actions with their reason while the rest of the pane remains", async () => {
    const reason = "This environment does not support Git Manager pull request operations.";
    const onRefresh = await renderPanel(vi.fn(), reason);

    expect(container.textContent).toContain("Pull requests and checks");
    expect(container.textContent).toContain(reason);
    expect(button("Create pull request")).toMatchObject({ disabled: true, title: reason });
    expect(button("Refresh")).toMatchObject({ disabled: true, title: reason });
    expect(h.listRequests).not.toHaveBeenCalled();
    expect(h.createPr).not.toHaveBeenCalled();
    expect(onRefresh).not.toHaveBeenCalled();
  });

  it("renders unavailable as explanation and loaded checks as local text", async () => {
    h.result = { status: "unavailable", pullRequests: [], checks: [] };
    await renderPanel();
    await act(async () => button("Refresh").click());
    expect(container.textContent).toContain("unavailable for this repository provider");

    h.result = {
      status: "available",
      pullRequests: [
        {
          number: 42,
          title: "Local author data only",
          url: "https://github.test/42",
          baseBranch: "main",
          headBranch: "feature",
          state: "open",
        },
      ],
      checks: [{ name: "build", state: "SUCCESS", link: null, workflow: "CI" }],
    };
    await renderPanel();
    expect(container.textContent).toContain("Local author data only");
    expect(container.textContent).toContain("build");
    expect(container.querySelector("img")).toBeNull();
  });

  it("opens the review surface without publishing or creating anything", async () => {
    const onRefresh = await renderPanel();
    expect(container.querySelector('[data-testid="create-pr-dialog"]')).toBeNull();

    await act(async () => button("Create pull request").click());

    expect(container.querySelector('[data-testid="create-pr-dialog"]')).not.toBeNull();
    expect(h.createPr).not.toHaveBeenCalled();
    expect(h.listRequests).not.toHaveBeenCalled();
    expect(onRefresh).not.toHaveBeenCalled();
    const dialog = h.dialogProps.at(-1);
    if (dialog === undefined) throw new Error("The review dialog did not render.");
    expect(dialog).toMatchObject({
      open: true,
      scope: { environmentId: "env-a", cwd: "/repo" },
    });
    const onSettled = dialog.onSettled as () => void;
    const onOpenChange = dialog.onOpenChange as (open: boolean) => void;

    // A settled pull request refreshes the pane the same way Refresh does.
    await act(async () => onSettled());
    expect(onRefresh).toHaveBeenCalledOnce();
    expect(h.listRequests).toHaveBeenCalledOnce();

    await act(async () => onOpenChange(false));
    expect(container.querySelector('[data-testid="create-pr-dialog"]')).toBeNull();
    expect(h.createPr).not.toHaveBeenCalled();
  });
});

it("links each current-branch row to its project-scoped Pull Requests detail", async () => {
  const projectRef = { environmentId: "env-a", projectId: "project-a" } as never;
  usePullRequestsStore.getState().setCheckoutCwd(projectRef, "/wrong-checkout");
  h.result = {
    status: "available",
    pullRequests: [
      {
        number: 14,
        title: "Review me",
        url: "https://github.test/14",
        baseBranch: "main",
        headBranch: "feature",
        state: "open",
      },
    ],
    checks: [],
  };
  const route = createRootRoute({
    component: () => (
      <GitManagerPullRequestPanel
        scope={{ environmentId: "env-a" as never, cwd: "/repo" }}
        projectRef={{ environmentId: "env-a", projectId: "project-a" } as never}
        statusLoaded
        onRefresh={() => undefined}
      />
    ),
  });
  const router = createRouter({
    routeTree: route,
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  await act(async () => {
    await router.load();
    root.render(<RouterProvider router={router} />);
  });
  await act(async () => button("Refresh").click());
  const link = [...container.querySelectorAll("a")].find(
    (a) => a.textContent === "Open in Pull Requests",
  );
  expect(link?.getAttribute("href")).toBe(
    "/project/env-a/project-a/pull-requests/14?tab=conversation",
  );
  await act(async () =>
    link!.dispatchEvent(
      new MouseEvent("click", { bubbles: true, cancelable: true, ctrlKey: true }),
    ),
  );
  expect(usePullRequestsStore.getState().selectViewState(projectRef).checkoutCwd).toBe("/repo");
});
