import { TerminalInputError, WS_METHODS, type TerminalBeginInput } from "@coddy/contracts";
import * as Cause from "effect/Cause";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import {
  requestInSession,
  type EnvironmentRpcInput,
  type EnvironmentUnaryRpcTag,
} from "../rpc/client.ts";
import { TerminalInputAdmissionSignal } from "../rpc/inputAdmission.ts";
import type { RpcSession } from "../rpc/session.ts";

import { terminalInputCharacterBytes } from "./terminalInput.ts";

export type TerminalInputTarget = Omit<TerminalBeginInput, "attachmentSequence">;

const nextAttachmentSequence = new WeakMap<RpcSession, number>();
const encoder = new TextEncoder();
const isTerminalInputError = Schema.is(TerminalInputError);
const inputError = (message: string) => new TerminalInputError({ code: "closed", message });

/** Every RPC here runs under a 15-second timeout whose `TimeoutError` has no message. */
function describeInputFailure(error: unknown): string {
  if (Cause.isTimeoutError(error)) {
    return "Terminal input timed out after 15 seconds. Reattach before typing again.";
  }
  return error instanceof Error && typeof error.message === "string" && error.message.length > 0
    ? error.message
    : String(error);
}

/** UTF-8 frames preserve complete Unicode scalar values, including surrogate pairs. */
export function splitTerminalInputFrames(data: string): string[] {
  const frames: string[] = [];
  let start = 0;
  let end = 0;
  let bytes = 0;
  for (const character of data) {
    const size = terminalInputCharacterBytes(character);
    if (bytes + size > 16 * 1024) {
      frames.push(data.slice(start, end));
      start = end;
      bytes = 0;
    }
    end += character.length;
    bytes += size;
  }
  if (end > start) frames.push(data.slice(start, end));
  return frames;
}

export interface OrderedTerminalInputBinding {
  readonly session: RpcSession;
  readonly prepare: Effect.Effect<void, TerminalInputError>;
  readonly write: (data: string) => Effect.Effect<void, TerminalInputError>;
  /** Synchronously fences all work; cancellation of the captured lease runs independently. */
  readonly dispose: () => void;
}

