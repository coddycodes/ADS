// @vitest-environment happy-dom

import type { GitManagerMergePreview, GitManagerRefEntry } from "@coddy/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  preview: null as GitManagerMergePreview | null,
  pending: false,
  error: null as string | null,
  onEvent: null as ((event: Record<string, unknown>) => void) | null,
  runOperation: vi.fn(
    (_registry: unknown, _target: unknown, onEvent: (event: Record<string, unknown>) => void) => {
      h.onEvent = onEvent;
      return { result: new Promise(() => undefined), cancel: vi.fn() };
    },
  ),
  previewMerge: vi.fn(() => ({ kind: "preview" })),
}));

vi.mock("../../../state/gitManager", () => ({
  gitManagerEnvironment: {
    previewMerge: h.previewMerge,
  },
  runGitManagerOperation: h.runOperation,
}));

vi.mock("../../../state/query", () => ({
  useEnvironmentQuery: () => ({
    data: h.preview,
    emission: { _tag: h.preview === null ? "Initial" : "Success" },
    error: h.error,
    isPending: h.pending,
    refresh: () => undefined,
  }),
}));

vi.mock("~/components/ui/dialog", () => ({
  Dialog: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  DialogDescription: ({ children }: { children: React.ReactNode }) => <p>{children}</p>,
  DialogFooter: ({ children }: { children: React.ReactNode }) => <footer>{children}</footer>,
  DialogHeader: ({ children }: { children: React.ReactNode }) => <header>{children}</header>,
  DialogPopup: ({ children }: { children: React.ReactNode }) => <section>{children}</section>,
  DialogTitle: ({ children }: { children: React.ReactNode }) => <h2>{children}</h2>,
}));

vi.mock("../toolbar/GitManagerOperationBanner", () => ({
  GitManagerOperationBanner: ({ operation }: { operation: { _tag: string } | null }) =>
    operation === null ? null : <div data-operation-event={operation._tag} />,
}));

import { GitManagerMergeDialog, type GitManagerMergeDialogProps } from "./GitManagerMergeDialog";

const cleanPreview: GitManagerMergePreview = {
  _tag: "clean",
  source: "refs/heads/feature",
  current: "main",
  ahead: 2,
  behind: 0,
};

function branch(name: string, current = false): GitManagerRefEntry {
  return {
    name,
    tipSha: `${name}-sha`,
    upstream: null,
    ahead: 0,
    behind: 0,
    current,
    isDefault: current,
    worktreePath: null,
    blocked: [],
  };
}

let container: HTMLDivElement;
let root: Root | null;

async function renderDialog(
  refs: ReadonlyArray<GitManagerRefEntry>,
  onOpenChange = vi.fn(),
  disabledReason: string | null = null,
  remoteRefs: ReadonlyArray<GitManagerRefEntry> = [],
  extra: Partial<GitManagerMergeDialogProps> = {},
) {
  await act(async () =>
    root?.render(
      <GitManagerMergeDialog
        open
        disabledReason={disabledReason}
        scope={{ environmentId: "env-a" as never, cwd: "/repo" }}
        projectRef={{ environmentId: "env-a", projectId: "project-a" } as never}
        refs={refs}
        remoteRefs={remoteRefs}
        recentNames={["feature"]}
        onOpenChange={onOpenChange}
        {...extra}
      />,
    ),
  );
  return onOpenChange;
}

function buttonWithText(text: string): HTMLButtonElement;
function buttonWithText(text: string, options: { optional: true }): HTMLButtonElement | null;
function buttonWithText(text: string, options?: { optional: true }): HTMLButtonElement | null {
  const button = [...container.querySelectorAll("button")].find(
    (candidate) => candidate.textContent === text,
  );
  if (button instanceof HTMLButtonElement) return button;
  if (options?.optional) return null;
  throw new Error(`Missing button: ${text}`);
}

function targetInput(): HTMLInputElement {
  const input = document.querySelector<HTMLInputElement>("#git-manager-merge-target");
  if (!input) throw new Error("Missing Into input");
  return input;
}

async function chooseTarget(label: string) {
  await act(async () => {
    targetInput().focus();
    targetInput()
      .closest('[data-slot="input-control"]')
      ?.parentElement?.querySelector<HTMLButtonElement>('[data-slot="combobox-trigger"]')
      ?.click();
  });
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(
      targetInput(),
      label,
    );
    targetInput().dispatchEvent(new Event("input", { bubbles: true }));
  });
  const option = [...document.querySelectorAll<HTMLElement>('[role="option"]')].find(
    (item) => item.textContent?.trim() === label,
  );
  if (!option) throw new Error(`Missing Into option: ${label}`);
  await act(async () => option.click());
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  h.preview = cleanPreview;
  h.pending = false;
  h.error = null;
  h.onEvent = null;
  h.runOperation.mockClear();
  h.previewMerge.mockClear();
});

