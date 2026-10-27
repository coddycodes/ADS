// @vitest-environment happy-dom
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { PullRequestsOperationError, type PullRequestsPermissions } from "@coddy/contracts";
import { usePullRequestsStore, type PendingInlineComment } from "../../../pullRequestsStore";
import { context, detail } from "../detail/testFixtures";
import { allowed, button, click, input, mount, projectRef, mockRun } from "./testHelpers";
const h = vi.hoisted(() => ({ toast: vi.fn() }));
vi.mock("../../ui/toast", () => ({ toastManager: { add: h.toast } }));
import { PullRequestsPendingReviewBar } from "./PullRequestsPendingReviewBar";
let view: Awaited<ReturnType<typeof mount>>;
const first: PendingInlineComment = {
  id: "a",
  path: "a.ts",
  line: 4,
  startLine: null,
  side: "right",
  body: "First",
};
const second: PendingInlineComment = { ...first, id: "b", line: 5, body: "Second" };
const grants = {
  ...detail.permissions,
  review: allowed,
  approve: allowed,
  requestChanges: allowed,
  revokeApproval: allowed,
  removeOwnChangeRequest: allowed,
};
const ui = (permissions: PullRequestsPermissions = grants) => (
  <PullRequestsPendingReviewBar
    detail={{ ...detail, permissions }}
    context={context}
    projectRef={projectRef}
  />
);
const pending = () =>
  usePullRequestsStore.getState().selectDraft(projectRef, detail.number).pendingReview;
