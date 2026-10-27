import type { PullRequestsMergeReadiness } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";
import {
  approvalStatusNote,
  checksSummaryLabel,
  fileTree,
  groupTimeline,
  headerSentence,
  readinessPresentation,
  patchWithoutWhitespaceChanges,
} from "./pullRequestsDetail.logic";
import {
  comment,
  context,
  detail,
  event,
  file,
  gitlabContext,
  review,
  thread,
} from "./testFixtures";
describe("headerSentence", () => {
  it("uses GitHub's author, count and base/head order", () =>
    expect(headerSentence(detail, context.provider)).toBe(
      "mubeda wants to merge 3 commits into main from feature",
    ));
  it("uses GitLab's source/target order", () =>
    expect(headerSentence(detail, gitlabContext.provider)).toBe(
      "mubeda requested to merge feature into main",
    ));
  it("uses singular commit", () =>
    expect(headerSentence({ ...detail, commitCount: 1 }, context.provider)).toContain(
      "1 commit into",
    ));
});
describe("approvalStatusNote", () => {
  it("says approval is optional when the host requires none", () => {
    expect(approvalStatusNote(null)).toBe("Approval is optional");
    expect(approvalStatusNote({ approved: 0, required: 0 })).toBe("Approval is optional");
  });
  it("counts the approvals the host already has against the number it requires", () => {
    expect(approvalStatusNote({ approved: 1, required: 2 })).toBe("1 of 2 approvals");
  });
});
describe("readinessPresentation", () => {
  it.each(["1 approving review required.", "  1 approving review required.\n"])(
    "shows a lone blocking reason only once: %j",
    (line) => {
      const readiness = {
        ...detail.readiness,
        summary: " 1 approving review required. ",
        details: [line],
      };
      expect(readinessPresentation(readiness, context.capabilities.vocabulary)).toEqual({
        tone: "warning",
        title: readiness.summary,
        lines: [],
      });
      expect(readiness.details).toEqual([line]);
    },
  );
  it("removes repeated titles while preserving other detail lines verbatim and in order", () => {
    const readiness = {
      ...detail.readiness,
      details: ["Waiting for Alice", " 1 approving review required. ", " Checks must pass. "],
    };
    expect(readinessPresentation(readiness, context.capabilities.vocabulary).lines).toEqual([
      "Waiting for Alice",
      " Checks must pass. ",
    ]);
  });
  it("keeps all detail lines when none repeats the title", () => {
    const readiness = {
      ...detail.readiness,
      details: ["Waiting for Alice", " Checks must pass. "],
    };
    expect(readinessPresentation(readiness, context.capabilities.vocabulary).lines).toEqual(
      readiness.details,
    );
  });
  it.each<[PullRequestsMergeReadiness["status"], string]>([
    ["mergeable", "success"],
    ["checks_pending", "warning"],
    ["checks_failing", "danger"],
    ["review_required", "warning"],
    ["changes_requested", "danger"],
    ["conflicts", "danger"],
    ["behind", "warning"],
    ["blocked", "warning"],
    ["draft", "neutral"],
    ["merged", "success"],
    ["closed", "neutral"],
    ["unknown", "neutral"],
  ])("presents %s without changing the server's explanation", (status, tone) => {
    expect(
      readinessPresentation({ ...detail.readiness, status }, context.capabilities.vocabulary),
    ).toEqual({
      tone,
      title: "1 approving review required.",
      lines: ["0 of 1 required approvals"],
    });
  });
});
describe("groupTimeline", () => {
  it("stable sorts created/submitted times and preserves thread comment order and opaque ids", () => {
    const laterFirst = {
      ...thread,
      comments: [
        { ...thread.comments[0]!, id: "ds:2", createdAt: "2026-09-20T11:00:00Z" },
        { ...thread.comments[0]!, id: "nt:1" },
      ],
    };
    const items = [review, laterFirst, comment, event, { ...event, id: "ev:same" }];
    const grouped = groupTimeline(items);
    expect(grouped.map((item) => item.id)).toEqual(["ev:17", "ev:same", "ic:17", "th:17", "rv:17"]);
    expect(grouped[3]).toBe(laterFirst);
    expect(items[0]).toBe(review);
    expect(laterFirst.comments.map((item) => item.id)).toEqual(["ds:2", "nt:1"]);
  });
});
describe("checksSummaryLabel", () => {
  it.each([
    ["none", "No checks reported"],
    ["success", "All checks passed"],
    ["pending", "Checks pending"],
    ["failure", "Checks failed"],
    ["neutral", "Checks completed"],
  ] as const)("summarizes %s", (summary, label) =>
    expect(checksSummaryLabel({ summary, groups: [], pipelineUrl: null })).toBe(label),
  );
});
describe("fileTree", () => {
  it("groups nested folder rows and preserves the file metadata", () => {
    const files = [{ ...file, path: "src/nested/z.ts" }, { ...file, path: "README.md" }, file];
    const rows = fileTree(files);
    expect(rows.map(({ kind, path, depth }) => [kind, path, depth])).toEqual([
      ["folder", "src", 0],
      ["folder", "src/nested", 1],
      ["file", "src/nested/z.ts", 2],
      ["file", "src/a.ts", 1],
      ["file", "README.md", 0],
    ]);
    expect(rows.find((row) => row.kind === "file" && row.path === file.path)).toMatchObject({
      file,
    });
  });
});
describe("patchWithoutWhitespaceChanges", () => {
  it("handles a bounded host patch containing hundreds of thousands of short added lines", () => {
    const patch = `--- /dev/null\n+++ b/x\n@@ -0,0 +1,180000 @@\n${"+x\n".repeat(180000)}`;
    expect(patchWithoutWhitespaceChanges(patch)).toBe(patch);
  });
  it("turns equal line pairs into context while keeping real changes and source coordinates", () => {
    const patch = "--- a/x\n+++ b/x\n@@ -40,2 +60,2 @@\n-  same\n-old\n+same\n+new\n";
    expect(patchWithoutWhitespaceChanges(patch)).toBe(
      "--- a/x\n+++ b/x\n@@ -40,2 +60,2 @@\n same\n-old\n+new\n",
    );
  });
  it("retains unequal blocks and no-newline markers conservatively", () => {
    const patch =
      "--- a/x\n+++ b/x\n@@ -1 +1,2 @@\n-a\n+ a\n+new\n@@ -9 +10 @@\n-b\n\\ No newline at end of file\n+ b\n";
    expect(patchWithoutWhitespaceChanges(patch)).toBe(patch);
  });
});
