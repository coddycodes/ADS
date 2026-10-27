import {
  isAtomCommandInterrupted,
  type AtomCommandResult,
} from "@coddy/client-runtime/state/runtime";
import type { EnvironmentId, ThreadId } from "@coddy/contracts";

export interface TerminalRetirementTarget {
  readonly environmentId: EnvironmentId;
  readonly threadId: ThreadId;
  readonly terminalId: string;
}

export interface TerminalRetirementDependencies {
  readonly closeSession: (input: {
    readonly environmentId: EnvironmentId;
    readonly input: {
      readonly threadId: ThreadId;
      readonly terminalId: string;
      readonly deleteHistory: true;
    };
  }) => Promise<AtomCommandResult<unknown, unknown>>;
  readonly writeExit: (input: TerminalRetirementTarget & { readonly data: "exit\n" }) => void;
  readonly releaseInput: (target: TerminalRetirementTarget) => void;
}

export type TerminalRetirementResult = "closed" | "interrupted" | "exit-fallback";

export async function retireTerminalSession(
  target: TerminalRetirementTarget,
  dependencies: TerminalRetirementDependencies,
): Promise<TerminalRetirementResult> {
  const closeResult = await dependencies.closeSession({
    environmentId: target.environmentId,
    input: {
      threadId: target.threadId,
      terminalId: target.terminalId,
      deleteHistory: true,
    },
  });
  if (closeResult._tag === "Success") {
    dependencies.releaseInput(target);
    return "closed";
  }
  if (isAtomCommandInterrupted(closeResult)) {
    return "interrupted";
  }
  dependencies.writeExit({ ...target, data: "exit\n" });
  return "exit-fallback";
}
