import { EnvironmentId, ProjectId } from "@coddy/contracts";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  effects: [] as Array<() => void | (() => void)>,
  refresh: vi.fn(),
  worktreeAtoms: { refresh: { command: "refresh-worktrees" } },
}));

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useEffect: (effect: () => void | (() => void)) => harness.effects.push(effect),
}));
vi.mock("@coddy/client-runtime/state/worktrees", () => ({
  createWorktreeEnvironmentAtoms: vi.fn(() => harness.worktreeAtoms),
}));
vi.mock("../connection/runtime", () => ({ connectionAtomRuntime: { runtime: true } }));
vi.mock("./use-atom-command", () => ({
  useAtomCommand: (command: unknown) => {
    expect(command).toBe(harness.worktreeAtoms.refresh);
    return harness.refresh;
  },
}));

import { useWorktreeCatalogFocusRefresh, worktreeEnvironment } from "./worktrees";

function eventTargetStub() {
  const listeners = new Map<string, Set<() => void>>();
  return {
    addEventListener: vi.fn((type: string, listener: () => void) => {
      const listenersForType = listeners.get(type) ?? new Set();
      listenersForType.add(listener);
      listeners.set(type, listenersForType);
    }),
    removeEventListener: vi.fn((type: string, listener: () => void) => {
      listeners.get(type)?.delete(listener);
    }),
    fire(type: string) {
      for (const listener of listeners.get(type) ?? []) {
        listener();
      }
    },
  };
}

beforeEach(() => {
  harness.effects.length = 0;
  harness.refresh.mockReset();
  harness.refresh.mockResolvedValue({ _tag: "Success", value: undefined });
});

describe("worktree environment state", () => {
  it("owns one application worktree atom instance", () => {
    expect(worktreeEnvironment).toBe(harness.worktreeAtoms);
  });

  it("refreshes unique subscribed physical projects on focus and visible transitions", () => {
    const windowTarget = eventTargetStub();
    const documentTarget = {
      ...eventTargetStub(),
      visibilityState: "visible",
    };
    vi.stubGlobal("window", windowTarget);
    vi.stubGlobal("document", documentTarget);
    const first = {
      environmentId: EnvironmentId.make("environment-1"),
      projectId: ProjectId.make("project-1"),
    };
    const second = {
      environmentId: EnvironmentId.make("environment-2"),
      projectId: ProjectId.make("project-1"),
    };

    useWorktreeCatalogFocusRefresh([first, first, second]);
    const cleanup = harness.effects[0]?.();
    windowTarget.fire("focus");

    expect(harness.refresh.mock.calls).toEqual([
      [
        {
          environmentId: first.environmentId,
          input: { projectId: first.projectId, reason: "focus" },
        },
      ],
      [
        {
          environmentId: second.environmentId,
          input: { projectId: second.projectId, reason: "focus" },
        },
      ],
    ]);

    documentTarget.visibilityState = "hidden";
    documentTarget.fire("visibilitychange");
    expect(harness.refresh).toHaveBeenCalledTimes(2);

    documentTarget.visibilityState = "visible";
    documentTarget.fire("visibilitychange");
    expect(harness.refresh).toHaveBeenCalledTimes(4);

    expect(typeof cleanup).toBe("function");
    cleanup?.();
    expect(windowTarget.removeEventListener).toHaveBeenCalledWith("focus", expect.any(Function));
    expect(documentTarget.removeEventListener).toHaveBeenCalledWith(
      "visibilitychange",
      expect.any(Function),
    );

    windowTarget.fire("focus");
    documentTarget.fire("visibilitychange");
    expect(harness.refresh).toHaveBeenCalledTimes(4);
  });

  it("does not install listeners without subscribed projects", () => {
    const windowTarget = eventTargetStub();
    const documentTarget = {
      ...eventTargetStub(),
      visibilityState: "visible",
    };
    vi.stubGlobal("window", windowTarget);
    vi.stubGlobal("document", documentTarget);

    useWorktreeCatalogFocusRefresh([]);

    expect(harness.effects[0]?.()).toBeUndefined();
    expect(windowTarget.addEventListener).not.toHaveBeenCalled();
    expect(documentTarget.addEventListener).not.toHaveBeenCalled();
  });
});
