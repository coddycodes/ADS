// @vitest-environment happy-dom
import type { PullRequestsDetail, PullRequestsMergeMethod } from "@coddy/contracts";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vite-plus/test";
import { usePullRequestsStore } from "../../../pullRequestsStore";
import { allowed, button, click, input, mockRun, mount, projectRef } from "../review/testHelpers";
import { context, detail, gitlabContext } from "./testFixtures";
import { PullRequestsMergeBox } from "./PullRequestsMergeBox";
let view: Awaited<ReturnType<typeof mount>>;
const mergeable: PullRequestsDetail = {
  ...detail,
  permissions: {
    ...detail.permissions,
    merge: { ...detail.permissions.merge, ...allowed },
    mergeBypass: allowed,
    enableAutoMerge: allowed,
  },
  readiness: {
    ...detail.readiness,
    status: "mergeable",
    headSha: "readiness-is-not-the-loaded-head",
  },
};
const names = { merge: "Merge commit", squash: "Squash and merge", rebase: "Rebase and merge" };
beforeEach(() => usePullRequestsStore.setState({ byProjectKey: {} }));
afterEach(async () => {
  await view?.unmount();
});
async function method(value: PullRequestsMergeMethod) {
  await click("Merge method");
  const option = [...document.querySelectorAll<HTMLElement>('[role="option"]')].find(
    (o) => o.textContent?.trim() === names[value],
  );
  expect(option).toBeDefined();
  await act(async () => option!.click());
}
async function checkbox(label: string) {
  await act(async () =>
    document.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`)!.click(),
  );
}
describe("merge controls", () => {
  it.each([
    { host: context, title: "Merge pull request" },
    { host: gitlabContext, title: "Merge merge request" },
  ] as const)("uses the host vocabulary for $title", async ({ host, title }) => {
    view = await mount(
      <PullRequestsMergeBox detail={mergeable} context={host} projectRef={projectRef} />,
      mockRun(),
      host.capabilities.vocabulary.pullRequest,
    );
    await click("Merge");
    const dialog = document.querySelector('[role="alertdialog"]')!;
    const titleId = dialog.getAttribute("aria-labelledby")!;
    expect(document.getElementById(titleId)?.textContent).toBe(title);
    expect(view.run).not.toHaveBeenCalled();
  });
  it("uses GitLab vocabulary for merge readiness and stale-head recovery copy", async () => {
    view = await mount(
      <PullRequestsMergeBox
        detail={{ ...mergeable, headSha: "" }}
        context={gitlabContext}
        projectRef={projectRef}
      />,
      mockRun(),
      gitlabContext.capabilities.vocabulary.pullRequest,
    );
    expect(button("Merge").title).toBe("Reload this merge request before merging");
    await view.render(
      <PullRequestsMergeBox detail={mergeable} context={gitlabContext} projectRef={projectRef} />,
    );
    view.run.mockRejectedValueOnce({ code: "stale_head", message: "Head moved" });
    await click("Merge");
    await click("Merge", document.querySelector('[role="alertdialog"]')!);
    expect(document.querySelector('[role="alert"]')?.textContent).toBe(
      "This merge request changed; reload and try again",
    );
  });
  it.each([false, true])(
    "rechecks both GitLab permissions after auto selection, with confirmation open=%s",
    async (confirming) => {
      view = await mount(
        <PullRequestsMergeBox detail={mergeable} context={gitlabContext} projectRef={projectRef} />,
      );
      await checkbox(gitlabContext.capabilities.autoMergeLabel);
      if (confirming) await click("Merge despite requested changes");
      await view.render(
        <PullRequestsMergeBox
          detail={{
            ...mergeable,
            permissions: {
              ...mergeable.permissions,
              enableAutoMerge: { allowed: false, reason: "Auto-merge permission revoked" },
            },
          }}
          context={gitlabContext}
          projectRef={projectRef}
        />,
      );
      const parent = confirming ? document.querySelector('[role="alertdialog"]')! : document;
      expect(button("Merge despite requested changes", parent).disabled).toBe(true);
      expect(button("Merge despite requested changes", parent).title).toBe(
        "Auto-merge permission revoked",
      );
      await click("Merge despite requested changes", parent);
      expect(view.run).not.toHaveBeenCalled();
    },
  );
  it.each(["merge", "squash", "rebase"] as const)(
    "confirms %s and pins exactly the head the user reviewed",
    async (selected) => {
      view = await mount(
        <PullRequestsMergeBox detail={mergeable} context={context} projectRef={projectRef} />,
      );
      await method(selected);
      await click("Merge");
      const dialog = document.querySelector('[role="alertdialog"]')!;
      expect(dialog.textContent).toContain(names[selected]);
      expect(dialog.textContent).toContain("main");
      expect(dialog.textContent).toContain("Delete branch: Yes");
      expect(dialog.textContent).toContain("Auto-merge: No");
      expect(view.run).not.toHaveBeenCalled();
      await click("Merge", dialog);
      expect(view.run).toHaveBeenCalledExactlyOnceWith({
        action: "merge",
        method: selected,
        deleteBranch: true,
        auto: false,
        bypass: false,
        headSha: "head-sha",
        subject: selected === "rebase" ? null : `${detail.title} (#14)`,
        body: null,
      });
    },
  );
  it("starts with the server method/deletion defaults and hides a single or project-fixed method select", async () => {
    view = await mount(
      <PullRequestsMergeBox detail={mergeable} context={context} projectRef={projectRef} />,
    );
    expect(button("Merge method").textContent).toContain("Squash and merge");
    expect(
      document.querySelector<HTMLInputElement>('[aria-label="Delete branch after merge"]')?.checked,
    ).toBe(true);
    await view.render(
      <PullRequestsMergeBox
        detail={{
          ...mergeable,
          permissions: {
            ...mergeable.permissions,
            merge: { ...mergeable.permissions.merge, methods: ["squash"] },
          },
        }}
        context={context}
        projectRef={projectRef}
      />,
    );
    expect(document.querySelector('[aria-label="Merge method"]')).toBeNull();
    await view.render(
      <PullRequestsMergeBox detail={mergeable} context={gitlabContext} projectRef={projectRef} />,
    );
    expect(document.querySelector('[aria-label="Merge method"]')).toBeNull();
    expect(view.container.textContent).toContain("Project merge method");
    expect(document.querySelector<HTMLInputElement>('[aria-label="Merge subject"]')?.value).toBe(
      "",
    );
  });
  it("preserves edited and deliberately blank drafts after cancel, failure and remount", async () => {
    const ui = (
      <PullRequestsMergeBox detail={mergeable} context={context} projectRef={projectRef} />
    );
    view = await mount(ui);
    await input(document.querySelector<HTMLInputElement>('[aria-label="Merge subject"]')!, "");
    await input(
      document.querySelector<HTMLTextAreaElement>('[aria-label="Merge body"]')!,
      "Keep merge body",
    );
    await click("Merge");
    await click("Cancel");
    await view.unmount();
    view = await mount(ui);
    expect(document.querySelector<HTMLInputElement>('[aria-label="Merge subject"]')?.value).toBe(
      "",
    );
    view.run.mockRejectedValueOnce({ code: "stale_head", message: "Moved" });
    await click("Merge");
    await click("Merge", document.querySelector('[role="alertdialog"]')!);
    expect(document.querySelector('[role="alert"]')?.textContent).toBe(
      "This pull request changed; reload and try again",
    );
    expect(view.run).toHaveBeenCalledOnce();
    expect(usePullRequestsStore.getState().selectDraft(projectRef, 14).mergeBody).toBe(
      "Keep merge body",
    );
  });
  it("freezes confirmation details until submission even if detail refreshes", async () => {
    view = await mount(
      <PullRequestsMergeBox detail={mergeable} context={context} projectRef={projectRef} />,
    );
    await click("Merge");
    await view.render(
      <PullRequestsMergeBox
        detail={{ ...mergeable, headSha: "new-head", baseBranch: "changed-base" }}
        context={context}
        projectRef={projectRef}
      />,
    );
    const dialog = document.querySelector('[role="alertdialog"]')!;
    expect(dialog.textContent).toContain("main");
    expect(dialog.textContent).not.toContain("changed-base");
    await click("Merge", dialog);
    expect(view.run).toHaveBeenCalledWith(expect.objectContaining({ headSha: "head-sha" }));
  });
  it("uses the auto-merge permission when immediate merging is blocked and disables GitHub bypass with a reason", async () => {
    view = await mount(
      <PullRequestsMergeBox
        detail={{
          ...mergeable,
          permissions: {
            ...mergeable.permissions,
            merge: { ...mergeable.permissions.merge, allowed: false, reason: "Checks pending" },
          },
        }}
        context={context}
        projectRef={projectRef}
      />,
    );
    expect(button("Merge").disabled).toBe(true);
    await checkbox(context.capabilities.autoMergeLabel);
    expect(button("Merge").disabled).toBe(false);
    const bypass = button("Merge without waiting for requirements");
    expect(bypass.disabled).toBe(true);
    expect(bypass.title).toBe("Auto-merge and bypass cannot be combined on GitHub.");
    await click("Merge");
    expect(document.querySelector('[role="alertdialog"]')?.textContent).toContain(
      "Auto-merge: Yes",
    );
    await click("Enable auto-merge", document.querySelector('[role="alertdialog"]')!);
    expect(view.run).toHaveBeenCalledWith(expect.objectContaining({ auto: true, bypass: false }));
  });
  it.each([context, gitlabContext])(
    "names the bypass consequence for $provider and supports GitLab auto plus bypass",
    async (host) => {
      view = await mount(
        <PullRequestsMergeBox detail={mergeable} context={host} projectRef={projectRef} />,
      );
      if (host.provider === "gitlab") await checkbox(host.capabilities.autoMergeLabel);
      const label =
        host.provider === "github"
          ? "Merge without waiting for requirements"
          : "Merge despite requested changes";
      await click(label);
      const dialog = document.querySelector('[role="alertdialog"]')!;
      expect(dialog.textContent).toContain(label);
      expect(dialog.textContent).toContain("main");
      expect(view.run).not.toHaveBeenCalled();
      await click(label, dialog);
      expect(view.run).toHaveBeenCalledWith(
        expect.objectContaining({
          auto: host.provider === "gitlab",
          bypass: true,
          headSha: "head-sha",
        }),
      );
    },
  );
  it("disables active auto-merge through the shared action", async () => {
    view = await mount(
      <PullRequestsMergeBox
        detail={{
          ...mergeable,
          permissions: { ...mergeable.permissions, disableAutoMerge: allowed },
          readiness: { ...mergeable.readiness, autoMerge: { enabled: true, method: "squash" } },
        }}
        context={context}
        projectRef={projectRef}
      />,
    );
    await click("Disable auto-merge");
    expect(view.run).toHaveBeenCalledExactlyOnceWith({ action: "disableAutoMerge" });
  });
  it.each(["merge", "rebase"] as const)(
    "updates a behind GitHub branch using %s",
    async (method) => {
      view = await mount(
        <PullRequestsMergeBox
          detail={{
            ...mergeable,
            permissions: {
              ...mergeable.permissions,
              updateBranch: { ...allowed, methods: [method] },
            },
            readiness: { ...mergeable.readiness, status: "behind" },
          }}
          context={context}
          projectRef={projectRef}
        />,
      );
      await click(method === "merge" ? "Update branch" : "Rebase");
      expect(view.run).toHaveBeenCalledWith({ action: "updateBranch", method, skipCi: false });
    },
  );
  it("offers both GitHub update methods in a split button", async () => {
    view = await mount(
      <PullRequestsMergeBox
        detail={{
          ...mergeable,
          permissions: {
            ...mergeable.permissions,
            updateBranch: { ...allowed, methods: ["merge", "rebase"] },
          },
          readiness: { ...mergeable.readiness, status: "behind" },
        }}
        context={context}
        projectRef={projectRef}
      />,
    );
    await click("Update branch options");
    await click("Rebase");
    expect(view.run).toHaveBeenCalledWith({
      action: "updateBranch",
      method: "rebase",
      skipCi: false,
    });
  });
  it("offers GitLab Rebase with optional Skip CI", async () => {
    view = await mount(
      <PullRequestsMergeBox
        detail={{
          ...mergeable,
          permissions: {
            ...mergeable.permissions,
            updateBranch: { ...allowed, methods: ["rebase"] },
          },
          readiness: { ...mergeable.readiness, status: "behind" },
        }}
        context={gitlabContext}
        projectRef={projectRef}
      />,
    );
    await checkbox("Skip CI");
    await click("Rebase");
    expect(view.run).toHaveBeenCalledWith({
      action: "updateBranch",
      method: "rebase",
      skipCi: true,
    });
  });
  it("prevents duplicate confirmed merges and clears matching drafts only on success", async () => {
    let finish!: (result: { kind: "merged"; mergedSha: null; autoMergeEnabled: false }) => void;
    view = await mount(
      <PullRequestsMergeBox detail={mergeable} context={context} projectRef={projectRef} />,
    );
    await input(
      document.querySelector<HTMLInputElement>('[aria-label="Merge subject"]')!,
      "Custom subject",
    );
    view.run.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    await click("Merge");
    const dialog = document.querySelector('[role="alertdialog"]')!;
    const confirm = button("Merge", dialog);
    await act(async () => {
      confirm.click();
      confirm.click();
    });
    expect(view.run).toHaveBeenCalledOnce();
    expect(button("Cancel", dialog).disabled).toBe(true);
    await act(async () => finish({ kind: "merged", mergedSha: null, autoMergeEnabled: false }));
    expect(usePullRequestsStore.getState().selectDraft(projectRef, 14).mergeDraftEdited).toBe(
      false,
    );
  });
  it("shows partial-progress errors verbatim and keeps the confirmation open without retry", async () => {
    view = await mount(
      <PullRequestsMergeBox detail={mergeable} context={gitlabContext} projectRef={projectRef} />,
    );
    view.run.mockRejectedValue({
      code: "host_error",
      message: "Override applied; merge failed. Refresh before trying again.",
    });
    await click("Merge despite requested changes");
    await click("Merge despite requested changes", document.querySelector('[role="alertdialog"]')!);
    expect(document.querySelector('[role="alert"]')?.textContent).toBe(
      "Override applied; merge failed. Refresh before trying again.",
    );
    expect(view.run).toHaveBeenCalledOnce();
  });
});
