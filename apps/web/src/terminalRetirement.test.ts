import { EnvironmentId, ThreadId } from "@coddy/contracts";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/reactivity";
import { describe, expect, it, vi } from "vite-plus/test";

import { retireTerminalSession } from "./terminalRetirement";

const target = {
  environmentId: EnvironmentId.make("environment-1"),
  threadId: ThreadId.make("thread-1"),
  terminalId: "terminal-1",
};

describe("retireTerminalSession", () => {
  it("closes once with history deletion and releases input state on success", async () => {
    const closeSession = vi.fn(async () => AsyncResult.success(undefined));
    const writeExit = vi.fn();
    const releaseInput = vi.fn();

    await retireTerminalSession(target, { closeSession, writeExit, releaseInput });

    expect(closeSession).toHaveBeenCalledOnce();
    expect(closeSession).toHaveBeenCalledWith({
      environmentId: target.environmentId,
      input: {
        threadId: target.threadId,
        terminalId: target.terminalId,
        deleteHistory: true,
      },
    });
    expect(writeExit).not.toHaveBeenCalled();
    expect(releaseInput).toHaveBeenCalledOnce();
  });

  it("enqueues an exit fallback after a non-interrupted close failure", async () => {
    const closeSession = vi.fn(async () => AsyncResult.failure(Cause.fail("offline")));
    const writeExit = vi.fn();
    const releaseInput = vi.fn();

    await retireTerminalSession(target, { closeSession, writeExit, releaseInput });

    expect(closeSession).toHaveBeenCalledOnce();
    expect(writeExit).toHaveBeenCalledOnce();
    expect(writeExit).toHaveBeenCalledWith({ ...target, data: "exit\n" });
    expect(releaseInput).not.toHaveBeenCalled();
  });

  it("preserves interruption semantics without scheduling fallback work", async () => {
    const closeSession = vi.fn(async () => AsyncResult.failure(Cause.interrupt(1)));
    const writeExit = vi.fn();
    const releaseInput = vi.fn();

    await retireTerminalSession(target, { closeSession, writeExit, releaseInput });

    expect(closeSession).toHaveBeenCalledOnce();
    expect(writeExit).not.toHaveBeenCalled();
    expect(releaseInput).not.toHaveBeenCalled();
  });
});
