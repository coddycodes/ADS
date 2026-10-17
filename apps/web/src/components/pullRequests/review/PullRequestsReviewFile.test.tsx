// @vitest-environment happy-dom
import type { FileDiffOptions, DiffLineAnnotation } from "@pierre/diffs";
import { act, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { usePullRequestsStore } from "../../../pullRequestsStore";
import { allowed, click, input, mount, projectRef } from "./testHelpers";
import { context, detail, file, thread } from "../detail/testFixtures";
const h = vi.hoisted(() => ({
  options: undefined as FileDiffOptions<ReactNode> | undefined,
  annotations: [] as DiffLineAnnotation<ReactNode>[],
}));
vi.mock("@pierre/diffs/react", () => ({
  FileDiff: (props: {
    options: FileDiffOptions<ReactNode>;
    lineAnnotations?: DiffLineAnnotation<ReactNode>[];
    renderAnnotation?: (annotation: DiffLineAnnotation<ReactNode>) => ReactNode;
  }) => {
    h.options = props.options;
    h.annotations = props.lineAnnotations ?? [];
    return (
      <div data-diff>
        {h.annotations.map((a) => (
          <div key={`${a.side}:${a.lineNumber}`} data-line={a.lineNumber} data-side={a.side}>
            {props.renderAnnotation?.(a)}
          </div>
        ))}
      </div>
    );
  },
}));
vi.mock("@legendapp/list/react", () => ({
  LegendList: ({
    data,
    keyExtractor,
    renderItem,
    ListHeaderComponent,
    ListFooterComponent,
  }: {
    data: unknown[];
    keyExtractor: (item: unknown) => string;
    renderItem: (value: { item: unknown }) => ReactNode;
    ListHeaderComponent?: ReactNode;
    ListFooterComponent?: ReactNode;
  }) => (
    <div>
      {ListHeaderComponent}
      {data.map((item) => (
        <div key={keyExtractor(item)}>{renderItem({ item })}</div>
      ))}
      {ListFooterComponent}
    </div>
  ),
}));
vi.mock("@tanstack/react-router", () => ({
  useLocation: () => ({ hash: "" }),
  useNavigate: () => vi.fn(),
  Link: ({ children }: { children: ReactNode }) => <a>{children}</a>,
}));
vi.mock("../../../hooks/useSettings", () => ({ useClientSettings: () => false }));
vi.mock("../../../hooks/useTheme", () => ({ useTheme: () => ({ resolvedTheme: "light" }) }));
vi.mock("../../DiffWorkerPoolProvider", () => ({
  DiffWorkerPoolProvider: ({ children }: { children: ReactNode }) => children,
}));
import { PullRequestsFiles } from "../detail/PullRequestsFiles";
import { PullRequestsRouteNavigation } from "../detail/pullRequestsNavigation";
let view: Awaited<ReturnType<typeof mount>>;
const files = {
  files: [
    {
      ...file,
      patch: "--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1,3 +1,3 @@\n-old\n+new\n context\n last\n",
    },
  ],
  diffRefs: { baseSha: "base", startSha: "start", headSha: "head" },
  truncated: false,
};
const ui = (
  <PullRequestsRouteNavigation projectRef={projectRef} number={detail.number}>
    <PullRequestsFiles
      files={files}
      detail={{ ...detail, permissions: { ...detail.permissions, review: allowed } }}
      projectRef={projectRef}
      context={context}
      timeline={{ items: [{ ...thread, path: file.path, line: 1 }], truncated: false }}
    />
  </PullRequestsRouteNavigation>
);
beforeEach(() => {
  usePullRequestsStore.setState({ byProjectKey: {} });
  h.annotations = [];
});
afterEach(async () => {
  await view?.unmount();
});
describe("Files review integration", () => {
  it("keeps drafts editable/removable when their file disappears from the diff", async () => {
    const orphan = {
      id: "orphan",
      path: "removed.ts",
      line: 4,
      startLine: null,
      side: "right" as const,
      body: "Pending on removed file",
    };
    usePullRequestsStore.getState().setPendingReview(projectRef, detail.number, [orphan]);
    usePullRequestsStore.getState().setInlineDraft(projectRef, detail.number, {
      ...orphan,
      id: "unsent",
      body: "Unadded on removed file",
    });
    view = await mount(ui);
    expect(view.container.textContent).toContain("Pending on removed file");
    expect(view.container.querySelector("textarea")?.value).toBe("Unadded on removed file");
    await click("Remove");
    expect(
      usePullRequestsStore.getState().selectDraft(projectRef, detail.number).pendingReview,
    ).toEqual([]);
    expect(
      usePullRequestsStore.getState().selectDraft(projectRef, detail.number).inlineDrafts,
    ).toHaveLength(1);
  });
  it("offers Apply N selected in the Files header and sends the selected suggestion ids", async () => {
    const suggestion = thread.comments[0]!.suggestion!;
    view = await mount(
      <PullRequestsRouteNavigation projectRef={projectRef} number={detail.number}>
        <PullRequestsFiles
          files={files}
          detail={{ ...detail, permissions: { ...detail.permissions, applySuggestion: allowed } }}
          context={context}
          projectRef={projectRef}
          timeline={{
            items: [
              {
                ...thread,
                path: file.path,
                line: 1,
                comments: [
                  { ...thread.comments[0]!, suggestion: { ...suggestion, id: "101" } },
                  {
                    ...thread.comments[0]!,
                    id: "rc:2:node",
                    suggestion: { ...suggestion, id: "102" },
                  },
                ],
              },
            ],
            truncated: false,
          }}
        />
      </PullRequestsRouteNavigation>,
    );
    await click("Show diff");
    const checkboxes = view.container.querySelectorAll<HTMLInputElement>(
      'input[aria-label^="Select suggestion"]',
    );
    expect(checkboxes).toHaveLength(2);
    await act(async () => {
      for (const checkbox of checkboxes) checkbox.click();
    });
    expect(view.container.querySelector("header")?.textContent).toContain("Apply 2 selected");
    await click("Apply 2 selected");
    await click("Apply 2 suggestions");
    expect(view.run).toHaveBeenCalledWith({
      action: "applySuggestions",
      suggestionIds: ["101", "102"],
      commitMessage: null,
    });
    expect(view.container.querySelector("header")?.textContent).not.toContain("Apply 2 selected");
  });
  it("click opens a composer beneath the correct diff side/line; pending comments stay anchored after reload", async () => {
    view = await mount(ui);
    await click("Show diff");
    await act(async () =>
      h.options!.onLineClick!({ lineNumber: 2, annotationSide: "additions" } as never),
    );
    expect(
      view.container.querySelector('[data-line="2"][data-side="additions"] textarea'),
    ).not.toBeNull();
    await input(view.container.querySelector('[data-line="2"] textarea')!, "Inline pending");
    await click("Add review comment");
    expect(view.container.querySelector('[data-line="2"]')?.textContent).toContain(
      "Inline pending",
    );
    await view.render(null);
    await act(async () => {
      await usePullRequestsStore.persist.rehydrate();
    });
    await view.render(ui);
    await click("Show diff");
    expect(view.container.querySelector('[data-line="2"]')?.textContent).toContain(
      "Inline pending",
    );
    expect(view.container.querySelector('[data-line="1"]')?.textContent).toContain(
      "Thread comment",
    );
  });
  it("shift-drag normalizes the range and anchors the suggestion composer at its final line", async () => {
    view = await mount(ui);
    await click("Show diff");
    await act(async () => h.options!.onLineSelectionEnd!({ start: 3, end: 1, side: "additions" }));
    expect(view.container.querySelector('[data-line="3"] textarea')).not.toBeNull();
    await click("Insert suggestion");
    expect(usePullRequestsStore.getState().selectDraft(projectRef, 14).inlineDrafts[0]).toEqual(
      expect.objectContaining({
        startLine: 1,
        line: 3,
        side: "right",
        body: "```suggestion\nnew\ncontext\nlast\n```",
      }),
    );
  });
  it("does not discard an existing unsent composer when another line is selected", async () => {
    view = await mount(ui);
    await click("Show diff");
    await act(async () =>
      h.options!.onLineClick!({ lineNumber: 2, annotationSide: "deletions" } as never),
    );
    await input(view.container.querySelector('[data-line="2"] textarea')!, "Keep this");
    await act(async () =>
      h.options!.onLineClick!({ lineNumber: 3, annotationSide: "additions" } as never),
    );
    expect(usePullRequestsStore.getState().selectDraft(projectRef, 14).inlineDrafts).toHaveLength(
      2,
    );
  });
});
