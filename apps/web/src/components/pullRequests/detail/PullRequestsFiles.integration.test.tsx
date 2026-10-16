// @vitest-environment happy-dom
import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vite-plus/test";
import { usePullRequestsStore } from "../../../pullRequestsStore";
import { context, detail, file } from "./testFixtures";
vi.mock("@tanstack/react-router", () => ({
  useLocation: () => ({ hash: "" }),
  useNavigate: () => () => Promise.resolve(),
}));
vi.mock("../../../hooks/useSettings", () => ({
  useClientSettings: (select: (settings: { diffIgnoreWhitespace: boolean }) => unknown) =>
    select({ diffIgnoreWhitespace: false }),
}));
vi.mock("../../../hooks/useTheme", () => ({ useTheme: () => ({ resolvedTheme: "light" }) }));
vi.mock("../../DiffWorkerPoolProvider", () => ({
  DiffWorkerPoolProvider: ({ children }: { children: ReactNode }) => children,
}));
import { PullRequestsFiles } from "./PullRequestsFiles";
import { PullRequestsRouteNavigation } from "./pullRequestsNavigation";

const projectRef = { environmentId: "env", projectId: "project" } as never;
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
it("updates mounted real LegendList rows for Viewed and whitespace changes", async () => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(600);
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(800);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(
    new DOMRect(0, 0, 800, 56),
  );
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  usePullRequestsStore.setState({ byProjectKey: {} });
  try {
    await act(async () =>
      root.render(
        <PullRequestsRouteNavigation projectRef={projectRef} number={detail.number}>
          <PullRequestsFiles
            files={{
              files: [
                {
                  ...file,
                  patch: "--- a/src/a.ts\n+++ b/src/a.ts\n@@ -10 +12 @@\n-  same\n+same\n",
                },
              ],
              diffRefs: { baseSha: "base", startSha: "start", headSha: "head" },
              truncated: false,
            }}
            detail={detail}
            projectRef={projectRef}
            context={context}
          />
        </PullRequestsRouteNavigation>,
      ),
    );
    const boxes = () => [
      ...container.querySelectorAll<HTMLInputElement>('input[aria-label="Viewed src/a.ts"]'),
    ];
    await vi.waitFor(() => expect(boxes()).toHaveLength(2));
    expect(boxes().map((box) => box.checked)).toEqual([false, false]);
    await act(async () => boxes()[0]!.click());
    expect(boxes().map((box) => box.checked)).toEqual([true, true]);
    await act(async () =>
      [...container.querySelectorAll("button")]
        .find((button) => button.textContent === "Show diff")!
        .click(),
    );
    const deletions = () =>
      container
        .querySelector("diffs-container")
        ?.shadowRoot?.querySelectorAll('[data-line-type="change-deletion"]');
    await vi.waitFor(() => expect(deletions()!.length).toBeGreaterThan(0));
    await act(async () =>
      container.querySelector<HTMLInputElement>('input[aria-label="Ignore whitespace"]')!.click(),
    );
    await vi.waitFor(() => expect(deletions()).toHaveLength(0));
  } finally {
    await act(async () => root.unmount());
    container.remove();
  }
});
