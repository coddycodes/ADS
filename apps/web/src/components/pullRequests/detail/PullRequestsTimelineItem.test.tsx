import { PullRequestsActionsContext } from "../usePullRequestsAction";
// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { renderReviewMarkup as renderToStaticMarkup, mount, click } from "../review/testHelpers";
import { describe, expect, it, vi } from "vite-plus/test";
import { PullRequestsNavigationContext } from "./pullRequestsNavigation";
import { PullRequestsTimelineItem } from "./PullRequestsTimelineItem";
import { comment, context, detail, event, fakeNavigation, review, thread } from "./testFixtures";
const projectRef = { environmentId: "env", projectId: "project" } as never;
const props = {
  url: "https://github.example/repo/pull/14",
  projectRef,
  number: 14,
  permissions: detail.permissions,
  context,
};
const navigation = fakeNavigation();
describe("PullRequestsTimelineItem", () => {
  it("collapses a minimized thread comment and offers Unminimize using its authoritative state", async () => {
    const item = { ...thread, comments: [{ ...thread.comments[0]!, minimized: true }] };
    const view = await mount(
      <PullRequestsNavigationContext value={navigation}>
        <PullRequestsTimelineItem
          {...props}
          permissions={{ ...props.permissions, minimizeComment: { allowed: true, reason: null } }}
          item={item}
        />
      </PullRequestsNavigationContext>,
    );
    try {
      expect(view.container.textContent).not.toContain("Thread comment");
      await click("Comment actions");
      await click("Unminimize");
      expect(view.run).toHaveBeenCalledWith({
        action: "minimizeComment",
        commentId: item.comments[0]!.id,
        minimized: false,
      });
      await click("Show comment");
      expect(view.container.textContent).toContain("Thread comment");
    } finally {
      await view.unmount();
    }
  });
  it("uses a safe fallback for an unknown event matching an object property", () => {
    const html = renderToStaticMarkup(
      <PullRequestsTimelineItem
        {...props}
        item={{ ...event, event: "constructor", detail: null }}
      />,
    );
    expect(html).toContain("updated the request");
  });
  it.each([
    ["review_requested", "alice", "requested review from alice"],
    ["ready_for_review", null, "marked ready for review"],
    ["converted_to_draft", null, "converted to draft"],
    ["head_ref_force_pushed", null, "force-pushed the head branch"],
    ["commits_added", "added 2 commits", "added 2 commits"],
    ["unknown", "Host activity", "updated the request: Host activity"],
  ] as const)("presents %s as readable activity", (kind, value, expected) => {
    const html = renderToStaticMarkup(
      <PullRequestsTimelineItem {...props} item={{ ...event, event: kind, detail: value }} />,
    );
    expect(html).toContain(expected);
    expect(html).not.toContain(`>${kind}<`);
  });
  it.each([
    [comment, "A comment"],
    [review, "approved these changes"],
    [thread, "Thread comment"],
    [event, "labeled"],
  ] as const)("renders $0.kind", (item, text) => {
    const html = renderToStaticMarkup(
      <PullRequestsNavigationContext value={navigation}>
        <PullRequestsTimelineItem {...props} item={item} />
      </PullRequestsNavigationContext>,
    );
    expect(html).toContain(text);
    expect(html).toContain("mubeda");
    expect(html).not.toContain("<img");
  });
  it("shows edited comments and read-only reactions", () => {
    const html = renderToStaticMarkup(<PullRequestsTimelineItem {...props} item={comment} />);
    expect(html).toContain("edited");
    expect(html).toContain("👍");
  });
  it("renders a thread's hunk, badges and suggestion", () => {
    const html = renderToStaticMarkup(
      <PullRequestsNavigationContext value={navigation}>
        <PullRequestsTimelineItem {...props} item={thread} />
      </PullRequestsNavigationContext>,
    );
    expect(html).toContain("@@ -3 +3 @@");
    expect(html).toContain("Resolved");
    expect(html).toContain("Outdated");
    expect(html).toContain("Lines 3–4");
  });
  it("opens the file in the Changes tab when a thread's path is clicked", async () => {
    const showFile = vi.fn();
    const view = await mount(
      <PullRequestsNavigationContext value={fakeNavigation({ showFile })}>
        <PullRequestsTimelineItem {...props} item={thread} />
      </PullRequestsNavigationContext>,
    );
    try {
      await click("src/a b#é.ts:3–4");
      expect(showFile).toHaveBeenCalledWith("src/a b#é.ts");
    } finally {
      await view.unmount();
    }
  });
  it("keeps a minimized comment collapsed until Show comment", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const container = document.createElement("div");
    const root = createRoot(container);
    try {
      await act(async () =>
        root.render(
          <PullRequestsActionsContext
            value={{ run: vi.fn(), pending: false, error: null, requestKind: "pull request" }}
          >
            <PullRequestsTimelineItem {...props} item={{ ...comment, minimized: true }} />
          </PullRequestsActionsContext>,
        ),
      );
      expect(container.textContent).not.toContain("A comment");
      expect(container.textContent).toContain("Show comment");
      await act(async () => container.querySelector("button")!.click());
      expect(container.textContent).toContain("A comment");
    } finally {
      await act(async () => root.unmount());
    }
  });
});
