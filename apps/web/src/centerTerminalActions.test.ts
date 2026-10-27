import { scopeThreadRef } from "@coddy/client-runtime/environment";
import { EnvironmentId, ThreadId, type TerminalLaunchCommand } from "@coddy/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

import { CENTER_PANEL_ROOT_GROUP_ID } from "./centerPanelLayout";
import {
  createCenterTerminal,
  type CenterTerminalActionDependencies,
  type CreateCenterTerminalInput,
} from "./centerTerminalActions";

const command: TerminalLaunchCommand = {
  executable: "pnpm",
  args: ["dev"],
  label: "Development server",
  env: { NODE_ENV: "development" },
};

const validAction: CreateCenterTerminalInput = {
  threadRef: scopeThreadRef(EnvironmentId.make("local"), ThreadId.make("thread-a")),
  terminalId: "term-4",
  placement: { type: "tab", groupId: CENTER_PANEL_ROOT_GROUP_ID },
  launch: {
    cwd: "/workspace/project",
    worktreePath: null,
    env: { CODDY_STUDIO_WORKTREE_PATH: "/workspace/project" },
  },
};

function dependencies(
  overrides: Partial<CenterTerminalActionDependencies> = {},
): CenterTerminalActionDependencies {
  return {
    validatePlacement: vi.fn(() => ({ ok: true as const })),
    canSplit: vi.fn(() => true),
    openSession: vi.fn(async () => ({ ok: true as const })),
    place: vi.fn(() => true),
    closeSession: vi.fn(async () => ({ ok: true as const })),
    ...overrides,
  };
}