beforeEach(() => {
  usePullRequestsStore.setState({ byProjectKey: {} });
  usePullRequestsStore.getState().setPendingReview(projectRef, detail.number, [first, second]);
  h.toast.mockClear();
});
afterEach(async () => {
  await view?.unmount();
});
describe("PullRequestsReviewPopover", () => {
  it("retries only failed comments after a posted review without reposting summary or approval", async () => {
    view = await mount(
      ui(),
      mockRun().mockResolvedValue({
        kind: "reviewSubmitted",
        reviewPosted: true,
        landed: 1,
        failed: [
          { path: second.path, line: second.line, body: second.body, message: "Invalid position" },
        ],
      }),
    );
    await click("Review · 2 pending comments");
    await input(
      document.querySelector('textarea[aria-label="Review summary"]')!,
      "Approved summary",
    );
    await click(context.capabilities.vocabulary.approve);
    await click("Submit review");
    expect(button("Comment").getAttribute("aria-checked")).toBe("true");
    await click("Submit review");
    expect(view.run).toHaveBeenLastCalledWith({
      action: "submitReview",
      event: "comment",
      body: null,
      headSha: detail.headSha,
      comments: [
        { path: second.path, line: second.line, startLine: null, side: "right", body: second.body },
      ],
    });
  });
  it.each([false, true])(
    "clears only an already-posted summary on partial receipt reviewPosted=%s",
    async (reviewPosted) => {
      view = await mount(
        ui(),
        mockRun().mockResolvedValue({
          kind: "reviewSubmitted",
          reviewPosted,
          landed: 0,
          failed: [first, second].map((c) => ({
            path: c.path,
            line: c.line,
            body: c.body,
            message: "Position invalid",
          })),
        }),
      );
      await click("Review · 2 pending comments");
      await input(document.querySelector('textarea[aria-label="Review summary"]')!, "Summary");
      await click("Submit review");
      expect(
        document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Review summary"]')?.value,
      ).toBe(reviewPosted ? "" : "Summary");
      expect(pending()).toEqual([first, second]);
    },
  );
  it("submits pending comments once with loaded head SHA and clears on complete success", async () => {
    const run = mockRun().mockResolvedValue({
      kind: "reviewSubmitted",
      reviewPosted: true,
      landed: 2,
      failed: [],
    });
    view = await mount(ui(), run);
    await click("Review · 2 pending comments");
    await input(document.querySelector('textarea[aria-label="Review summary"]')!, "Summary");
    await click(context.capabilities.vocabulary.approve);
    await click("Submit review");
    expect(run).toHaveBeenCalledWith({
      action: "submitReview",
      event: "approve",
      body: "Summary",
      headSha: detail.headSha,
      comments: [
        expect.objectContaining({ path: "a.ts", line: 4, body: "First" }),
        expect.objectContaining({ line: 5, body: "Second" }),
      ],
    });
    expect(run.mock.calls[0]![0]).not.toHaveProperty("comments.0.id");
    expect(pending()).toEqual([]);
    expect(h.toast).toHaveBeenCalledWith(expect.objectContaining({ title: "Review submitted" }));
    expect(
      document.querySelector('[data-slot="popover-popup"] [aria-label="Review summary"]'),
    ).toBeNull();
  });
  it.each([0, 1])("keeps exactly failures and the popover open with %s landed", async (landed) => {
    const failed = (landed === 0 ? [first, second] : [second]).map((c) => ({
      path: c.path,
      line: c.line,
      body: c.body,
      message: `Invalid position ${c.line}`,
    }));
    view = await mount(
      ui(),
      mockRun().mockResolvedValue({ kind: "reviewSubmitted", reviewPosted: true, landed, failed }),
    );
    await click("Review · 2 pending comments");
    await click("Submit review");
    expect(pending()).toEqual(landed === 0 ? [first, second] : [second]);
    expect(document.querySelector('textarea[aria-label="Review summary"]')).not.toBeNull();
    expect(h.toast).toHaveBeenCalledWith(
      expect.objectContaining({
        title: `${failed.length} of 2 comments could not be posted`,
        description: expect.stringContaining("Invalid position 5"),
      }),
    );
    expect(document.querySelector('[role="alert"]')?.textContent).toContain("Invalid position 5");
  });
  it("stale_head preserves every pending comment and the summary", async () => {
    const error = new PullRequestsOperationError({
      operation: "submitReview",
      code: "stale_head",
      message: "Head changed",
      hostDetail: null,
      retryable: true,
    });
    view = await mount(ui(), mockRun().mockRejectedValue(error));
    await click("Review · 2 pending comments");
    await input(document.querySelector('textarea[aria-label="Review summary"]')!, "Keep summary");
    await click("Submit review");
    expect(pending()).toEqual([first, second]);
    expect(
      document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Review summary"]')?.value,
    ).toBe("Keep summary");
  });
  it("persists review summary across popover close and remount", async () => {
    view = await mount(ui());
    await click("Review · 2 pending comments");
    await input(document.querySelector('textarea[aria-label="Review summary"]')!, "Unsent summary");
    await click("Close review");
    await view.render(null);
    await act(async () => {
      await usePullRequestsStore.persist.rehydrate();
    });
    await view.render(ui());
    await click("Review · 2 pending comments");
    expect(
      document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Review summary"]')?.value,
    ).toBe("Unsent summary");
  });
  it("shows all host-disabled review actions with their server reasons and uses separate revoke/remove actions", async () => {
    const denial = { allowed: false, reason: "You cannot approve your own pull request" };
    view = await mount(
      ui({
        ...grants,
        approve: denial,
        requestChanges: denial,
        revokeApproval: { allowed: false, reason: "GitHub cannot revoke approvals" },
      }),
    );
    await click("Review · 2 pending comments");
    expect(button(context.capabilities.vocabulary.approve).title).toBe(denial.reason);
    expect(button(context.capabilities.vocabulary.requestChanges).title).toBe(denial.reason);
    expect(button("Revoke approval").disabled).toBe(true);
    expect(button("Revoke approval").title).toBe("GitHub cannot revoke approvals");
    await click("Remove my change request");
    expect(view.run).toHaveBeenCalledWith({ action: "removeOwnChangeRequest" });
    expect(pending()).toEqual([first, second]);
    await view.render(ui());
    await click("Revoke approval");
    expect(view.run).toHaveBeenCalledWith({ action: "revokeApproval" });
  });
});
