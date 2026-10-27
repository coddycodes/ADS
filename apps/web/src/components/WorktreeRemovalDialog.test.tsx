// @vitest-environment happy-dom

import {
  EnvironmentId,
  ProjectId,
  ThreadId,
  WorktreeRemovalError,
  WorktreeRemovalPlanToken,
  type WorktreeRemovalPlan,
} from "@coddy/contracts";
import * as Cause from "effect/Cause";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

interface ThreadFixture {
  readonly id: string;
  readonly environmentId: string;
  readonly projectId: string;
  readonly kind: "workspace" | "panel";
  readonly worktreePath: string | null;
  readonly archivedAt: string | null;
  readonly session: { readonly status: string; readonly activeTurnId: string | null } | null;
}

const h = vi.hoisted(() => ({
  commands: new Map<string, (input: unknown) => Promise<unknown>>(),
  runners: new Map<string, (input: unknown) => Promise<unknown>>(),
  calls: [] as Array<{ readonly label: string; readonly input: unknown }>,
  /** The live thread shells; tests replace the array, as the store does. */
  threads: [] as ThreadFixture[],
}));

vi.mock("../state/use-atom-command", () => ({
  useAtomCommand: (command: { label: string }) => h.runners.get(command.label),
}));

vi.mock("../state/entities", () => {
  const find = (ref: { environmentId: string; threadId: string }) =>
    h.threads.find(
      (thread) => thread.environmentId === ref.environmentId && thread.id === ref.threadId,
    ) ?? null;
  return {
    useThreadShell: (ref: { environmentId: string; threadId: string } | null) =>
      ref === null ? null : find(ref),
    useThreadShellsForProjectRefs: (
      refs: ReadonlyArray<{ environmentId: string; projectId: string }>,
    ) =>
      h.threads.filter((thread) =>
        refs.some(
          (ref) => ref.environmentId === thread.environmentId && ref.projectId === thread.projectId,
        ),
      ),
    readThreadShell: find,
    readEnvironmentThreadRefs: (environmentId: string) =>
      h.threads
        .filter((thread) => thread.environmentId === environmentId)
        .map((thread) => ({ environmentId, threadId: thread.id })),
  };
});

vi.mock("../state/worktrees", () => ({
  worktreeEnvironment: {
    getRemovalPlan: { label: "get-plan" },
    removeFromCoddyStudio: { label: "detach" },
    remove: { label: "remove" },
  },
}));

vi.mock("../lib/utils", () => ({
  cn: (...values: unknown[]) => values.filter(Boolean).join(" "),
  newCommandId: () => "command-test",
}));

vi.mock("./ui/dialog", () => ({
  Dialog: ({ open, children }: { open: boolean; children: unknown }) =>
    open ? <div role="presentation">{children as never}</div> : null,
  DialogPopup: ({ children }: { children: unknown }) => (
    <div role="dialog">{children as never}</div>
  ),
  DialogHeader: ({ children }: { children: unknown }) => <header>{children as never}</header>,
  DialogTitle: ({ children }: { children: unknown }) => <h2>{children as never}</h2>,
  DialogDescription: ({ children }: { children: unknown }) => <p>{children as never}</p>,
  DialogPanel: ({ children }: { children: unknown }) => <div>{children as never}</div>,
  DialogFooter: ({ children }: { children: unknown }) => <footer>{children as never}</footer>,
}));

import { WorktreeRemovalDialog, type WorktreeRemovalTarget } from "./WorktreeRemovalDialog";

const target: WorktreeRemovalTarget = {
  environmentId: EnvironmentId.make("environment-one"),
  projectId: ProjectId.make("project-one"),
  threadId: ThreadId.make("thread-one"),
  title: "Feature chat",
  path: "/repo/worktrees/feature-one",
  branch: "feature/one",
  availability: "present",
  registrationState: "registered",
  locked: false,
};

function plan(overrides: Partial<WorktreeRemovalPlan> = {}): WorktreeRemovalPlan {
  return {
    planToken: WorktreeRemovalPlanToken.make("plan-one"),
    generation: 7,
    availability: "present",
    registered: true,
    locked: false,
    trackedChangeCount: 0,
    untrackedFileCount: 0,
    pruneImpact: [],
    ...overrides,
  };
}

