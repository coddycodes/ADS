import { projectKey } from "@coddy/client-runtime/state/entities";
import type { ScopedProjectRef } from "@coddy/contracts";
import { beforeEach, describe, expect, it } from "vite-plus/test";
import { createJSONStorage } from "zustand/middleware";

import { usePullRequestsStore, PULL_REQUESTS_STORAGE_KEY } from "./pullRequestsStore";

const ref = (environmentId: string, projectId: string) =>
  ({ environmentId, projectId }) as ScopedProjectRef;
const persisted = new Map<string, string>();
const storage = {
  getItem: (name: string) => persisted.get(name) ?? null,
  setItem: (name: string, value: string) => {
    persisted.set(name, value);
  },
  removeItem: (name: string) => {
    persisted.delete(name);
  },
};

describe("pullRequestsStore", () => {
  beforeEach(() => {
    persisted.clear();
    usePullRequestsStore.persist.setOptions({ storage: createJSONStorage(() => storage) });
    usePullRequestsStore.setState({ byProjectKey: {} });
  });

  it("keeps the two most recently used projects", () => {
    const s = usePullRequestsStore.getState();
    s.touchProject(ref("a", "1"));
    s.touchProject(ref("a", "2"));
    s.touchProject(ref("a", "1"));
    s.touchProject(ref("a", "3"));
    expect(Object.keys(usePullRequestsStore.getState().byProjectKey)).toEqual([
      projectKey(ref("a", "3")),
      projectKey(ref("a", "1")),
    ]);
  });

  it("stores drafts per pull request number and survives partialize", async () => {
    const s = usePullRequestsStore.getState();
    const p = ref("a", "1");
    s.setCommentDraft(p, 14, "hello");
    s.setCommentDraft(p, 15, "other");
    s.setPendingReview(p, 14, [
      { id: "i", path: "src/a.ts", line: 2, startLine: null, side: "right", body: "Review" },
    ]);
    s.setMergeDraft(p, 14, "subject", "body");
    s.setTitleEdit(p, 14, "title");
    s.setBodyEdit(p, 14, "");
    s.setCommentEdit(p, 14, "comment-id", "edited");
    s.setReplyDraft(p, 14, "ds:opaque", "reply draft");
    const serialized = persisted.get(PULL_REQUESTS_STORAGE_KEY)!;
    usePullRequestsStore.setState({ byProjectKey: {} });
    persisted.set(PULL_REQUESTS_STORAGE_KEY, serialized);
    await usePullRequestsStore.persist.rehydrate();
    expect(s.selectDraft(p, 14)).toEqual({
      comment: "hello",
      pendingReview: [
        { id: "i", path: "src/a.ts", line: 2, startLine: null, side: "right", body: "Review" },
      ],
      mergeSubject: "subject",
      mergeBody: "body",
      mergeDraftEdited: true,
      titleEdit: "title",
      bodyEdit: "",
      commentEdits: { "comment-id": "edited" },
      replyDrafts: { "ds:opaque": "reply draft" },
      inlineDrafts: [],
      reviewBody: "",
    });
    s.clearDraft(p, 14);
    expect(s.selectDraft(p, 14).comment).toBe("");
    expect(s.selectDraft(p, 15).comment).toBe("other");
    expect(s.selectDraft(ref("b", "1"), 15).comment).toBe("");
  });

  it("distinguishes a deliberately blank merge draft from untouched host defaults across reload", async () => {
    const s = usePullRequestsStore.getState();
    const p = ref("a", "1");
    expect(s.selectDraft(p, 14).mergeDraftEdited).toBe(false);
    s.setMergeDraft(p, 14, "", "");
    const serialized = persisted.get(PULL_REQUESTS_STORAGE_KEY)!;
    usePullRequestsStore.setState({ byProjectKey: {} });
    persisted.set(PULL_REQUESTS_STORAGE_KEY, serialized);
    await usePullRequestsStore.persist.rehydrate();
    expect(s.selectDraft(p, 14)).toMatchObject({
      mergeSubject: "",
      mergeBody: "",
      mergeDraftEdited: true,
    });
    s.setMergeDraft(p, 14, null, null);
    expect(s.selectDraft(p, 14).mergeDraftEdited).toBe(false);
  });

  it("keeps viewed files per pull request", () => {
    const s = usePullRequestsStore.getState();
    s.toggleViewedFile(ref("a", "1"), 14, "src/a.ts");
    expect(s.selectViewState(ref("a", "1")).viewedFiles[14]).toEqual(["src/a.ts"]);
    expect(s.selectViewState(ref("a", "1")).viewedFiles[15]).toBeUndefined();
    s.toggleViewedFile(ref("a", "1"), 14, "src/a.ts");
    expect(s.selectViewState(ref("a", "1")).viewedFiles[14]).toEqual([]);
  });

  it("round trips checkout, list state, scroll and last number without losing drafts", async () => {
    const s = usePullRequestsStore.getState();
    const p = ref("a", "1");
    s.setCheckoutCwd(p, "Z:\\opaque\\checkout");
    s.setListTab(p, "closed");
    s.setFilters(p, { search: "bug", labels: ["fix"] });
    s.setFilters(p, { author: "@me" });
    s.setSort(p, "oldest");
    s.setScrollTop(p, 112);
    s.setLastNumber(p, 14);
    const serialized = persisted.get(PULL_REQUESTS_STORAGE_KEY)!;
    usePullRequestsStore.setState({ byProjectKey: {} });
    persisted.set(PULL_REQUESTS_STORAGE_KEY, serialized);
    await usePullRequestsStore.persist.rehydrate();
    expect(s.selectViewState(p)).toMatchObject({
      checkoutCwd: "Z:\\opaque\\checkout",
      listTab: "closed",
      filters: { search: "bug", labels: ["fix"], author: "@me" },
      sort: "oldest",
      scrollTop: 112,
      lastNumber: 14,
    });
  });

  it("sanitizes corrupt persisted fields and retains valid draft text", async () => {
    persisted.set(
      PULL_REQUESTS_STORAGE_KEY,
      JSON.stringify({
        version: 1,
        state: {
          byProjectKey: {
            bad: {},
            [projectKey(ref("a", "1"))]: {
              listTab: "bad",
              filters: null,
              scrollTop: -5,
              drafts: {
                14: {
                  comment: "keep",
                  pendingReview: [null],
                  commentEdits: { valid: "text", bad: 2 },
                },
              },
              viewedFiles: { 14: ["a", null, "a"] },
            },
          },
        },
      }),
    );
    await usePullRequestsStore.persist.rehydrate();
    const s = usePullRequestsStore.getState();
    expect(Object.keys(s.byProjectKey)).toEqual([projectKey(ref("a", "1"))]);
    expect(s.selectViewState(ref("a", "1"))).toMatchObject({
      listTab: "open",
      scrollTop: 0,
      viewedFiles: { 14: ["a"] },
    });
    expect(s.selectDraft(ref("a", "1"), 14)).toMatchObject({
      comment: "keep",
      pendingReview: [],
      commentEdits: { valid: "text" },
    });
  });
});