afterEach(async () => {
  await act(async () => root?.unmount());
  root = null;
  container.remove();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("GitManagerMergeDialog", () => {
  it("disables confirm while previewing and for unrelated histories", async () => {
    h.preview = null;
    h.pending = true;
    await renderDialog([branch("main", true), branch("feature")]);
    expect(buttonWithText("Merge").disabled).toBe(true);

    h.preview = { ...cleanPreview, _tag: "unrelated-histories" };
    h.pending = false;
    await renderDialog([branch("main", true), branch("feature")]);
    expect(buttonWithText("Merge").disabled).toBe(true);
    expect(container.textContent).toContain("unrelated histories");
  });

  it("opens on Merge commit as the visibly selected mode and moves the frame on click", async () => {
    await renderDialog([branch("main", true), branch("feature")]);
    expect(buttonWithText("Merge commit").getAttribute("aria-pressed")).toBe("true");
    expect(buttonWithText("Squash merge").getAttribute("aria-pressed")).toBe("false");
    expect(buttonWithText("Merge commit").className).toContain("aria-pressed:border-primary");

    await act(async () => buttonWithText("Squash merge").click());
    expect(buttonWithText("Squash merge").getAttribute("aria-pressed")).toBe("true");
    expect(buttonWithText("Merge commit").getAttribute("aria-pressed")).toBe("false");
  });

  it("disables confirm and explains when the source has nothing to merge", async () => {
    h.preview = { ...cleanPreview, ahead: 0, behind: 1 };
    await renderDialog([branch("main", true), branch("feature")]);

    const confirm = buttonWithText("Merge");
    expect(confirm.disabled).toBe(true);
    expect(confirm.title).toBe("Nothing to merge: `feature` has no commits that `main` lacks.");
    expect(container.textContent).toContain("Nothing to merge");
  });

  it("renders a server block verbatim and links it to the disabled confirm button", async () => {
    const message = "Server says the working tree must be clean first.";
    const feature = {
      ...branch("feature"),
      blocked: [{ operation: "merge", code: "dirty-working-tree", message }],
    } as GitManagerRefEntry;
    await renderDialog([branch("main", true), feature]);

    const confirm = buttonWithText("Merge");
    expect(confirm.disabled).toBe(true);
    expect(confirm.title).toBe(message);
    expect(confirm.getAttribute("aria-describedby")).toBe("git-manager-merge-disabled-reason");
    expect(container.textContent).toContain(message);
  });

  it("skips merge preview and disables confirmation with the capability reason only", async () => {
    const reason = "This environment does not support Git Manager stash and merge operations.";
    await renderDialog([branch("main", true), branch("feature")], vi.fn(), reason);

    expect(h.previewMerge).not.toHaveBeenCalled();
    expect(buttonWithText("Merge")).toMatchObject({ disabled: true, title: reason });
    expect(container.textContent).toContain(reason);
    expect(buttonWithText("Cancel").disabled).toBe(false);
    expect(container.textContent).toContain("feature");
  });

  it("lists remote branches and sends their full ref with the repository-level block", async () => {
    const message = "Merge is blocked: the working tree has uncommitted changes.";
    const main = {
      ...branch("main", true),
      blocked: [{ operation: "merge", code: "dirty-working-tree", message }],
    } as GitManagerRefEntry;
    h.preview = { ...cleanPreview, source: "refs/remotes/origin/topic" };
    await renderDialog([main, branch("feature")], vi.fn(), null, [branch("origin/topic")]);
    expect(container.textContent).toContain("origin/topic");
    await act(async () => buttonWithText("origin/topic").click());
    expect(h.previewMerge).toHaveBeenLastCalledWith(
      expect.objectContaining({ input: { cwd: "/repo", source: "refs/remotes/origin/topic" } }),
    );
    expect(buttonWithText("Merge")).toMatchObject({ disabled: true, title: message });
  });

  it("merges the checked-out branch into another branch with merge-into", async () => {
    h.preview = { ...cleanPreview, source: "refs/heads/main", current: "release" };
    await renderDialog([branch("main", true), branch("release")], vi.fn(), null, [], {
      mergeIntoAvailable: true,
    });
    await chooseTarget("release");
    expect(container.textContent).toContain("`release` is updated without checking it out.");
    expect(buttonWithText("Squash merge", { optional: true })).toBeNull();
    await act(async () => buttonWithText("main").click());
    await act(async () => buttonWithText("Merge").click());
    expect(h.runOperation).toHaveBeenLastCalledWith(
      expect.anything(),
      {
        environmentId: "env-a",
        input: {
          _tag: "merge-into",
          cwd: "/repo",
          projectId: "project-a",
          source: "refs/heads/main",
          target: "release",
        },
      },
      expect.any(Function),
    );
  });

  it("keeps Merge disabled until the preview matches the chosen target", async () => {
    h.preview = { ...cleanPreview, current: "main" };
    await renderDialog(
      [branch("main", true), branch("release"), branch("feature")],
      vi.fn(),
      null,
      [],
      { mergeIntoAvailable: true },
    );
    await chooseTarget("release");
    expect(buttonWithText("Merge").disabled).toBe(true);
  });

  it("hides the Into picker without the capability", async () => {
    await renderDialog([branch("main", true), branch("feature")]);
    expect(document.querySelector("#git-manager-merge-target")).toBeNull();
  });

  it("allows merging into the current branch when Git is too old to preview", async () => {
    h.preview = null;
    h.error = "Merge preview needs Git 2.38 or later on this environment (found 2.34.1).";
    await renderDialog([branch("main", true), branch("feature")]);
    expect(buttonWithText("Merge").disabled).toBe(false);
    expect(container.textContent).toContain("found 2.34.1");
  });

  it("keeps merge-into disabled when Git is too old to preview", async () => {
    h.preview = null;
    h.error = "Merge preview needs Git 2.38 or later on this environment (found 2.34.1).";
    await renderDialog([branch("main", true), branch("release")], vi.fn(), null, [], {
      mergeIntoAvailable: true,
    });
    await chooseTarget("release");
    expect(buttonWithText("Merge").disabled).toBe(true);
  });

  it("fetches the selected source's remote, then asks for fresh refs", async () => {
    const onRefsStale = vi.fn();
    h.preview = { ...cleanPreview, source: "refs/remotes/origin/topic" };
    await renderDialog([branch("main", true)], vi.fn(), null, [branch("origin/topic")], {
      remotes: ["origin"],
      onRefsStale,
    });
    await act(async () => buttonWithText("origin/topic").click());
    await act(async () => buttonWithText("Fetch").click());
    expect(h.runOperation).toHaveBeenLastCalledWith(
      expect.anything(),
      {
        environmentId: "env-a",
        input: { _tag: "fetch", cwd: "/repo", projectId: "project-a", remote: "origin" },
      },
      expect.any(Function),
    );
    await act(async () =>
      h.onEvent?.({ _tag: "finished", operation: "fetch", message: "Fetched." }),
    );
    expect(onRefsStale).toHaveBeenCalledTimes(1);
  });

  it("fetches each remote in turn for a local source", async () => {
    const onRefsStale = vi.fn();
    await renderDialog([branch("main", true), branch("feature")], vi.fn(), null, [], {
      remotes: ["origin", "upstream"],
      onRefsStale,
    });
    await act(async () => buttonWithText("Fetch").click());
    expect(h.runOperation).toHaveBeenLastCalledWith(
      expect.anything(),
      expect.objectContaining({ input: expect.objectContaining({ remote: "origin" }) }),
      expect.any(Function),
    );
    await act(async () =>
      h.onEvent?.({ _tag: "finished", operation: "fetch", message: "Fetched." }),
    );
    expect(h.runOperation).toHaveBeenLastCalledWith(
      expect.anything(),
      expect.objectContaining({ input: expect.objectContaining({ remote: "upstream" }) }),
      expect.any(Function),
    );
    expect(onRefsStale).not.toHaveBeenCalled();
    await act(async () =>
      h.onEvent?.({ _tag: "finished", operation: "fetch", message: "Fetched." }),
    );
    expect(onRefsStale).toHaveBeenCalledTimes(1);
  });

  it("keeps the dialog open and shows a failed fetch", async () => {
    const onOpenChange = await renderDialog(
      [branch("main", true), branch("feature")],
      vi.fn(),
      null,
      [],
      { remotes: ["origin"] },
    );
    await act(async () => buttonWithText("Fetch").click());
    await act(async () =>
      h.onEvent?.({
        _tag: "failed",
        operation: "fetch",
        code: "authentication",
        message: "Authentication failed.",
        blocked: null,
      }),
    );
    expect(onOpenChange).not.toHaveBeenCalledWith(false);
    expect(container.textContent).toContain("Authentication failed.");
  });

  it("reports whether a fetch or merge is running", async () => {
    const onRunningChange = vi.fn();
    await renderDialog([branch("main", true), branch("feature")], vi.fn(), null, [], {
      remotes: ["origin"],
      onRunningChange,
    });
    await act(async () => buttonWithText("Fetch").click());
    expect(onRunningChange).toHaveBeenLastCalledWith(true);
    await act(async () =>
      h.onEvent?.({ _tag: "finished", operation: "fetch", message: "Fetched." }),
    );
    expect(onRunningChange).toHaveBeenLastCalledWith(false);
  });

  it("refreshes refs when a later remote fails after an earlier fetch succeeded", async () => {
    const onRefsStale = vi.fn();
    await renderDialog([branch("main", true), branch("feature")], vi.fn(), null, [], {
      remotes: ["origin", "upstream"],
      onRefsStale,
    });
    await act(async () => buttonWithText("Fetch").click());
    await act(async () =>
      h.onEvent?.({ _tag: "finished", operation: "fetch", message: "Fetched." }),
    );
    await act(async () =>
      h.onEvent?.({
        _tag: "failed",
        operation: "fetch",
        code: "authentication",
        message: "Authentication failed.",
        blocked: null,
      }),
    );
    expect(onRefsStale).toHaveBeenCalledTimes(1);
    expect(container.textContent).toContain("Authentication failed.");
  });

  it("clears its owner's busy state when unmounted mid-operation", async () => {
    const onRunningChange = vi.fn();
    await renderDialog([branch("main", true), branch("feature")], vi.fn(), null, [], {
      remotes: ["origin"],
      onRunningChange,
    });
    await act(async () => buttonWithText("Fetch").click());
    expect(onRunningChange).toHaveBeenLastCalledWith(true);
    await act(async () => root?.unmount());
    root = null;
    expect(onRunningChange).toHaveBeenLastCalledWith(false);
  });

  it("shows a refs loading failure with a Retry action", async () => {
    const onRefsStale = vi.fn();
    await renderDialog([], vi.fn(), null, [], {
      refsError: "Could not load branches.",
      onRefsStale,
    });
    expect(container.textContent).toContain("Could not load branches.");
    expect(container.textContent).not.toContain("No source branches found.");
    await act(async () => buttonWithText("Retry").click());
    expect(onRefsStale).toHaveBeenCalledTimes(1);
  });

  it("locks the source choice while a fetch runs so the preview stays the fetched one", async () => {
    await renderDialog(
      [branch("main", true), branch("feature")],
      vi.fn(),
      null,
      [branch("origin/topic")],
      {
        remotes: ["origin"],
      },
    );
    await act(async () => buttonWithText("Fetch").click());
    expect(buttonWithText("origin/topic").disabled).toBe(true);
    expect(buttonWithText("feature").disabled).toBe(true);
    await act(async () =>
      h.onEvent?.({ _tag: "finished", operation: "fetch", message: "Fetched." }),
    );
    expect(buttonWithText("origin/topic").disabled).toBe(false);
  });

  it("explains a failed preview on the disabled Merge button", async () => {
    h.preview = null;
    h.error = "The current HEAD could not be resolved for merge preview.";
    await renderDialog([branch("main", true), branch("feature")]);
    expect(buttonWithText("Merge")).toMatchObject({
      disabled: true,
      title: "The current HEAD could not be resolved for merge preview.",
    });
  });

  it("closes on a conflicted current-branch merge so the panel strip shows", async () => {
    const onOpenChange = await renderDialog(
      [branch("main", true), branch("feature")],
      vi.fn(),
      null,
      [],
      { targetMode: "current-branch" },
    );
    expect(buttonWithText("Squash merge", { optional: true })).toBeNull();
    await act(async () => buttonWithText("Merge").click());
    await act(async () =>
      h.onEvent?.({
        _tag: "failed",
        operation: "merge",
        code: "conflicts",
        message: "Conflicts.",
        blocked: null,
      }),
    );
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("closes on finished and stays open with the failure code on failed", async () => {
    const onOpenChange = await renderDialog([branch("main", true), branch("feature")]);
    await act(async () => buttonWithText("Merge").click());
    expect(h.runOperation).toHaveBeenCalledWith(
      expect.anything(),
      {
        environmentId: "env-a",
        input: {
          _tag: "merge",
          cwd: "/repo",
          projectId: "project-a",
          source: "refs/heads/feature",
          noVerify: false,
        },
      },
      expect.any(Function),
    );

    await act(async () =>
      h.onEvent?.({
        _tag: "failed",
        operation: "merge",
        code: "merge-conflicts",
        message: "Resolve conflicts.",
        blocked: null,
      }),
    );
    expect(onOpenChange).not.toHaveBeenCalledWith(false);
    expect(container.textContent).toContain("merge-conflicts");

    await act(async () =>
      h.onEvent?.({ _tag: "finished", operation: "merge", message: "Merged." }),
    );
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });
});
