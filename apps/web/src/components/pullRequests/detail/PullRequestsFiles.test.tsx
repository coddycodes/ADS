// @vitest-environment happy-dom
import { act, useImperativeHandle, type ReactNode, type Ref } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { createJSONStorage } from "zustand/middleware";
import { usePullRequestsStore } from "../../../pullRequestsStore";
import type { PullRequestsDetail, PullRequestsTimeline } from "@bibcode/contracts";
import { PullRequestsActionsContext } from "../usePullRequestsAction";
import { allowed, mockRun } from "../review/testHelpers";
import { context, detail, file, thread } from "./testFixtures";
const h = vi.hoisted(() => ({
  hash: "",
  navigate: vi.fn(),
  scroll: vi.fn().mockResolvedValue(undefined),
  lists: [] as Record<string, unknown>[],
}));
vi.mock("@tanstack/react-router", () => ({
  useLocation: () => ({ hash: h.hash }),
  useNavigate: () => h.navigate,
  Link: ({ children }: { children: ReactNode }) => <a>{children}</a>,
}));
vi.mock("../../../hooks/useSettings", () => ({
  useClientSettings: (select: (settings: { diffIgnoreWhitespace: boolean }) => unknown) =>
    select({ diffIgnoreWhitespace: true }),
}));
vi.mock("../../../hooks/useTheme", () => ({ useTheme: () => ({ resolvedTheme: "light" }) }));
vi.mock("../../DiffWorkerPoolProvider", () => ({
  DiffWorkerPoolProvider: ({ children }: { children: ReactNode }) => children,
}));
vi.mock("@legendapp/list/react", () => ({
  LegendList: (props: {
    data: readonly unknown[];
    keyExtractor: (item: unknown) => string;
    renderItem: (p: { item: unknown }) => ReactNode;
    ref?: Ref<unknown>;
  }) => {
    h.lists.push(props);
    useImperativeHandle(props.ref, () => ({ scrollToIndex: h.scroll }));
    return (
      <div>
        {props.data.map((item) => (
          <div key={props.keyExtractor(item)}>{props.renderItem({ item })}</div>
        ))}
      </div>
    );
  },
}));
import { PullRequestsFiles } from "./PullRequestsFiles";
import { PullRequestsRouteNavigation } from "./pullRequestsNavigation";
let root: Root;
let container: HTMLDivElement;
const persisted = new Map<string, string>();
const storage = {
  getItem: (key: string) => persisted.get(key) ?? null,
  setItem: (key: string, value: string) => {
    persisted.set(key, value);
  },
  removeItem: (key: string) => {
    persisted.delete(key);
  },
};
const projectRef = { environmentId: "env", projectId: "project" } as never;
const files = {
  files: [file, { ...file, path: "docs/read me#é.md", patch: null }],
  diffRefs: { baseSha: "base", startSha: "start", headSha: "head" },
  truncated: false,
};
async function render(
  overrideDetail: PullRequestsDetail = detail,
  timeline?: PullRequestsTimeline,
  live?: PullRequestsDetail | null,
) {
  await act(async () =>
    root.render(
      <PullRequestsActionsContext
        value={{ run: mockRun(), pending: false, error: null, requestKind: "pull request" }}
      >
        <PullRequestsRouteNavigation projectRef={projectRef} number={overrideDetail.number}>
          <PullRequestsFiles
            files={files}
            {...(timeline ? { timeline } : {})}
            detail={overrideDetail}
            {...(live === undefined ? {} : { live })}
            projectRef={projectRef}
            context={context}
          />
        </PullRequestsRouteNavigation>
      </PullRequestsActionsContext>,
    ),
  );
}
beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  persisted.clear();
  usePullRequestsStore.persist.setOptions({ storage: createJSONStorage(() => storage) });
  usePullRequestsStore.setState({ byProjectKey: {} });
  h.hash = "";
  h.lists = [];
  vi.clearAllMocks();
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});
describe("PullRequestsFiles", () => {
  it("virtualizes tree and diff lists, scrolls to the selected file and persists Viewed", async () => {
    await render();
    expect(container.textContent).toContain("docs");
    expect(container.textContent).toContain("src");
    expect(h.lists.some((p) => (p.data as unknown[]).length === 4)).toBe(true);
    expect(h.lists.some((p) => (p.data as unknown[]).length === 2)).toBe(true);
    await act(async () =>
      container.querySelector<HTMLButtonElement>('[data-file-path="docs/read me#é.md"]')!.click(),
    );
    expect(h.scroll).toHaveBeenCalledWith({ index: 1, animated: false, viewPosition: 0 });
    expect(h.navigate).toHaveBeenCalledWith(
      expect.objectContaining({ hash: "file=docs%2Fread%20me%23%C3%A9.md", replace: true }),
    );
    await act(async () =>
      container.querySelector<HTMLInputElement>('input[aria-label="Viewed src/a.ts"]')!.click(),
    );
    expect(usePullRequestsStore.getState().selectViewState(projectRef).viewedFiles[14]).toEqual([
      "src/a.ts",
    ]);
    const saved = persisted.get("bibcode:pull-requests-state:v1")!;
    expect(saved).toContain("src/a.ts");
    await act(async () => root.render(null));
    usePullRequestsStore.setState({ byProjectKey: {} });
    persisted.set("bibcode:pull-requests-state:v1", saved);
    await usePullRequestsStore.persist.rehydrate();
    await render();
    expect(
      container.querySelector<HTMLInputElement>('input[aria-label="Viewed src/a.ts"]')!.checked,
    ).toBe(true);
    expect(
      container.querySelector<HTMLInputElement>('input[aria-label="Ignore whitespace"]')!.checked,
    ).toBe(true);
  });
  it("lays out the changed-files tree by the detail's width, not the window's", async () => {
    await render();
    const tree = container.querySelector('nav[aria-label="Changed files"]')!;
    expect(tree.className).toContain("@3xl/pull-request:w-64");
    expect(tree.className).toContain("@3xl/pull-request:border-r");
    expect(tree.className).not.toMatch(/(^|\s)md:/);
    expect(tree.parentElement!.className).toContain("@3xl/pull-request:flex-row");
    expect(tree.parentElement!.className).not.toMatch(/(^|\s)md:/);
  });
  it("selects and scrolls the file encoded in the route hash", async () => {
    h.hash = "file=docs%2Fread%20me%23%C3%A9.md";
    await render();
    expect(h.scroll).toHaveBeenCalledWith({ index: 1, animated: false, viewPosition: 0 });
    expect(
      container.querySelector('[data-file-path="docs/read me#é.md"]')?.getAttribute("aria-current"),
    ).toBe("true");
  });
  it("leaves the bulk apply-suggestions control inactive with title Loading… when live is null", async () => {
    const suggestible: PullRequestsDetail = {
      ...detail,
      permissions: { ...detail.permissions, applySuggestion: allowed },
    };
    const threadAt = (id: string, suggestionId: string, fromLine: number, toLine: number) => ({
      ...thread,
      id,
      path: file.path,
      comments: [
        {
          ...thread.comments[0]!,
          id: `${id}:comment`,
          suggestion: { ...thread.comments[0]!.suggestion!, id: suggestionId, fromLine, toLine },
        },
      ],
    });
    const timeline: PullRequestsTimeline = {
      items: [threadAt("th:1", "sg:1", 3, 4), threadAt("th:2", "sg:2", 6, 7)],
      truncated: false,
    };
    await render(suggestible, timeline);
    await act(async () =>
      container.querySelector<HTMLElement>("#file\\=src\\/a\\.ts button")!.click(),
    );
    await act(async () =>
      container
        .querySelector<HTMLInputElement>('input[aria-label="Select suggestion for lines 3–4"]')!
        .click(),
    );
    await act(async () =>
      container
        .querySelector<HTMLInputElement>('input[aria-label="Select suggestion for lines 6–7"]')!
        .click(),
    );
    const bulkButton = () =>
      [...container.querySelectorAll("button")].find((b) => b.textContent === "Apply 2 selected")!;
    expect(bulkButton().disabled).toBe(false);
    // Re-render with a null live detail; the selection state carries over on the
    // same mounted instance, isolating the live gate as the only variable.
    await render(suggestible, timeline, null);
    expect(bulkButton().disabled).toBe(true);
    expect(bulkButton().title).toBe("Loading…");
  });
});
