// @vitest-environment happy-dom
import type { PullRequestsDetail } from "@coddy/contracts";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { allowed, button, click, mount, projectRef } from "../review/testHelpers";
import { PullRequestsHeader } from "./PullRequestsHeader";
import { PullRequestsMergeBox } from "./PullRequestsMergeBox";
import { context, detail, gitlabContext } from "./testFixtures";
const h = vi.hoisted(() => ({
  copy: vi.fn().mockResolvedValue(undefined),
  open: vi.fn().mockResolvedValue(undefined),
  toast: vi.fn().mockReturnValue("undo"),
  close: vi.fn(),
}));
vi.mock("../../../hooks/useCopyToClipboard", () => ({ writeTextToClipboard: h.copy }));
vi.mock("../../../localApi", () => ({ readLocalApi: () => ({ shell: { openExternal: h.open } }) }));
vi.mock("../../ui/toast", () => ({ toastManager: { add: h.toast, close: h.close } }));
vi.mock("@tanstack/react-router", () => ({ useNavigate: () => vi.fn() }));
vi.mock("../../../state/use-atom-command", () => ({ useAtomCommand: () => vi.fn() }));
vi.mock("../../../state/query", () => ({
  useEnvironmentQuery: () => ({ data: null, refresh: vi.fn(), error: null, isPending: false }),
}));
let view: Awaited<ReturnType<typeof mount>>;
beforeEach(() => vi.clearAllMocks());
afterEach(async () => {
  await view?.unmount();
});
const editable: PullRequestsDetail = {
  ...detail,
  permissions: {
    ...detail.permissions,
    markReady: allowed,
    convertToDraft: allowed,
    lock: allowed,
    unlock: allowed,
    close: allowed,
    reopen: allowed,
    delete: allowed,
    revert: allowed,
  },
};
function header(d = editable, host = context) {
  return (
    <PullRequestsHeader
      detail={d}
      context={host}
      projectRef={projectRef}
      scope={{ environmentId: projectRef.environmentId, cwd: "/repo" }}
      onRefresh={() => {}}
      refreshing={false}
    />
  );
}
describe("secondary request actions", () => {
  it.each([
    ["merged", false, "Revert"],
    ["merged", true, "Revert"],
    ["closed", false, "Reopen"],
    ["closed", true, "Reopen"],
  ] as const)(
    "omits draft actions for %s requests, isDraft=%s",
    async (state, isDraft, stateAction) => {
      view = await mount(header({ ...editable, state, isDraft }));
      await click("More actions");
      const menu = document.querySelector('[role="menu"]')!;
      expect(menu.textContent).not.toContain("Convert to draft");
      expect(menu.textContent).not.toContain("Mark ready");
      for (const label of ["Lock", stateAction, "Copy URL", "Open in browser"])
        expect(menu.textContent).toContain(label);
      expect(view.run).not.toHaveBeenCalled();
    },
  );
  it.each([false, true])(
    "renders full-width menu rows without Button layout, allowed=%s",
    async (canWrite) => {
      view = await mount(
        header(canWrite ? editable : detail, {
          ...context,
          capabilities: { ...context.capabilities, lockReasons: ["spam", "off-topic"] },
        }),
      );
      await click("More actions");
      function expectMenuRows() {
        const items = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')];
        expect(items.length).toBeGreaterThanOrEqual(4);
        for (const item of items) {
          expect(item.tagName).not.toBe("BUTTON");
          expect(item.closest('[data-slot="button"]')).toBeNull();
          expect(item.classList.contains("inline-flex")).toBe(false);
          expect(item.parentElement?.classList.contains("inline-flex")).toBe(false);
          expect(item.classList.contains("flex")).toBe(true);
          expect(item.classList.contains("w-full")).toBe(true);
          expect(item.classList.contains("text-left")).toBe(true);
        }
      }
      expectMenuRows();
      if (canWrite) {
        await click("Lock");
        expect(button("spam")).toBeDefined();
        expect(button("off-topic")).toBeDefined();
        expectMenuRows();
      }
      expect(view.run).not.toHaveBeenCalled();
    },
  );
  it.each([
    [false, "Convert to draft", true],
    [true, "Mark ready", false],
  ] as const)(
    "changes draft=%s without confirmation and offers Undo",
    async (isDraft, label, draft) => {
      view = await mount(header({ ...editable, isDraft }));
      await click("More actions");
      await click(label);
      expect(view.run).toHaveBeenCalledWith({ action: "setDraft", draft });
      expect(document.querySelector('[role="alertdialog"]')).toBeNull();
      await act(async () => h.toast.mock.calls[0]![0].actionProps.onClick());
      expect(view.run).toHaveBeenLastCalledWith(
        { action: "setDraft", draft: !draft },
        { waitForPending: true },
      );
    },
  );
  it.each([
    ["open", "Close", "close"],
    ["closed", "Reopen", "reopen"],
  ] as const)(
    "offers the reversible %s state action without confirmation",
    async (state, label, action) => {
      view = await mount(header({ ...editable, state }));
      await click("More actions");
      await click(label);
      expect(view.run).toHaveBeenCalledExactlyOnceWith({ action });
      expect(document.querySelector('[role="alertdialog"]')).toBeNull();
    },
  );
  it("loads GitHub lock reasons from capabilities and reverses lock", async () => {
    view = await mount(
      header(editable, {
        ...context,
        capabilities: { ...context.capabilities, lockReasons: ["spam", "off-topic"] },
      }),
    );
    await click("More actions");
    await click("Lock");
    await click("off-topic");
    expect(view.run).toHaveBeenLastCalledWith({ action: "lock", reason: "off-topic" });
    await act(async () => h.toast.mock.calls[0]![0].actionProps.onClick());
    expect(view.run).toHaveBeenLastCalledWith({ action: "unlock" }, { waitForPending: true });
  });
  it("locks GitLab without a reason and restores the old reason when undoing unlock", async () => {
    view = await mount(
      header(editable, {
        ...gitlabContext,
        capabilities: { ...gitlabContext.capabilities, lockReasons: [] },
      }),
    );
    await click("More actions");
    await click("Lock");
    expect(view.run).toHaveBeenLastCalledWith({ action: "lock", reason: null });
    await view.render(header({ ...editable, locked: true, lockReason: "spam" }));
    await click("More actions");
    await click("Unlock");
    expect(view.run).toHaveBeenLastCalledWith({ action: "unlock" });
    await act(async () => h.toast.mock.calls.at(-1)![0].actionProps.onClick());
    expect(view.run).toHaveBeenLastCalledWith(
      { action: "lock", reason: "spam" },
      { waitForPending: true },
    );
  });
  it("confirms permanent GitLab deletion with its host and number, and cancels without a write", async () => {
    view = await mount(header(editable, gitlabContext));
    await click("More actions");
    await click("Delete");
    let dialog = document.querySelector('[role="alertdialog"]')!;
    expect(dialog.textContent).toContain("!14");
    expect(dialog.textContent).toContain(`This cannot be undone on ${gitlabContext.host}`);
    expect(view.run).not.toHaveBeenCalled();
    await click("Cancel");
    await click("More actions");
    await click("Delete");
    dialog = document.querySelector('[role="alertdialog"]')!;
    await click("Delete merge request", dialog);
    expect(view.run).toHaveBeenCalledExactlyOnceWith({ action: "delete" });
  });
  it("hides Delete without its capability and Revert before merged even if permissions allow", async () => {
    view = await mount(header());
    await click("More actions");
    const menu = document.querySelector('[role="menu"]')!;
    expect(menu.textContent).not.toContain("Delete");
    expect(menu.textContent).not.toContain("Revert");
  });
  it.each(["header", "merge box"] as const)(
    "confirms Revert from the %s with its creation consequence",
    async (surface) => {
      const merged = {
        ...editable,
        state: "merged" as const,
        readiness: { ...detail.readiness, status: "merged" as const },
      };
      view = await mount(
        surface === "header" ? (
          header(merged)
        ) : (
          <PullRequestsMergeBox detail={merged} context={context} projectRef={projectRef} />
        ),
      );
      if (surface === "header") await click("More actions");
      await click("Revert");
      const dialog = document.querySelector('[role="alertdialog"]')!;
      expect(dialog.textContent).toContain("This creates a new pull request that reverts #14");
      expect(view.run).not.toHaveBeenCalled();
      view.run.mockResolvedValue({
        kind: "pullRequestCreated",
        number: 29,
        url: "https://github.com/team/repo/pull/29",
      });
      await click("Create revert pull request", dialog);
      expect(view.run).toHaveBeenCalledWith({ action: "revert" });
    },
  );
  it("copies the URL and opens the host from the overflow with no mutation or console errors", async () => {
    const consoleError = vi.spyOn(console, "error");
    try {
      view = await mount(header());
      await click("More actions");
      await click("Copy URL");
      expect(h.copy).toHaveBeenCalledWith(detail.url, "pull request URL");
      await click("More actions");
      const menu = document.querySelector('[role="menu"]')!;
      await act(async () => menu.querySelector<HTMLAnchorElement>("a")!.click());
      expect(h.open).toHaveBeenCalledWith(detail.url);
      expect(view.run).not.toHaveBeenCalled();
      expect(consoleError).not.toHaveBeenCalled();
    } finally {
      consoleError.mockRestore();
    }
  });
  it("renders every denied action with its reason and refuses dispatch", async () => {
    view = await mount(header(detail, gitlabContext));
    await click("More actions");
    for (const label of ["Convert to draft", "Lock", "Close", "Delete"]) {
      const item = button(label);
      expect(item.disabled || item.getAttribute("aria-disabled") === "true").toBe(true);
      expect(item.title).toBe(detail.permissions.close.reason);
      expect(document.getElementById(item.getAttribute("aria-describedby")!)?.textContent).toBe(
        item.title,
      );
    }
    expect(view.run).not.toHaveBeenCalled();
  });
});
