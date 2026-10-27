// @vitest-environment happy-dom
import type { PullRequestsVocabulary } from "@coddy/contracts";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { allowed, button, click, input, mount, projectRef } from "../review/testHelpers";
import { context, detail } from "../detail/testFixtures";
const h = vi.hoisted(() => ({
  vocabulary: vi.fn((request: unknown) => request),
  data: null as PullRequestsVocabulary | null,
  error: null as string | null,
  refresh: vi.fn(),
  toast: vi.fn().mockReturnValue("undo"),
  close: vi.fn(),
}));
vi.mock("../../../state/pullRequests", () => ({
  pullRequestsEnvironment: { getVocabulary: h.vocabulary },
}));
vi.mock("../shared/usePullRequestsQuery", () => ({
  usePullRequestsQuery: () => ({
    data: h.data,
    error: h.error,
    isPending: false,
    refresh: h.refresh,
  }),
}));
vi.mock("../../ui/toast", () => ({ toastManager: { add: h.toast, close: h.close } }));
import { PullRequestsPicker } from "./PullRequestsPicker";
import { PullRequestsSideColumn } from "../detail/PullRequestsSideColumn";
let view: Awaited<ReturnType<typeof mount>>;
const scope = { environmentId: projectRef.environmentId, cwd: "/repo" };
const entry = (label: string, id = label) => ({ id, label, description: null, color: "ff0000" });
beforeEach(() => {
  vi.clearAllMocks();
  h.error = null;
  h.data = { kind: "labels", entries: [entry("bug"), entry("feature")], truncated: false };
});
afterEach(async () => {
  await view?.unmount();
  vi.useRealTimers();
});
async function toggle(label: string) {
  const item = document.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`);
  expect(item).not.toBeNull();
  await act(async () => item!.click());
}
describe("metadata pickers", () => {
  it("searches text entered before the initial truncated response arrives", async () => {
    h.data = null;
    const ui = (
      <PullRequestsPicker
        scope={scope}
        label="Labels"
        kind="labels"
        multiple
        selected={[]}
        permission={allowed}
        onChange={vi.fn()}
      />
    );
    view = await mount(ui);
    await click("Edit Labels");
    vi.useFakeTimers();
    await input(document.querySelector<HTMLInputElement>('[aria-label="Search Labels"]')!, "late");
    await act(async () => vi.advanceTimersByTime(300));
    expect(h.vocabulary).toHaveBeenCalledOnce();
    h.data = { kind: "labels", entries: [], truncated: true };
    await view.render(
      <PullRequestsPicker
        scope={scope}
        label="Labels"
        kind="labels"
        multiple
        selected={[]}
        permission={allowed}
        onChange={vi.fn()}
      />,
    );
    await act(async () => vi.advanceTimersByTime(300));
    expect(h.vocabulary).toHaveBeenLastCalledWith({
      environmentId: "env",
      input: { cwd: "/repo", kind: "labels", query: "late" },
    });
  });
  it("reconciles a fresh server selection even when Undo returns it to the initial values", async () => {
    const onChange = vi.fn().mockResolvedValue(undefined);
    view = await mount(
      <PullRequestsPicker
        scope={scope}
        label="Labels"
        kind="labels"
        multiple
        selected={[]}
        permission={allowed}
        onChange={onChange}
      />,
    );
    await click("Edit Labels");
    await toggle("bug");
    expect(document.querySelector<HTMLInputElement>('input[aria-label="bug"]')?.checked).toBe(true);
    // A new authoritative snapshot after Undo can contain exactly the initial values.
    await view.render(
      <PullRequestsPicker
        scope={scope}
        label="Labels"
        kind="labels"
        multiple
        selected={[]}
        permission={allowed}
        onChange={onChange}
      />,
    );
    expect(document.querySelector<HTMLInputElement>('input[aria-label="bug"]')?.checked).toBe(
      false,
    );
    await toggle("bug");
    expect(onChange).toHaveBeenLastCalledWith(["bug"], []);
  });
  it("loads lazily, filters complete vocabularies locally, and applies exact deltas immediately", async () => {
    const onChange = vi.fn().mockResolvedValue(undefined);
    view = await mount(
      <PullRequestsPicker
        scope={scope}
        label="Labels"
        kind="labels"
        multiple
        selected={["bug"]}
        permission={allowed}
        onChange={onChange}
      />,
    );
    expect(h.vocabulary).not.toHaveBeenCalled();
    await click("Edit Labels");
    expect(h.vocabulary).toHaveBeenCalledExactlyOnceWith({
      environmentId: "env",
      input: { cwd: "/repo", kind: "labels", query: null },
    });
    await input(
      document.querySelector<HTMLInputElement>('[aria-label="Search Labels"]')!,
      "feature",
    );
    expect(document.querySelector('input[aria-label="bug"]')).toBeNull();
    await toggle("feature");
    expect(onChange).toHaveBeenCalledExactlyOnceWith(["feature"], []);
    expect(h.vocabulary).toHaveBeenCalledOnce();
  });
  it("debounces truncated server searches for 300 ms and cancels them when closed", async () => {
    h.data = { ...h.data!, truncated: true };
    view = await mount(
      <PullRequestsPicker
        scope={scope}
        label="Labels"
        kind="labels"
        multiple
        selected={[]}
        permission={allowed}
        onChange={vi.fn()}
      />,
    );
    await click("Edit Labels");
    vi.useFakeTimers();
    const search = document.querySelector<HTMLInputElement>('[aria-label="Search Labels"]')!;
    await input(search, "f");
    await input(search, "feature");
    await act(async () => vi.advanceTimersByTime(299));
    expect(h.vocabulary).toHaveBeenCalledOnce();
    await act(async () => vi.advanceTimersByTime(1));
    expect(h.vocabulary).toHaveBeenLastCalledWith({
      environmentId: "env",
      input: { cwd: "/repo", kind: "labels", query: "feature" },
    });
    await input(search, "cancelled");
    await click("Done");
    const calls = h.vocabulary.mock.calls.length;
    await act(async () => vi.advanceTimersByTime(500));
    expect(h.vocabulary).toHaveBeenCalledTimes(calls);
  });
  it("uses milestone ids verbatim and offers Clear", async () => {
    h.data = { kind: "milestones", entries: [entry("Release next", "42")], truncated: false };
    const onChange = vi.fn().mockResolvedValue(undefined);
    view = await mount(
      <PullRequestsPicker
        scope={scope}
        label="Milestone"
        kind="milestones"
        multiple={false}
        selected={["7"]}
        permission={allowed}
        onChange={onChange}
      />,
    );
    await click("Edit Milestone");
    await toggle("Release next");
    expect(onChange).toHaveBeenLastCalledWith(["42"], ["7"]);
    await click("Clear");
    expect(onChange).toHaveBeenLastCalledWith([], ["42"]);
  });
  it("keeps selected values on failure, displays the exact error, and allows retry", async () => {
    const onChange = vi.fn().mockRejectedValue({ message: "Labels changed on host" });
    view = await mount(
      <PullRequestsPicker
        scope={scope}
        label="Labels"
        kind="labels"
        multiple
        selected={["bug"]}
        permission={allowed}
        onChange={onChange}
      />,
    );
    await click("Edit Labels");
    await toggle("bug");
    expect(document.querySelector<HTMLInputElement>('input[aria-label="bug"]')?.checked).toBe(true);
    expect(document.querySelector('[role="alert"]')?.textContent).toBe("Labels changed on host");
    onChange.mockResolvedValue(undefined);
    await toggle("bug");
    expect(onChange).toHaveBeenLastCalledWith([], ["bug"]);
  });
  it("shows vocabulary read errors with Retry", async () => {
    h.data = null;
    h.error = "Sign in again";
    view = await mount(
      <PullRequestsPicker
        scope={scope}
        label="Labels"
        kind="labels"
        multiple
        selected={[]}
        permission={allowed}
        onChange={vi.fn()}
      />,
    );
    await click("Edit Labels");
    expect(document.querySelector('[role="alert"]')?.textContent).toBe("Sign in again");
    await click("Retry");
    expect(h.refresh).toHaveBeenCalledOnce();
  });
  it.each([
    ["Reviewers", "setReviewers", "users"],
    ["Assignees", "setAssignees", "users"],
    ["Labels", "setLabels", "labels"],
  ] as const)(
    "wires %s to a reversible host action and excludes the author from reviewers",
    async (label, action, kind) => {
      h.data = {
        kind,
        entries: [entry("someone", "100"), entry(detail.author.login, "200")],
        truncated: false,
      };
      view = await mount(
        <PullRequestsSideColumn
          detail={{
            ...detail,
            permissions: {
              ...detail.permissions,
              editReviewers: allowed,
              editAssignees: allowed,
              editLabels: allowed,
            },
          }}
          context={context}
          scope={scope}
          projectRef={projectRef}
        />,
      );
      await click(`Edit ${label}`);
      if (kind === "users" && label === "Reviewers")
        expect(document.querySelector(`input[aria-label="${detail.author.login}"]`)).toBeNull();
      await toggle("someone");
      expect(view.run).toHaveBeenCalledWith({ action, add: ["someone"], remove: [] });
      expect(h.toast).toHaveBeenCalledOnce();
      const undo = h.toast.mock.calls[0]![0];
      expect(undo.timeout).toBe(5000);
      await act(async () => undo.actionProps.onClick());
      expect(view.run).toHaveBeenLastCalledWith(
        { action, add: [], remove: ["someone"] },
        { waitForPending: true },
      );
    },
  );
  it("wires milestone set/clear and restores the prior numeric id with Undo", async () => {
    h.data = { kind: "milestones", entries: [entry("Next", "42")], truncated: false };
    view = await mount(
      <PullRequestsSideColumn
        detail={{
          ...detail,
          milestone: { id: "7", title: "Previous", dueOn: null },
          permissions: { ...detail.permissions, editMilestone: allowed },
        }}
        context={context}
        scope={scope}
        projectRef={projectRef}
      />,
    );
    await click("Edit Milestone");
    await toggle("Next");
    expect(view.run).toHaveBeenLastCalledWith({ action: "setMilestone", milestoneId: "42" });
    await act(async () => h.toast.mock.calls[0]![0].actionProps.onClick());
    expect(view.run).toHaveBeenLastCalledWith(
      { action: "setMilestone", milestoneId: "7" },
      { waitForPending: true },
    );
  });
  it("explains denied controls without fetching", async () => {
    view = await mount(
      <PullRequestsPicker
        scope={scope}
        label="Labels"
        kind="labels"
        multiple
        selected={[]}
        permission={detail.permissions.editLabels}
        onChange={vi.fn()}
      />,
    );
    expect(button("Edit Labels").disabled).toBe(true);
    expect(button("Edit Labels").title).toBe(detail.permissions.editLabels.reason);
    expect(h.vocabulary).not.toHaveBeenCalled();
  });
});
