import { parseProjectKey, projectKey } from "@coddy/client-runtime/state/entities";
import type { PullRequestsListInput, ScopedProjectRef } from "@coddy/contracts";
import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

import { resolveStorage } from "./lib/storage";

export const PULL_REQUESTS_STORAGE_KEY = "coddy-studio:pull-requests-state:v1";
export type PullRequestsDetailTab = "conversation" | "commits" | "checks" | "files";
export type PendingInlineComment = {
  id: string;
  path: string;
  line: number;
  startLine: number | null;
  side: "left" | "right";
  body: string;
};
export type PullRequestsDraft = {
  comment: string;
  reviewBody: string;
  pendingReview: PendingInlineComment[];
  inlineDrafts: PendingInlineComment[];
  mergeSubject: string;
  mergeBody: string;
  mergeDraftEdited: boolean;
  titleEdit: string | null;
  bodyEdit: string | null;
  commentEdits: Record<string, string>;
  replyDrafts: Record<string, string>;
};
export type PullRequestsFilters = Pick<
  PullRequestsListInput,
  | "search"
  | "author"
  | "assignee"
  | "reviewer"
  | "reviewStatus"
  | "draft"
  | "labels"
  | "milestone"
  | "targetBranch"
>;
export interface PullRequestsViewState {
  readonly checkoutCwd: string | null;
  readonly listTab: PullRequestsListInput["state"];
  readonly filters: PullRequestsFilters;
  readonly sort: PullRequestsListInput["sort"];
  readonly scrollTop: number;
  readonly lastNumber: number | null;
  readonly viewedFiles: Record<number, string[]>;
  readonly drafts: Record<number, PullRequestsDraft>;
  readonly lastUsedAt: number;
}
export const DEFAULT_PULL_REQUESTS_FILTERS: PullRequestsFilters = Object.freeze({
  search: null,
  author: null,
  assignee: null,
  reviewer: null,
  reviewStatus: null,
  draft: null,
  labels: Object.freeze([]),
  milestone: null,
  targetBranch: null,
});
const DEFAULT_DRAFT: PullRequestsDraft = {
  comment: "",
  reviewBody: "",
  pendingReview: [],
  inlineDrafts: [],
  mergeSubject: "",
  mergeBody: "",
  mergeDraftEdited: false,
  titleEdit: null,
  bodyEdit: null,
  commentEdits: {},
  replyDrafts: {},
};
export const DEFAULT_PULL_REQUESTS_VIEW_STATE: PullRequestsViewState = Object.freeze({
  checkoutCwd: null,
  listTab: "open",
  filters: DEFAULT_PULL_REQUESTS_FILTERS,
  sort: "newest",
  scrollTop: 0,
  lastNumber: null,
  viewedFiles: {},
  drafts: {},
  lastUsedAt: 0,
});
interface PullRequestsStoreState {
  readonly byProjectKey: Record<string, PullRequestsViewState>;
  readonly selectViewState: (ref: ScopedProjectRef) => PullRequestsViewState;
  readonly selectDraft: (ref: ScopedProjectRef, number: number) => PullRequestsDraft;
  readonly touchProject: (ref: ScopedProjectRef) => void;
  readonly setCheckoutCwd: (ref: ScopedProjectRef, cwd: string | null) => void;
  readonly setListTab: (ref: ScopedProjectRef, tab: PullRequestsViewState["listTab"]) => void;
  readonly setFilters: (ref: ScopedProjectRef, filters: Partial<PullRequestsFilters>) => void;
  readonly setSort: (ref: ScopedProjectRef, sort: PullRequestsViewState["sort"]) => void;
  readonly setScrollTop: (ref: ScopedProjectRef, scrollTop: number) => void;
  readonly setLastNumber: (ref: ScopedProjectRef, number: number | null) => void;
  readonly toggleViewedFile: (ref: ScopedProjectRef, number: number, path: string) => void;
  readonly setCommentDraft: (ref: ScopedProjectRef, number: number, comment: string) => void;
  readonly setReviewBody: (ref: ScopedProjectRef, number: number, body: string) => void;
  readonly setPendingReview: (
    ref: ScopedProjectRef,
    number: number,
    comments: PendingInlineComment[],
  ) => void;
  readonly setInlineDraft: (
    ref: ScopedProjectRef,
    number: number,
    comment: PendingInlineComment,
  ) => void;
  readonly removeInlineDraft: (ref: ScopedProjectRef, number: number, id: string) => void;
  readonly setMergeDraft: (
    ref: ScopedProjectRef,
    number: number,
    subject: string | null,
    body: string | null,
  ) => void;
  readonly setTitleEdit: (ref: ScopedProjectRef, number: number, title: string | null) => void;
  readonly setBodyEdit: (ref: ScopedProjectRef, number: number, body: string | null) => void;
  readonly setCommentEdit: (
    ref: ScopedProjectRef,
    number: number,
    id: string,
    body: string | null,
  ) => void;
  readonly setReplyDraft: (
    ref: ScopedProjectRef,
    number: number,
    threadId: string,
    body: string | null,
  ) => void;
  readonly clearDraft: (ref: ScopedProjectRef, number: number) => void;
}
function retainMostRecent(byProjectKey: Record<string, PullRequestsViewState>) {
  const entries = Object.entries(byProjectKey);
  if (entries.length <= 2) return byProjectKey;
  entries.sort(([a, x], [b, y]) => y.lastUsedAt - x.lastUsedAt || a.localeCompare(b));
  return Object.fromEntries(entries.slice(0, 2));
}
function updateProject(
  state: PullRequestsStoreState,
  ref: ScopedProjectRef,
  update: (current: PullRequestsViewState) => PullRequestsViewState,
) {
  let latest = 0;
  for (const view of Object.values(state.byProjectKey)) latest = Math.max(latest, view.lastUsedAt);
  const key = projectKey(ref);
  return {
    byProjectKey: retainMostRecent({
      ...state.byProjectKey,
      [key]: {
        ...update(state.byProjectKey[key] ?? DEFAULT_PULL_REQUESTS_VIEW_STATE),
        lastUsedAt: Math.max(Date.now(), latest + 1),
      },
    }),
  };
}
function record(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}
function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}
function nonNegative(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
}
function strings(value: unknown): string[] {
  return Array.isArray(value)
    ? [...new Set(value.filter((s): s is string => typeof s === "string" && s.trim().length > 0))]
    : [];
}
function positiveInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}
function sanitizeInlineComments(value: unknown): PendingInlineComment[] {
  const pendingReview: PendingInlineComment[] = [];
  for (const entry of Array.isArray(value) ? value : []) {
    const c = record(entry);
    if (
      typeof c.id === "string" &&
      typeof c.path === "string" &&
      positiveInteger(c.line) &&
      (c.startLine === null || positiveInteger(c.startLine)) &&
      (c.side === "left" || c.side === "right") &&
      typeof c.body === "string"
    ) {
      pendingReview.push({
        id: c.id,
        path: c.path,
        line: c.line,
        startLine: c.startLine,
        side: c.side,
        body: c.body,
      });
    }
  }
  return pendingReview;
}
function sanitizeDraft(value: unknown): PullRequestsDraft {
  const v = record(value);
  return {
    comment: text(v.comment) ?? "",
    reviewBody: text(v.reviewBody) ?? "",
    pendingReview: sanitizeInlineComments(v.pendingReview),
    inlineDrafts: sanitizeInlineComments(v.inlineDrafts),
    mergeSubject: text(v.mergeSubject) ?? "",
    mergeBody: text(v.mergeBody) ?? "",
    mergeDraftEdited:
      typeof v.mergeDraftEdited === "boolean"
        ? v.mergeDraftEdited
        : Boolean(text(v.mergeSubject) || text(v.mergeBody)),
    titleEdit: text(v.titleEdit),
    bodyEdit: text(v.bodyEdit),
    replyDrafts: Object.fromEntries(
      Object.entries(record(v.replyDrafts)).filter(
        (entry): entry is [string, string] => typeof entry[1] === "string",
      ),
    ),
    commentEdits: Object.fromEntries(
      Object.entries(record(v.commentEdits)).filter(
        (entry): entry is [string, string] => typeof entry[1] === "string",
      ),
    ),
  };
}
function sanitizePersistedState(value: unknown): Pick<PullRequestsStoreState, "byProjectKey"> {
  const entries: [string, PullRequestsViewState][] = [];
  for (const [key, raw] of Object.entries(record(record(value).byProjectKey))) {
    try {
      parseProjectKey(key);
    } catch {
      continue;
    }
    const v = record(raw);
    const f = record(v.filters);
    entries.push([
      key,
      {
        checkoutCwd: text(v.checkoutCwd),
        listTab:
          v.listTab === "closed" || v.listTab === "merged" || v.listTab === "all"
            ? v.listTab
            : "open",
        filters: {
          search: text(f.search),
          author: text(f.author),
          assignee: text(f.assignee),
          reviewer: text(f.reviewer),
          reviewStatus:
            f.reviewStatus === "review_required" ||
            f.reviewStatus === "approved" ||
            f.reviewStatus === "changes_requested" ||
            f.reviewStatus === "not_approved"
              ? f.reviewStatus
              : null,
          draft: f.draft === "only" || f.draft === "exclude" ? f.draft : null,
          labels: strings(f.labels),
          milestone: text(f.milestone),
          targetBranch: text(f.targetBranch),
        },
        sort:
          v.sort === "oldest" || v.sort === "recently_updated" || v.sort === "most_commented"
            ? v.sort
            : "newest",
        scrollTop: nonNegative(v.scrollTop),
        lastNumber: positiveInteger(v.lastNumber) ? v.lastNumber : null,
        viewedFiles: Object.fromEntries(
          Object.entries(record(v.viewedFiles))
            .filter(([n]) => positiveInteger(Number(n)))
            .map(([n, paths]) => [n, strings(paths)]),
        ),
        drafts: Object.fromEntries(
          Object.entries(record(v.drafts))
            .filter(([n]) => positiveInteger(Number(n)))
            .map(([n, draft]) => [n, sanitizeDraft(draft)]),
        ),
        lastUsedAt: nonNegative(v.lastUsedAt),
      },
    ]);
  }
  return { byProjectKey: retainMostRecent(Object.fromEntries(entries)) };
}
export const usePullRequestsStore = create<PullRequestsStoreState>()(
  persist(
    (set, get) => {
      const update = (
        ref: ScopedProjectRef,
        fn: (current: PullRequestsViewState) => PullRequestsViewState,
      ) => set((s) => updateProject(s, ref, fn));
      const draft = (
        ref: ScopedProjectRef,
        number: number,
        fn: (current: PullRequestsDraft) => PullRequestsDraft,
      ) =>
        update(ref, (v) => ({
          ...v,
          drafts: { ...v.drafts, [number]: fn(v.drafts[number] ?? DEFAULT_DRAFT) },
        }));
      return {
        byProjectKey: {},
        selectViewState: (ref) =>
          get().byProjectKey[projectKey(ref)] ?? DEFAULT_PULL_REQUESTS_VIEW_STATE,
        selectDraft: (ref, number) => get().selectViewState(ref).drafts[number] ?? DEFAULT_DRAFT,
        touchProject: (ref) => update(ref, (v) => v),
        setCheckoutCwd: (ref, checkoutCwd) => update(ref, (v) => ({ ...v, checkoutCwd })),
        setListTab: (ref, listTab) => update(ref, (v) => ({ ...v, listTab, scrollTop: 0 })),
        setFilters: (ref, filters) =>
          update(ref, (v) => ({ ...v, filters: { ...v.filters, ...filters }, scrollTop: 0 })),
        setSort: (ref, sort) => update(ref, (v) => ({ ...v, sort, scrollTop: 0 })),
        setScrollTop: (ref, scrollTop) =>
          update(ref, (v) => ({ ...v, scrollTop: nonNegative(scrollTop) })),
        setLastNumber: (ref, lastNumber) => update(ref, (v) => ({ ...v, lastNumber })),
        toggleViewedFile: (ref, number, path) =>
          update(ref, (v) => {
            const paths = v.viewedFiles[number] ?? [];
            return {
              ...v,
              viewedFiles: {
                ...v.viewedFiles,
                [number]: paths.includes(path) ? paths.filter((p) => p !== path) : [...paths, path],
              },
            };
          }),
        setCommentDraft: (ref, number, comment) => draft(ref, number, (d) => ({ ...d, comment })),
        setReviewBody: (ref, number, reviewBody) =>
          draft(ref, number, (d) => ({ ...d, reviewBody })),
        setPendingReview: (ref, number, pendingReview) =>
          draft(ref, number, (d) => ({ ...d, pendingReview })),
        setInlineDraft: (ref, number, comment) =>
          draft(ref, number, (d) => ({
            ...d,
            inlineDrafts: d.inlineDrafts.some((c) => c.id === comment.id)
              ? d.inlineDrafts.map((c) => (c.id === comment.id ? comment : c))
              : [...d.inlineDrafts, comment],
          })),
        removeInlineDraft: (ref, number, id) =>
          draft(ref, number, (d) => ({
            ...d,
            inlineDrafts: d.inlineDrafts.filter((c) => c.id !== id),
          })),
        setMergeDraft: (ref, number, mergeSubject, mergeBody) =>
          draft(ref, number, (d) => ({
            ...d,
            mergeSubject: mergeSubject ?? "",
            mergeBody: mergeBody ?? "",
            mergeDraftEdited: mergeSubject !== null || mergeBody !== null,
          })),
        setTitleEdit: (ref, number, titleEdit) => draft(ref, number, (d) => ({ ...d, titleEdit })),
        setBodyEdit: (ref, number, bodyEdit) => draft(ref, number, (d) => ({ ...d, bodyEdit })),
        setCommentEdit: (ref, number, id, body) =>
          draft(ref, number, (d) => {
            const commentEdits = { ...d.commentEdits };
            if (body === null) delete commentEdits[id];
            else commentEdits[id] = body;
            return { ...d, commentEdits };
          }),
        setReplyDraft: (ref, number, threadId, body) =>
          draft(ref, number, (d) => {
            const replyDrafts = { ...d.replyDrafts };
            if (body === null) delete replyDrafts[threadId];
            else replyDrafts[threadId] = body;
            return { ...d, replyDrafts };
          }),
        clearDraft: (ref, number) =>
          update(ref, (v) => {
            const drafts = { ...v.drafts };
            delete drafts[number];
            return { ...v, drafts };
          }),
      };
    },
    {
      name: PULL_REQUESTS_STORAGE_KEY,
      version: 1,
      storage: createJSONStorage(() =>
        resolveStorage(typeof window !== "undefined" ? window.localStorage : undefined),
      ),
      partialize: (state) => ({ byProjectKey: state.byProjectKey }),
      migrate: sanitizePersistedState,
      merge: (persisted, current) => ({ ...current, ...sanitizePersistedState(persisted) }),
    },
  ),
);