export function createOrderedTerminalInputBinding(
  session: RpcSession,
  environmentId: string,
  target: TerminalInputTarget,
  context: Context.Context<never> = Context.empty(),
): OrderedTerminalInputBinding {
  const attachmentSequence = nextAttachmentSequence.get(session) ?? 0;
  nextAttachmentSequence.set(session, attachmentSequence + 1);
  const runPromise = Effect.runPromiseWith(context);
  const controller = new AbortController();
  let stopped = false;
  let ordered = false;
  let inputId: string | undefined;
  let sequence = 0;
  let pendingBytes = 0;
  let preparation: Promise<void> | undefined;
  let legacyTail = Promise.resolve();
  const check = () => {
    if (stopped) throw inputError("Terminal input stopped. Reattach before typing again.");
  };
  const run = <T extends EnvironmentUnaryRpcTag>(
    tag: T,
    input: EnvironmentRpcInput<T>,
    abortable = true,
  ) =>
    runPromise(
      requestInSession(session, environmentId, tag, input).pipe(
        Effect.provideService(TerminalInputAdmissionSignal, controller.signal),
        Effect.raceFirst(session.closed),
        Effect.timeout("15 seconds"),
      ),
      abortable ? { signal: controller.signal } : undefined,
    );
  const cancelLease = (lease: string): void => {
    void runPromise(
      requestInSession(session, environmentId, WS_METHODS.terminalCancelInput, {
        ...target,
        inputId: lease,
      }).pipe(Effect.raceFirst(session.closed), Effect.timeout("5 seconds")),
    ).catch(() => undefined);
  };
  const dispose = (): void => {
    if (stopped) return;
    stopped = true;
    controller.abort();
    const lease = inputId;
    if (lease !== undefined) cancelLease(lease);
  };
  const prepare = (): Promise<void> => {
    check();
    preparation ??= (async () => {
      const config = await runPromise(
        session.initialConfig.pipe(Effect.raceFirst(session.closed), Effect.timeout("15 seconds")),
        { signal: controller.signal },
      );
      check();
      ordered = config.environment.capabilities.terminalOrderedInput === true;
      if (ordered) {
        if (!Number.isSafeInteger(attachmentSequence)) {
          throw new TerminalInputError({
            code: "capacity",
            message: "Terminal attachment sequence exhausted. Reconnect before typing again.",
          });
        }
        const lease = await run(
          WS_METHODS.terminalBeginInput,
          { ...target, attachmentSequence },
          false,
        );
        if (stopped) {
          cancelLease(lease.inputId);
          check();
        }
        inputId = lease.inputId;
      }
    })().catch((error: unknown) => {
      dispose();
      throw error;
    });
    return preparation;
  };
  const mapError = (error: unknown): TerminalInputError =>
    isTerminalInputError(error) ? error : inputError(describeInputFailure(error));
  return {
    session,
    prepare: Effect.tryPromise({ try: prepare, catch: mapError }),
    write: (data) =>
      Effect.tryPromise({
        try: async (signal) => {
          check();
          const bytes = encoder.encode(data).length;
          if (pendingBytes + bytes > 1024 * 1024) {
            dispose();
            throw new TerminalInputError({
              code: "capacity",
              message:
                "Terminal input exceeded the 1 MiB pending limit. Reattach before typing again.",
            });
          }
          pendingBytes += bytes;
          const abort = () => dispose();
          signal.addEventListener("abort", abort, { once: true });
          try {
            await prepare();
            check();
            const frames = splitTerminalInputFrames(data);
            if (ordered) {
              await Promise.all(
                frames.map(async (frame) => {
                  check();
                  const frameSequence = sequence++;
                  const lease = inputId!;
                  const acknowledgement = await run(WS_METHODS.terminalWriteInput, {
                    ...target,
                    inputId: lease,
                    sequence: frameSequence,
                    data: frame,
                  });
                  check();
                  if (
                    acknowledgement.inputId !== lease ||
                    acknowledgement.sequence !== frameSequence
                  ) {
                    throw new TerminalInputError({
                      code: "sequence",
                      message: "Terminal input acknowledgement did not match its frame.",
                    });
                  }
                }),
              );
            } else {
              const write = legacyTail.then(async () => {
                check();
                for (const frame of frames)
                  await run(WS_METHODS.terminalWrite, { ...target, data: frame });
              });
              legacyTail = write.catch(() => undefined);
              await write;
            }
          } catch (error) {
            if (ordered) dispose();
            throw error;
          } finally {
            pendingBytes -= bytes;
            signal.removeEventListener("abort", abort);
          }
        },
        catch: mapError,
      }),
    dispose,
  };
}

/** Attachment owns reset/prepare; ordinary writes cannot revive an invalidated binding. */
export function createTerminalInputBindingRegistry() {
  const bindings = new Map<string, OrderedTerminalInputBinding | null>();
  const key = (environmentId: string, target: TerminalInputTarget) =>
    JSON.stringify([environmentId, target.threadId, target.terminalId ?? null]);
  return {
    prepare(
      session: RpcSession,
      environmentId: string,
      target: TerminalInputTarget,
      context?: Context.Context<never>,
    ) {
      const id = key(environmentId, target);
      let binding = bindings.get(id);
      if (binding == null || binding.session !== session) {
        binding?.dispose();
        binding = createOrderedTerminalInputBinding(session, environmentId, target, context);
        bindings.set(id, binding);
      }
      return binding.prepare;
    },
    write(
      session: RpcSession,
      environmentId: string,
      target: TerminalInputTarget,
      data: string,
      context?: Context.Context<never>,
    ) {
      const id = key(environmentId, target);
      let binding = bindings.get(id);
      if (binding === null || (binding !== undefined && binding.session !== session)) {
        binding?.dispose();
        return Effect.fail(inputError("Terminal input requires a fresh attachment."));
      }
      if (binding === undefined) {
        binding = createOrderedTerminalInputBinding(session, environmentId, target, context);
        bindings.set(id, binding);
      }
      return binding.write(data);
    },
    reset(
      environmentId: string,
      target: { readonly threadId: string; readonly terminalId?: string | undefined },
    ): void {
      if (target.terminalId === undefined) {
        const prefix = JSON.stringify([environmentId, target.threadId]).slice(0, -1) + ",";
        for (const [id, binding] of bindings) {
          if (id.startsWith(prefix)) {
            bindings.set(id, null);
            binding?.dispose();
          }
        }
        return;
      }
      const id = key(environmentId, { ...target, terminalId: target.terminalId });
      const binding = bindings.get(id);
      bindings.set(id, null);
      binding?.dispose();
    },
  };
}
