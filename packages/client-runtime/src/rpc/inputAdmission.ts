import { TerminalInputError, WS_METHODS } from "@coddy/contracts";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Stream from "effect/Stream";
import type { WsRpcProtocolClient } from "./protocol.ts";

/** Cancels admission before dispatch while allowing a begun lease reply to be cleaned up. */
export class TerminalInputAdmissionSignal extends Context.Reference<AbortSignal | undefined>(
  "@coddy/client-runtime/rpc/TerminalInputAdmissionSignal",
  { defaultValue: () => undefined },
) {}

/** One owner per physical RPC client, including the lifetime of streaming requests. */
export function withInputAdmission(client: WsRpcProtocolClient): WsRpcProtocolClient {
  let occupied = 0;
  let inputFrames = 0;
  let inputBytes = 0;
  let queuedBytes = 0;
  const waiting: Array<{ input: boolean; bytes: number; resume: (release: () => void) => void }> =
    [];
  const pump = (): void => {
    for (let index = 0; index < waiting.length;) {
      const item = waiting[index]!;
      if (
        occupied >= (item.input ? 56 : 64) ||
        (item.input && (inputFrames >= 16 || inputBytes + item.bytes > 256 * 1024))
      ) {
        index++;
        continue;
      }
      waiting.splice(index, 1);
      if (item.input) queuedBytes -= item.bytes;
      occupied++;
      if (item.input) {
        inputFrames++;
        inputBytes += item.bytes;
      }
      let released = false;
      item.resume(() => {
        if (released) return;
        released = true;
        occupied--;
        if (item.input) {
          inputFrames--;
          inputBytes -= item.bytes;
        }
        pump();
      });
    }
  };
  const acquire = (input: boolean, bytes: number, priority: boolean) =>
    Effect.flatMap(TerminalInputAdmissionSignal, (signal) =>
      Effect.callback<() => void, TerminalInputError>((resume) => {
        if (input && signal?.aborted) {
          resume(
            Effect.fail(
              new TerminalInputError({
                code: "closed",
                message: "Terminal input was cancelled before dispatch.",
              }),
            ),
          );
          return;
        }
        if (input && (bytes > 16 * 1024 || queuedBytes + bytes > 1024 * 1024)) {
          resume(
            Effect.fail(
              new TerminalInputError({
                code: "capacity",
                message: "Terminal input capacity exceeded. Reattach before typing again.",
              }),
            ),
          );
          return;
        }
        if (input) queuedBytes += bytes;
        const item = {
          input,
          bytes,
          resume: (release: () => void) => {
            signal?.removeEventListener("abort", cancel);
            resume(Effect.succeed(release));
          },
        };
        const cancel = (): void => {
          const index = waiting.indexOf(item);
          if (index === -1) return;
          waiting.splice(index, 1);
          queuedBytes -= bytes;
          resume(
            Effect.fail(
              new TerminalInputError({
                code: "closed",
                message: "Terminal input was cancelled before dispatch.",
              }),
            ),
          );
        };
        if (input) signal?.addEventListener("abort", cancel, { once: true });
        if (priority) waiting.unshift(item);
        else waiting.push(item);
        pump();
        return Effect.sync(() => {
          signal?.removeEventListener("abort", cancel);
          const index = waiting.indexOf(item);
          if (index !== -1) {
            waiting.splice(index, 1);
            if (input) queuedBytes -= bytes;
          }
        });
      }),
    );
  return Object.fromEntries(
    Object.entries(client).map(([tag, method]) => [
      tag,
      (...args: Parameters<typeof method>) => {
        const input =
          tag === WS_METHODS.terminalBeginInput || tag === WS_METHODS.terminalWriteInput;
        const payload = args[0] as { readonly data?: string };
        const bytes = input ? new TextEncoder().encode(payload.data ?? "").length : 0;
        const admission = acquire(input, bytes, tag === WS_METHODS.terminalCancelInput).pipe(
          Effect.interruptible,
        );
        const result = (
          method as (
            ...values: typeof args
          ) => Effect.Effect<unknown, Error> | Stream.Stream<unknown, Error>
        )(...args);
        return Stream.isStream(result)
          ? Stream.unwrap(
              Effect.map(
                Effect.acquireRelease(admission, (release) => Effect.sync(release)),
                () => result,
              ),
            )
          : Effect.acquireUseRelease(
              admission,
              () => result,
              (release) => Effect.sync(release),
            );
      },
    ]),
  ) as WsRpcProtocolClient;
}
