import { runStream } from "@bibcode/client-runtime/rpc";
import { createGitManagerEnvironmentAtoms } from "@bibcode/client-runtime/state/git-manager";
import {
  runStreamInEnvironment,
  type AtomCommandResult,
} from "@bibcode/client-runtime/state/runtime";
import {
  WS_METHODS,
  type EnvironmentId,
  type GitManagerOperationEvent,
  type GitManagerOperationRequest,
} from "@bibcode/contracts";
import { RegistryContext } from "@effect/atom-react";
import * as Cause from "effect/Cause";
import { type AtomRegistry } from "effect/reactivity";
import * as AsyncResult from "effect/reactivity/AsyncResult";
import { useCallback, useContext } from "react";

import { connectionAtomRuntime } from "../connection/runtime";

export const gitManagerEnvironment = createGitManagerEnvironmentAtoms(connectionAtomRuntime);

export interface GitManagerOperationHandle {
  readonly result: Promise<AtomCommandResult<GitManagerOperationEvent, unknown>>;
  readonly cancel: () => void;
}

/**
 * Runs the operation stream through the environment runtime while exposing an explicit
 * cancellation handle. The RPC stream emits one output chunk per completed Git command.
 */
export function runGitManagerOperation(
  registry: AtomRegistry.AtomRegistry,
  target: {
    readonly environmentId: EnvironmentId;
    readonly input: GitManagerOperationRequest;
  },
  onEvent: (event: GitManagerOperationEvent) => void,
): GitManagerOperationHandle {
  const operationAtom = connectionAtomRuntime.atom(
    runStreamInEnvironment(
      target.environmentId,
      runStream(WS_METHODS.gitManagerRunOperation, target.input),
    ),
  );
  let settled = false;
  let unmount: () => void = () => {};
  let unsubscribe: () => void = () => {};
  let settleResult:
    | ((result: AtomCommandResult<GitManagerOperationEvent, unknown>) => void)
    | null = null;
  let lastEvent: GitManagerOperationEvent | null = null;
  const cleanup = () => {
    unsubscribe();
    unmount();
  };
  const settle = (result: AtomCommandResult<GitManagerOperationEvent, unknown>) => {
    if (settled) return;
    settled = true;
    cleanup();
    settleResult?.(result);
  };
  const result = new Promise<AtomCommandResult<GitManagerOperationEvent, unknown>>((resolve) => {
    settleResult = resolve;
    try {
      unmount = registry.mount(operationAtom);
      unsubscribe = registry.subscribe(
        operationAtom,
        (emission) => {
          if (emission._tag === "Success" && emission.value !== lastEvent) {
            lastEvent = emission.value;
            try {
              onEvent(emission.value);
            } catch {
              // Presentation observers cannot fail or cancel the Git operation.
            }
          }
          if (emission._tag !== "Initial" && !emission.waiting) {
            settle(emission);
          }
        },
        { immediate: true },
      );
    } catch (defect) {
      settle(AsyncResult.failure(Cause.die(defect)));
    }
  });

  return {
    result,
    cancel: () => settle(AsyncResult.failure(Cause.interrupt())),
  };
}

/** `runGitManagerOperation` bound to the current atom registry. */
export function useRunGitManagerOperation() {
  const registry = useContext(RegistryContext);
  return useCallback(
    (
      target: {
        readonly environmentId: EnvironmentId;
        readonly input: GitManagerOperationRequest;
      },
      onEvent: (event: GitManagerOperationEvent) => void,
    ) => runGitManagerOperation(registry, target, onEvent),
    [registry],
  );
}