describe("createCenterTerminal", () => {
  it("rejects the fifth center pane before opening a server session", async () => {
    const deps = dependencies({
      validatePlacement: () => ({ ok: false, reason: "pane-limit" }),
    });

    const result = await createCenterTerminal(validAction, deps);

    expect(result).toEqual({ status: "rejected", reason: "Center pane limit reached." });
    expect(deps.openSession).not.toHaveBeenCalled();
    expect(deps.place).not.toHaveBeenCalled();
    expect(deps.closeSession).not.toHaveBeenCalled();
  });

  it("rejects a split that is too small before opening a server session", async () => {
    const deps = dependencies({ canSplit: () => false });
    const action: CreateCenterTerminalInput = {
      ...validAction,
      placement: { type: "split", groupId: CENTER_PANEL_ROOT_GROUP_ID, direction: "right" },
    };

    const result = await createCenterTerminal(action, deps);

    expect(result).toEqual({
      status: "rejected",
      reason: "Center pane is too small to split.",
    });
    expect(deps.openSession).not.toHaveBeenCalled();
    expect(deps.place).not.toHaveBeenCalled();
    expect(deps.closeSession).not.toHaveBeenCalled();
  });

  it("rejects missing terminal launch context before placement preflight or spawn", async () => {
    const deps = dependencies();

    const result = await createCenterTerminal({ ...validAction, launch: null }, deps);

    expect(result).toEqual({
      status: "rejected",
      reason: "Terminal launch context is unavailable.",
    });
    expect(deps.validatePlacement).not.toHaveBeenCalled();
    expect(deps.canSplit).not.toHaveBeenCalled();
    expect(deps.openSession).not.toHaveBeenCalled();
    expect(deps.place).not.toHaveBeenCalled();
    expect(deps.closeSession).not.toHaveBeenCalled();
  });

  it("returns the open failure without placing or compensating", async () => {
    const deps = dependencies({
      openSession: vi.fn(async () => ({ ok: false as const, reason: "PTY unavailable." })),
    });

    const result = await createCenterTerminal(validAction, deps);

    expect(result).toEqual({ status: "failed", reason: "PTY unavailable." });
    expect(deps.place).not.toHaveBeenCalled();
    expect(deps.closeSession).not.toHaveBeenCalled();
  });

  it("closes a spawned session when the atomic placement loses its race", async () => {
    const closeSession = vi.fn(async () => ({ ok: true as const }));
    const deps = dependencies({ place: vi.fn(() => false), closeSession });

    const result = await createCenterTerminal(validAction, deps);

    expect(result).toEqual({
      status: "failed",
      reason: "Center terminal placement is no longer available.",
    });
    expect(closeSession).toHaveBeenCalledWith({
      threadId: validAction.threadRef.threadId,
      terminalId: validAction.terminalId,
      deleteHistory: true,
    });
  });

  it("does not finish a failed placement until compensating close settles", async () => {
    let finishClose: (() => void) | undefined;
    const closeSession = vi.fn(
      () =>
        new Promise<{ readonly ok: true }>((resolve) => {
          finishClose = () => resolve({ ok: true });
        }),
    );
    const deps = dependencies({ place: vi.fn(() => false), closeSession });
    let settled = false;

    const resultPromise = createCenterTerminal(validAction, deps).then((result) => {
      settled = true;
      return result;
    });
    await vi.waitFor(() => expect(closeSession).toHaveBeenCalledOnce());

    expect(settled).toBe(false);
    finishClose?.();
    await expect(resultPromise).resolves.toEqual({
      status: "failed",
      reason: "Center terminal placement is no longer available.",
    });
  });

  it("rechecks split geometry after session startup before committing placement", async () => {
    const canSplit = vi.fn().mockReturnValueOnce(true).mockReturnValueOnce(false);
    const closeSession = vi.fn(async () => ({ ok: true as const }));
    const deps = dependencies({ canSplit, closeSession });
    const action: CreateCenterTerminalInput = {
      ...validAction,
      placement: { type: "split", groupId: CENTER_PANEL_ROOT_GROUP_ID, direction: "right" },
    };

    const result = await createCenterTerminal(action, deps);

    expect(result).toEqual({
      status: "failed",
      reason: "Center pane became too small to split.",
    });
    expect(canSplit).toHaveBeenCalledTimes(2);
    expect(deps.place).not.toHaveBeenCalled();
    expect(closeSession).toHaveBeenCalledWith({
      threadId: validAction.threadRef.threadId,
      terminalId: validAction.terminalId,
      deleteHistory: true,
    });
  });

  it("reports when compensating close fails and the spawned session may remain", async () => {
    const deps = dependencies({
      place: vi.fn(() => false),
      closeSession: vi.fn(async () => ({
        ok: false as const,
        reason: "terminal backend unavailable",
      })),
    });

    const result = await createCenterTerminal(validAction, deps);

    expect(result).toEqual({
      status: "failed",
      reason:
        "Center terminal placement failed and the spawned session could not be closed: terminal backend unavailable",
      cleanupFailed: true,
    });
  });

  it("classifies interrupted compensating close separately for notice suppression", async () => {
    const deps = dependencies({
      place: vi.fn(() => false),
      closeSession: vi.fn(async () => ({
        ok: false as const,
        reason: "interrupted",
        interrupted: true as const,
      })),
    });

    const result = await createCenterTerminal(validAction, deps);

    expect(result).toEqual({
      status: "failed",
      reason: "Center terminal placement failed and cleanup was interrupted.",
      interrupted: true,
    });
  });

  it("opens then atomically places a terminal with its complete launch context", async () => {
    const openSession = vi.fn<CenterTerminalActionDependencies["openSession"]>(async () => ({
      ok: true,
    }));
    const place = vi.fn<CenterTerminalActionDependencies["place"]>(() => true);
    const deps = dependencies({ openSession, place });
    const action: CreateCenterTerminalInput = {
      ...validAction,
      launch: {
        cwd: "/workspace/worktree",
        worktreePath: "/workspace/worktree",
        env: {
          CODDY_STUDIO_PROJECT_PATH: "/workspace/project",
          CODDY_STUDIO_WORKTREE_PATH: "/workspace/worktree",
        },
        label: "Dev server",
        command,
      },
    };

    const result = await createCenterTerminal(action, deps);

    expect(result).toEqual({ status: "opened", terminalId: "term-4" });
    expect(openSession).toHaveBeenCalledWith({
      threadId: action.threadRef.threadId,
      terminalId: "term-4",
      cwd: "/workspace/worktree",
      worktreePath: "/workspace/worktree",
      env: {
        CODDY_STUDIO_PROJECT_PATH: "/workspace/project",
        CODDY_STUDIO_WORKTREE_PATH: "/workspace/worktree",
      },
      centerPanel: true,
      command,
    });
    expect(place).toHaveBeenCalledWith("term-4", action.placement, {
      label: "Dev server",
      command,
    });
    expect(deps.closeSession).not.toHaveBeenCalled();
    expect(openSession).toHaveBeenCalledBefore(place);
  });
});
