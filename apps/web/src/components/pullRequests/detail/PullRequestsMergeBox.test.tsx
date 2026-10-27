// @vitest-environment happy-dom
import type { PullRequestsMergeReadiness } from "@coddy/contracts";
import { projectRef, renderReviewMarkup as renderToStaticMarkup } from "../review/testHelpers";
import { describe, expect, it } from "vite-plus/test";
import { PullRequestsMergeBox } from "./PullRequestsMergeBox";
import { context, detail } from "./testFixtures";
describe("PullRequestsMergeBox", () => {
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
  ])("renders %s tone and the server's exact title and lines", (status, tone) => {
    const html = renderToStaticMarkup(
      <PullRequestsMergeBox
        projectRef={projectRef}
        detail={{ ...detail, readiness: { ...detail.readiness, status } }}
        context={context}
      />,
    );
    expect(html).toContain(`data-tone="${tone}"`);
    expect(html).toContain("1 approving review required.");
    expect(html).toContain("0 of 1 required approvals");
    if (status === "conflicts") expect(html).toContain("Checkout");
  });
  it("shows the server denial without a method selector or irrelevant merge inputs", () => {
    const container = document.createElement("div");
    container.innerHTML = renderToStaticMarkup(
      <PullRequestsMergeBox
        projectRef={projectRef}
        detail={{
          ...detail,
          permissions: {
            ...detail.permissions,
            merge: { ...detail.permissions.merge, methods: [], defaultMethod: null },
          },
        }}
        context={context}
      />,
    );
    expect(container.querySelector("select, [aria-label='Merge method']")).toBeNull();
    expect(container.textContent).not.toContain("No default merge method");
    expect(container.textContent).toContain(detail.permissions.merge.reason);
    expect(container.querySelector("button")?.disabled).toBe(true);
    expect(container.textContent).not.toContain("Merge without waiting");
  });
  it("shows merged author/time and Revert, and closed requests have no merge controls", () => {
    const merged = renderToStaticMarkup(
      <PullRequestsMergeBox
        projectRef={projectRef}
        detail={{
          ...detail,
          state: "merged",
          mergedBy: detail.author,
          mergedAt: "2026-09-20T12:00:00Z",
          readiness: { ...detail.readiness, status: "merged" },
        }}
        context={context}
      />,
    );
    expect(merged).toContain("Merged by mubeda");
    expect(merged).toContain('dateTime="2026-09-20T12:00:00Z"');
    expect(merged).toContain(">Revert<");
    expect(merged).not.toContain("<select");
    const closed = renderToStaticMarkup(
      <PullRequestsMergeBox
        projectRef={projectRef}
        detail={{
          ...detail,
          state: "closed",
          readiness: { ...detail.readiness, status: "closed" },
        }}
        context={context}
      />,
    );
    expect(closed).toContain("Closed");
    expect(closed).not.toContain("<select");
  });
});
