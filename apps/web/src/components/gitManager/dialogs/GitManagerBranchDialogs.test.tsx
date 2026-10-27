// @vitest-environment happy-dom

import type { GitManagerRefEntry } from "@coddy/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

vi.mock("~/components/ui/dialog", () => ({
  Dialog: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  DialogDescription: ({ children }: { children: React.ReactNode }) => <p>{children}</p>,
  DialogFooter: ({ children }: { children: React.ReactNode }) => <footer>{children}</footer>,
  DialogHeader: ({ children }: { children: React.ReactNode }) => <header>{children}</header>,
  DialogPopup: ({ children }: { children: React.ReactNode }) => <section>{children}</section>,
  DialogTitle: ({ children }: { children: React.ReactNode }) => <h2>{children}</h2>,
}));

import {
  GitManagerBranchDialogs,
  type GitManagerBranchDialogSubmission,
} from "./GitManagerBranchDialogs";
import { GitManagerSwitchWithChangesDialog } from "./GitManagerSwitchWithChangesDialog";

function branch(name: string, options: Partial<GitManagerRefEntry> = {}): GitManagerRefEntry {
  return {
    name,
    tipSha: `${name}-sha`,
    upstream: null,
    ahead: 0,
    behind: 0,
    current: false,
    isDefault: false,
    worktreePath: null,
    blocked: [],
    ...options,
  };
}

let container: HTMLDivElement;
let root: Root | null;

function buttonWithText(text: string): HTMLButtonElement {
  const button = [...document.querySelectorAll("button")].find(
    (candidate) => candidate.textContent === text,
  );
  if (!(button instanceof HTMLButtonElement)) throw new Error(`Missing button: ${text}`);
  return button;
}

const LOCAL_REFS = [branch("main", { current: true }), branch("feature")];
const REMOTE_REFS = [branch("origin/main"), branch("origin/release")];

async function renderCreate(
  base: { readonly baseBranch: string | null; readonly baseCommit: string | null },
  refs: ReadonlyArray<GitManagerRefEntry> = LOCAL_REFS,
) {
  const submissions: GitManagerBranchDialogSubmission[] = [];
  await act(async () =>
    root?.render(
      <GitManagerBranchDialogs
        busy={false}
        dialog={{ kind: "create", ...base }}
        errorMessage={null}
        refs={refs}
        remoteRefs={REMOTE_REFS}
        onClose={() => undefined}
        onSubmit={(submission) => {
          submissions.push(submission);
          return Promise.resolve();
        }}
      />,
    ),
  );
  return submissions;
}

function sourceInput(): HTMLInputElement {
  const input = document.querySelector<HTMLInputElement>("#git-manager-create-branch-source");
  if (!input) throw new Error("Missing source input");
  return input;
}

function checkoutBox(): HTMLElement & { checked: boolean } {
  const box = document.querySelector<HTMLElement>(
    '[data-testid="git-manager-create-branch-checkout"]',
  );
  if (!box) throw new Error("Missing checkout checkbox");
  return Object.assign(box, {
    get checked() {
      return box.getAttribute("aria-checked") === "true" || box.hasAttribute("data-checked");
    },
  });
}

/** The user's path: clicking the visible label. */
async function toggleCheckout() {
  await act(async () =>
    [...document.querySelectorAll("label")]
      .find((label) => label.textContent === "Check out after creating")!
      .click(),
  );
}