const DELETE_BLOCKED_REASON = "Stop the running session before deleting this worktree.";
const RUNNING = { status: "running", activeTurnId: "turn-one" } as const;
const READY = { status: "ready", activeTurnId: null } as const;

/** A live thread shell in the target's project, in the target's worktree unless overridden. */
function threadShell(id: string, overrides: Partial<ThreadFixture> = {}): ThreadFixture {
  return {
    id,
    environmentId: target.environmentId,
    projectId: target.projectId,
    kind: "workspace",
    worktreePath: target.path,
    archivedAt: null,
    session: null,
    ...overrides,
  };
}

let root: Root;
let container: HTMLDivElement;

async function renderDialog(
  overrides: Partial<React.ComponentProps<typeof WorktreeRemovalDialog>> = {},
) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => {
    root.render(
      <WorktreeRemovalDialog
        open
        target={target}
        onOpenChange={vi.fn()}
        onRemoved={vi.fn()}
        {...overrides}
      />,
    );
    await Promise.resolve();
  });
}

async function updateDialog(
  overrides: Partial<React.ComponentProps<typeof WorktreeRemovalDialog>> = {},
) {
  await act(async () => {
    root.render(
      <WorktreeRemovalDialog
        open
        target={target}
        onOpenChange={vi.fn()}
        onRemoved={vi.fn()}
        {...overrides}
      />,
    );
    await Promise.resolve();
  });
}

function button(name: string): HTMLButtonElement {
  const match = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (candidate) => candidate.textContent?.trim() === name,
  );
  expect(match).toBeDefined();
  return match!;
}