async function setInputValue(input: HTMLInputElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function typeName(name: string) {
  const input = document.querySelector<HTMLInputElement>("#git-manager-create-branch-name");
  if (!input) throw new Error("Missing name input");
  await setInputValue(input, name);
  // Branch-name rules are debounced.
  await act(async () => new Promise((resolve) => setTimeout(resolve, 300)));
}

async function chooseSource(label: string) {
  await act(async () => {
    sourceInput().focus();
    sourceInput()
      .closest('[data-slot="input-control"]')
      ?.parentElement?.querySelector<HTMLButtonElement>('[data-slot="combobox-trigger"]')
      ?.click();
  });
  await setInputValue(sourceInput(), label);
  const option = [...document.querySelectorAll<HTMLElement>('[role="option"]')].find(
    (item) => item.textContent?.trim() === label,
  );
  if (!option) throw new Error(`Missing source option: ${label}`);
  await act(async () => option.click());
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root?.unmount());
  root = null;
  container.remove();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("GitManagerBranchDialogs", () => {
  it("renders a server-authored rename block verbatim and disables submit", async () => {
    const message = "Rename is blocked because /opaque/feature holds this branch.";
    await act(async () =>
      root?.render(
        <GitManagerBranchDialogs
          busy={false}
          dialog={{
            kind: "rename",
            branch: branch("feature", {
              blocked: [{ operation: "branch-rename", code: "worktree-checked-out", message }],
            }),
          }}
          errorMessage={null}
          refs={[]}
          remoteRefs={[]}
          onClose={() => undefined}
          onSubmit={() => Promise.resolve()}
        />,
      ),
    );

    expect(document.body.textContent).toContain(message);
    expect(buttonWithText("Rename").disabled).toBe(true);
    expect(buttonWithText("Rename").title).toBe(message);
  });

  it("creates and checks out from the active branch by default", async () => {
    const submissions = await renderCreate({ baseBranch: "main", baseCommit: null });

    expect(sourceInput().value).toBe("main");
    expect(checkoutBox().checked).toBe(true);
    await typeName("feature/login");
    await act(async () => buttonWithText("Create and check out").click());

    expect(submissions).toEqual([
      { kind: "create", name: "feature/login", startPoint: "refs/heads/main", checkout: true },
    ]);
  });

  it("creates without checking out and says the active branch stays checked out", async () => {
    const submissions = await renderCreate({ baseBranch: "main", baseCommit: null });

    await toggleCheckout();
    expect(checkoutBox().checked).toBe(false);
    expect(document.body.textContent).toContain("You stay on main.");
    await typeName("feature/later");
    await act(async () => buttonWithText("Create branch").click());

    expect(submissions).toEqual([
      { kind: "create", name: "feature/later", startPoint: "refs/heads/main", checkout: false },
    ]);
  });

  it("creates from another local or remote branch chosen as the source", async () => {
    const submissions = await renderCreate({ baseBranch: "main", baseCommit: null });

    await chooseSource("origin/release");
    expect(sourceInput().value).toBe("origin/release");
    await typeName("hotfix/one");
    await act(async () => buttonWithText("Create and check out").click());
    await chooseSource("feature");
    await act(async () => buttonWithText("Create and check out").click());

    expect(
      submissions.map((submission) => submission.kind === "create" && submission.startPoint),
    ).toEqual(["refs/remotes/origin/release", "refs/heads/feature"]);
  });

  it("starts from the chosen commit when opened from History", async () => {
    const sha = "0123456789abcdef0123456789abcdef01234567";
    const submissions = await renderCreate({ baseBranch: null, baseCommit: sha });

    expect(sourceInput().value).toBe("Commit 0123456");
    await typeName("from-commit");
    await act(async () => buttonWithText("Create and check out").click());

    expect(submissions).toEqual([
      { kind: "create", name: "from-commit", startPoint: sha, checkout: true },
    ]);
  });

  it("starts from the current HEAD when it is detached", async () => {
    const submissions = await renderCreate({ baseBranch: null, baseCommit: null }, [
      branch("main"),
      branch("feature"),
    ]);

    expect(sourceInput().value).toBe("Current HEAD");
    await toggleCheckout();
    expect(document.body.textContent).toContain("You stay on the current commit.");
    await typeName("rescue");
    await act(async () => buttonWithText("Create branch").click());

    expect(submissions).toEqual([
      { kind: "create", name: "rescue", startPoint: null, checkout: false },
    ]);
  });

  it("requires explicit confirmation before deleting a branch", async () => {
    const submissions: GitManagerBranchDialogSubmission[] = [];
    await act(async () =>
      root?.render(
        <GitManagerBranchDialogs
          busy={false}
          dialog={{ kind: "delete", branch: branch("old-feature"), existsUpstream: true }}
          errorMessage={null}
          refs={[]}
          remoteRefs={[]}
          onClose={() => undefined}
          onSubmit={(submission) => {
            submissions.push(submission);
            return Promise.resolve();
          }}
        />,
      ),
    );

    const deleteButton = buttonWithText("Delete branch");
    expect(deleteButton.disabled).toBe(true);
    expect(document.body.textContent).toContain("cannot be undone");

    const confirmation = document.querySelector('input[name="confirm-delete"]');
    expect(confirmation).toBeInstanceOf(HTMLInputElement);
    await act(async () => (confirmation as HTMLInputElement).click());
    expect(deleteButton.disabled).toBe(false);
    await act(async () => deleteButton.click());

    expect(submissions).toEqual([{ kind: "delete", name: "old-feature", deleteRemote: false }]);
  });
});

describe("GitManagerSwitchWithChangesDialog", () => {
  it("resolves both switch strategies with visible stash semantics", async () => {
    const onResolve = vi.fn(() => Promise.resolve());
    await act(async () =>
      root?.render(
        <GitManagerSwitchWithChangesDialog
          branchName="feature"
          busy={false}
          open
          onOpenChange={() => undefined}
          onResolve={onResolve}
        />,
      ),
    );

    expect(document.body.textContent).toContain("ordinary, visible stash entry");
    await act(async () => buttonWithText("Leave my changes").click());
    await act(async () => buttonWithText("Bring my changes").click());

    expect(onResolve).toHaveBeenNthCalledWith(1, { strategy: "stash" });
    expect(onResolve).toHaveBeenNthCalledWith(2, { strategy: "bring" });
  });
});