async function click(name: string): Promise<void> {
  await act(async () => {
    button(name).click();
    await Promise.resolve();
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.calls = [];
  h.commands.clear();
  h.runners.clear();
  h.threads = [threadShell(target.threadId, { session: READY })];
  for (const label of ["get-plan", "detach", "remove"]) {
    h.runners.set(label, async (input: unknown) => {
      h.calls.push({ label, input });
      const command = h.commands.get(label);
      if (!command) throw new Error(`Missing ${label} command mock`);
      return command(input);
    });
  }
  h.commands.set("get-plan", vi.fn().mockResolvedValue({ _tag: "Success", value: plan() }));
  h.commands.set(
    "detach",
    vi.fn().mockResolvedValue({
      _tag: "Success",
      value: { threadRemoved: true, gitOutcome: "not-requested", orphanCleanupPending: false },
    }),
  );
  h.commands.set(
    "remove",
    vi.fn().mockResolvedValue({
      _tag: "Success",
      value: {
        _tag: "Removed",
        result: { threadRemoved: true, gitOutcome: "removed", orphanCleanupPending: false },
      },
    }),
  );
});

afterEach(async () => {
  if (root) {
    await act(async () => root.unmount());
  }
  container?.remove();
});

describe("WorktreeRemovalDialog", () => {
  it("shows a session-running refusal once as a status without an error or plan retry", async () => {
    const onRemoved = vi.fn();
    h.commands.set("remove", async () => ({
      _tag: "Failure",
      cause: Cause.fail(
        new WorktreeRemovalError({ reason: "session-running", message: "Server session is busy." }),
      ),
    }));
    await renderDialog({ onRemoved });

    await click("Delete Git worktree and remove");

    expect(container.textContent?.split(DELETE_BLOCKED_REASON)).toHaveLength(2);
    const reason = container.querySelector('[role="status"]');
    expect(reason?.textContent).toBe(DELETE_BLOCKED_REASON);
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(container.textContent).not.toContain("Retry removal details");
    const deleteButton = button("Delete Git worktree and remove");
    expect(deleteButton.disabled).toBe(false);
    expect(deleteButton.getAttribute("aria-describedby")).toBe(reason?.id);
    expect(onRemoved).not.toHaveBeenCalled();
    expect(button("Cancel").disabled).toBe(false);
    expect(h.calls.filter((call) => call.label === "remove")).toHaveLength(1);
  });

  it("hands a server refusal over to live session state and clears it when the session stops", async () => {
    h.commands.set("remove", async () => ({
      _tag: "Failure",
      cause: Cause.fail(
        new WorktreeRemovalError({ reason: "session-running", message: "Server session is busy." }),
      ),
    }));
    await renderDialog();
    await click("Delete Git worktree and remove");

    h.threads = [threadShell(target.threadId, { session: RUNNING })];
    await updateDialog();

    expect(container.textContent?.split(DELETE_BLOCKED_REASON)).toHaveLength(2);
    const reason = container.querySelector('[role="status"]');
    expect(reason?.textContent).toBe(DELETE_BLOCKED_REASON);
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(container.textContent).not.toContain("Retry removal details");
    expect(button("Delete Git worktree and remove").disabled).toBe(true);
    expect(button("Delete Git worktree and remove").getAttribute("aria-describedby")).toBe(
      reason?.id,
    );

    h.threads = [threadShell(target.threadId, { session: READY })];
    await updateDialog();

    expect(container.textContent).not.toContain(DELETE_BLOCKED_REASON);
    expect(container.querySelector('[role="status"]')).toBeNull();
    expect(button("Delete Git worktree and remove").disabled).toBe(false);
    expect(button("Delete Git worktree and remove").hasAttribute("aria-describedby")).toBe(false);
  });

  it("keeps other removal failures in an alert with a plan retry", async () => {
    const onRemoved = vi.fn();
    h.commands.set("remove", async () => ({
      _tag: "Failure",
      cause: Cause.fail(
        new WorktreeRemovalError({
          reason: "git-failed",
          message: "Git could not remove the worktree.",
        }),
      ),
    }));
    await renderDialog({ onRemoved });

    await click("Delete Git worktree and remove");

    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "Git could not remove the worktree.",
    );
    expect(button("Retry removal details").disabled).toBe(false);
    expect(container.textContent).not.toContain(DELETE_BLOCKED_REASON);
    expect(onRemoved).not.toHaveBeenCalled();
    expect(button("Cancel").disabled).toBe(false);
    expect(h.calls.filter((call) => call.label === "remove")).toHaveLength(1);
  });

  it("loads the plan and presents explicit present-worktree choices without a path payload", async () => {
    await renderDialog();

    expect(container.textContent).toContain("feature/one");
    expect(container.textContent).toContain("/repo/worktrees/feature-one");
    expect(button("Remove from Coddy Studio").disabled).toBe(false);
    expect(button("Delete Git worktree and remove").disabled).toBe(false);
    expect(button("Cancel").disabled).toBe(false);

    await click("Delete Git worktree and remove");
    const removal = h.calls.find((call) => call.label === "remove");
    expect(removal?.input).toEqual({
      environmentId: "environment-one",
      input: {
        commandId: "command-test",
        projectId: "project-one",
        threadId: "thread-one",
        mode: "delete-git-worktree",
        expectedGeneration: 7,
        planToken: "plan-one",
        forceDirty: false,
        confirmRepositoryWidePrune: false,
      },
    });
    expect(JSON.stringify(removal?.input)).not.toContain("/repo/worktrees/feature-one");
  });

  it("keeps detach enabled while loading and for missing-unregistered worktrees", async () => {
    let resolvePlan!: (value: unknown) => void;
    h.commands.set(
      "get-plan",
      vi.fn(() => new Promise((resolve) => (resolvePlan = resolve))),
    );
    await renderDialog({
      target: {
        ...target,
        availability: "missing-unregistered",
        registrationState: null,
      },
    });

    expect(button("Remove from Coddy Studio").disabled).toBe(false);
    expect(container.textContent).toContain("Loading removal details");
    await act(async () => {
      resolvePlan({
        _tag: "Success",
        value: plan({ availability: "missing-unregistered", registered: false }),
      });
      await Promise.resolve();
    });
    expect(container.textContent).not.toContain("Clean stale Git registration and remove");
  });

  it("uses the fresh plan for registration display and keeps verification failures detach-only", async () => {
    h.commands.set(
      "get-plan",
      vi.fn().mockResolvedValue({
        _tag: "Success",
        value: plan({ availability: "verification-unavailable", registered: true }),
      }),
    );
    await renderDialog({
      target: {
        ...target,
        availability: "verification-unavailable",
        registrationState: null,
      },
    });

    expect(container.textContent).toContain("Registration remains");
    expect(button("Remove from Coddy Studio").disabled).toBe(false);
    expect(container.textContent).not.toContain("Delete Git worktree and remove");
    expect(container.textContent).not.toContain("Clean stale Git registration and remove");
  });

  it("offers stale cleanup only for an unlocked missing registration and explains locks", async () => {
    h.commands.set(
      "get-plan",
      vi.fn().mockResolvedValue({
        _tag: "Success",
        value: plan({ availability: "missing-registered", registered: true }),
      }),
    );
    await renderDialog({ target: { ...target, availability: "missing-registered" } });
    expect(button("Clean stale Git registration and remove").disabled).toBe(false);

    await act(async () => root.unmount());
    container.remove();
    h.commands.set(
      "get-plan",
      vi.fn().mockResolvedValue({
        _tag: "Success",
        value: plan({
          availability: "missing-registered",
          registered: true,
          locked: true,
          lockReason: "Kept by another tool",
        }),
      }),
    );
    await renderDialog({
      target: {
        ...target,
        availability: "missing-registered",
        locked: true,
        lockReason: "Kept by another tool",
      },
    });
    expect(container.textContent).toContain("Kept by another tool");
    expect(container.textContent).not.toContain("Clean stale Git registration and remove");
    expect(button("Remove from Coddy Studio").disabled).toBe(false);
  });

  it("requires separate dirty and repository-wide prune confirmations", async () => {
    h.commands.set(
      "get-plan",
      vi.fn().mockResolvedValue({
        _tag: "Success",
        value: plan({
          trackedChangeCount: 2,
          untrackedFileCount: 3,
          pruneImpact: [
            { path: "/repo/worktrees/old-one", pruneReason: "directory missing", locked: false },
            { path: "/repo/worktrees/old-two", pruneReason: "gitdir invalid", locked: false },
          ],
        }),
      }),
    );
    await renderDialog();

    await click("Delete Git worktree and remove");
    expect(container.textContent).toContain("2 tracked changes");
    expect(container.textContent).toContain("3 untracked files");
    expect(h.calls.filter((call) => call.label === "remove")).toHaveLength(0);

    await click("Delete dirty worktree");
    expect(container.textContent).toContain("/repo/worktrees/old-one");
    expect(container.textContent).toContain("directory missing");
    expect(container.textContent).toContain("/repo/worktrees/old-two");
    expect(h.calls.filter((call) => call.label === "remove")).toHaveLength(0);

    await click("Confirm repository-wide prune");
    expect(h.calls.find((call) => call.label === "remove")?.input).toMatchObject({
      input: { forceDirty: true, confirmRepositoryWidePrune: true },
    });
  });

  it("returns to choices when a confirmation is canceled and closes from the primary cancel", async () => {
    const onOpenChange = vi.fn();
    h.commands.set(
      "get-plan",
      vi.fn().mockResolvedValue({
        _tag: "Success",
        value: plan({ trackedChangeCount: 1 }),
      }),
    );
    await renderDialog({ onOpenChange });
    await click("Delete Git worktree and remove");
    await click("Back");
    expect(container.textContent).toContain("Delete Git worktree and remove");
    await click("Cancel");
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("shows a fresh stale-plan result for review and never automatically retries", async () => {
    h.commands.set(
      "remove",
      vi.fn().mockResolvedValue({
        _tag: "Success",
        value: {
          _tag: "PlanChanged",
          plan: plan({
            planToken: WorktreeRemovalPlanToken.make("plan-two"),
            generation: 8,
            trackedChangeCount: 1,
          }),
        },
      }),
    );
    await renderDialog();
    await click("Delete Git worktree and remove");

    expect(container.textContent).toContain("Removal details changed");
    expect(container.textContent).toContain("1 tracked change");
    expect(h.calls.filter((call) => call.label === "remove")).toHaveLength(1);
  });

  it("ignores a late removal plan from the previously open target", async () => {
    let resolveFirstPlan!: (value: unknown) => void;
    h.commands.set(
      "get-plan",
      vi
        .fn()
        .mockImplementationOnce(() => new Promise((resolve) => (resolveFirstPlan = resolve)))
        .mockResolvedValueOnce({
          _tag: "Success",
          value: plan({
            planToken: WorktreeRemovalPlanToken.make("plan-two"),
            generation: 8,
          }),
        }),
    );
    await renderDialog();
    const secondTarget: WorktreeRemovalTarget = {
      ...target,
      threadId: ThreadId.make("thread-two"),
      title: "Second worktree",
      path: "/repo/worktrees/feature-two",
      branch: "feature/two",
    };
    await updateDialog({ target: secondTarget });

    await act(async () => {
      resolveFirstPlan({
        _tag: "Success",
        value: plan({
          availability: "missing-registered",
          registered: true,
          planToken: WorktreeRemovalPlanToken.make("plan-stale-target"),
        }),
      });
      await Promise.resolve();
    });

    expect(container.textContent).toContain("Second worktree");
    expect(container.textContent).toContain("Delete Git worktree and remove");
    expect(container.textContent).not.toContain("Clean stale Git registration and remove");
  });

  it("reports late removal success for its initiating target without corrupting the next dialog", async () => {
    let resolveRemoval!: (value: unknown) => void;
    h.commands.set(
      "remove",
      vi.fn(() => new Promise((resolve) => (resolveRemoval = resolve))),
    );
    const onRemoved = vi.fn();
    await renderDialog({ onRemoved });
    await click("Delete Git worktree and remove");
    const secondTarget: WorktreeRemovalTarget = {
      ...target,
      threadId: ThreadId.make("thread-two"),
      title: "Second worktree",
      path: "/repo/worktrees/feature-two",
      branch: "feature/two",
    };
    await updateDialog({ target: secondTarget, onRemoved });

    const removalResult = {
      threadRemoved: true,
      gitOutcome: "removed" as const,
      orphanCleanupPending: false,
    };
    await act(async () => {
      resolveRemoval({
        _tag: "Success",
        value: { _tag: "Removed", result: removalResult },
      });
      await Promise.resolve();
    });

    expect(onRemoved).toHaveBeenCalledWith(target, removalResult);
    expect(container.textContent).toContain("Second worktree");
    expect(container.textContent).not.toContain("Removed from Coddy Studio");
  });

  it("reports partial cleanup after the row is removed", async () => {
    const onRemoved = vi.fn();
    h.commands.set(
      "get-plan",
      vi.fn().mockResolvedValue({
        _tag: "Success",
        value: plan({ availability: "missing-registered", registered: true }),
      }),
    );
    h.commands.set(
      "remove",
      vi.fn().mockResolvedValue({
        _tag: "Success",
        value: {
          _tag: "Removed",
          result: {
            threadRemoved: true,
            gitOutcome: "failed",
            detail: "Run git worktree prune manually.",
            orphanCleanupPending: true,
          },
        },
      }),
    );
    await renderDialog({
      target: { ...target, availability: "missing-registered" },
      onRemoved,
    });
    await click("Clean stale Git registration and remove");

    expect(onRemoved).toHaveBeenCalledWith(
      expect.objectContaining({ threadId: "thread-one" }),
      expect.objectContaining({ threadRemoved: true, gitOutcome: "failed" }),
    );
    expect(container.textContent).toContain("Removed from Coddy Studio");
    expect(container.textContent).toContain("Run git worktree prune manually.");
  });

  it("disables mutation while removal is already in progress", async () => {
    await renderDialog({ target: { ...target, availability: "removing" } });
    expect(container.textContent).toContain("Removal is already in progress");
    expect(button("Remove from Coddy Studio").disabled).toBe(true);
  });
});

describe("WorktreeRemovalDialog while a session runs in the worktree", () => {
  /** The visible line explaining why deletion waits, if the dialog shows it. */
  function deletionBlockedReason(): HTMLElement | null {
    return (
      [...container.querySelectorAll<HTMLElement>("p")].find(
        (line) => line.textContent === DELETE_BLOCKED_REASON,
      ) ?? null
    );
  }

  function removeCalls() {
    return h.calls.filter((call) => call.label === "remove");
  }

  it("disables deletion and says why in the dialog, keeping Remove from Coddy Studio available", async () => {
    h.threads = [threadShell(target.threadId, { session: RUNNING })];
    await renderDialog();

    const deleteButton = button("Delete Git worktree and remove");
    expect(deleteButton.disabled).toBe(true);
    const reason = deletionBlockedReason();
    expect(reason, "the reason is visible text, not a hover-only tooltip").not.toBeNull();
    expect(deleteButton.getAttribute("aria-describedby")).toBe(reason!.id);
    expect(button("Remove from Coddy Studio").disabled).toBe(false);
  });

  it("re-enables deletion as soon as the session stops", async () => {
    h.threads = [threadShell(target.threadId, { session: RUNNING })];
    await renderDialog();
    expect(button("Delete Git worktree and remove").disabled).toBe(true);

    h.threads = [threadShell(target.threadId, { session: READY })];
    await updateDialog();

    const deleteButton = button("Delete Git worktree and remove");
    expect(deleteButton.disabled).toBe(false);
    expect(deleteButton.hasAttribute("aria-describedby")).toBe(false);
    expect(deletionBlockedReason()).toBeNull();
    await click("Delete Git worktree and remove");
    expect(removeCalls()).toHaveLength(1);
  });

  it("refuses a click that lands after a session started but before the dialog re-rendered", async () => {
    await renderDialog();
    expect(button("Delete Git worktree and remove").disabled).toBe(false);

    // The store already reports the session; the dialog has not re-rendered.
    h.threads = [threadShell(target.threadId, { session: RUNNING })];
    await click("Delete Git worktree and remove");

    expect(removeCalls()).toHaveLength(0);
    await updateDialog();
    expect(button("Delete Git worktree and remove").disabled).toBe(true);
    expect(deletionBlockedReason()).not.toBeNull();
  });

  it("guards the dirty and prune confirmations too", async () => {
    h.commands.set(
      "get-plan",
      vi.fn().mockResolvedValue({
        _tag: "Success",
        value: plan({
          trackedChangeCount: 1,
          pruneImpact: [
            { path: "/repo/worktrees/old-one", pruneReason: "directory missing", locked: false },
          ],
        }),
      }),
    );
    await renderDialog();
    await click("Delete Git worktree and remove");

    h.threads = [threadShell(target.threadId, { session: RUNNING })];
    await updateDialog();
    expect(button("Delete dirty worktree").disabled).toBe(true);
    expect(deletionBlockedReason()).not.toBeNull();

    h.threads = [threadShell(target.threadId, { session: READY })];
    await updateDialog();
    await click("Delete dirty worktree");
    h.threads = [threadShell(target.threadId, { session: RUNNING })];
    await updateDialog();
    expect(button("Confirm repository-wide prune").disabled).toBe(true);
    expect(deletionBlockedReason()).not.toBeNull();
    expect(removeCalls()).toHaveLength(0);
  });

  it("guards an archived target, which the live store leaves out, while a chat in its worktree runs", async () => {
    // Settings → Archived Threads: the target is archived, so it has no live shell.
    h.threads = [
      threadShell("chat-elsewhere", {
        kind: "panel",
        worktreePath: "/repo/worktrees/other",
        session: RUNNING,
      }),
    ];
    await renderDialog();
    expect(button("Delete Git worktree and remove").disabled).toBe(false);

    h.threads = [...h.threads, threadShell("chat-here", { kind: "panel", session: RUNNING })];
    await updateDialog();
    expect(button("Delete Git worktree and remove").disabled).toBe(true);
    expect(deletionBlockedReason()).not.toBeNull();
  });

  it("shows no reason when the plan offers no deletion to wait for", async () => {
    h.commands.set(
      "get-plan",
      vi.fn().mockResolvedValue({
        _tag: "Success",
        value: plan({ availability: "verification-unavailable", registered: true }),
      }),
    );
    h.threads = [threadShell(target.threadId, { session: RUNNING })];
    await renderDialog({
      target: { ...target, availability: "verification-unavailable", registrationState: null },
    });

    expect(container.textContent).not.toContain("Delete Git worktree and remove");
    expect(deletionBlockedReason()).toBeNull();
    expect(button("Remove from Coddy Studio").disabled).toBe(false);
  });
});
