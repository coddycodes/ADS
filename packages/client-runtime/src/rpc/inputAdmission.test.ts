import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Stream from "effect/Stream";
import { WS_METHODS } from "@coddy/contracts";
import type { WsRpcProtocolClient } from "./protocol.ts";
import { TerminalInputAdmissionSignal, withInputAdmission } from "./inputAdmission.ts";

describe("physical session input admission", () => {
  it.effect("counts persistent streams and leaves eight control slots", () =>
    Effect.gen(function* () {
      let writes = 0;
      let controls = 0;
      const client = withInputAdmission({
        [WS_METHODS.terminalAttach]: () => Stream.never,
        [WS_METHODS.terminalWriteInput]: () =>
          Effect.sync(() => {
            writes++;
          }),
        [WS_METHODS.terminalCancelInput]: () =>
          Effect.sync(() => {
            controls++;
          }),
      } as unknown as WsRpcProtocolClient);
      const streams = yield* Effect.all(
        Array.from({ length: 56 }, () =>
          Stream.runDrain(client[WS_METHODS.terminalAttach]({} as never)).pipe(Effect.forkChild),
        ),
      );
      yield* Effect.yieldNow;
      const write = yield* client[WS_METHODS.terminalWriteInput]({ data: "a" } as never).pipe(
        Effect.forkChild,
      );
      yield* Effect.yieldNow;
      expect(writes).toBe(0);
      yield* client[WS_METHODS.terminalCancelInput]({} as never);
      expect(controls).toBe(1);
      yield* Fiber.interrupt(streams[0]!);
      yield* Fiber.join(write);
      expect(writes).toBe(1);
      yield* Effect.all(streams.map(Fiber.interrupt));
    }),
  );

  it.effect("shares sixteen outstanding writes across callers and cancels waiting input", () =>
    Effect.gen(function* () {
      let writes = 0;
      const client = withInputAdmission({
        [WS_METHODS.terminalWriteInput]: () =>
          Effect.sync(() => {
            writes++;
          }).pipe(Effect.andThen(Effect.never)),
      } as unknown as WsRpcProtocolClient);
      const fibers = yield* Effect.all(
        Array.from({ length: 17 }, () =>
          client[WS_METHODS.terminalWriteInput]({ data: "x" } as never).pipe(Effect.forkChild),
        ),
      );
      yield* Effect.yieldNow;
      expect(writes).toBe(16);
      yield* Fiber.interrupt(fibers[16]!);
      yield* Fiber.interrupt(fibers[0]!);
      yield* Effect.yieldNow;
      expect(writes).toBe(16);
      yield* Effect.all(fibers.map(Fiber.interrupt));
    }),
  );
  it.effect("cancels an unadmitted begin immediately when its binding is reset", () =>
    Effect.gen(function* () {
      let begins = 0;
      const client = withInputAdmission({
        [WS_METHODS.terminalAttach]: () => Stream.never,
        [WS_METHODS.terminalBeginInput]: () =>
          Effect.sync(() => {
            begins++;
            return { inputId: "lease" };
          }),
      } as unknown as WsRpcProtocolClient);
      const streams = yield* Effect.all(
        Array.from({ length: 56 }, () =>
          Stream.runDrain(client[WS_METHODS.terminalAttach]({} as never)).pipe(Effect.forkChild),
        ),
      );
      yield* Effect.yieldNow;
      const controller = new AbortController();
      const begin = yield* client[WS_METHODS.terminalBeginInput]({} as never).pipe(
        Effect.provideService(TerminalInputAdmissionSignal, controller.signal),
        Effect.flip,
        Effect.forkChild,
      );
      yield* Effect.yieldNow;
      controller.abort();
      const error = yield* Fiber.join(begin);
      expect(error._tag).toBe("TerminalInputError");
      expect(begins).toBe(0);
      yield* Effect.all(streams.map(Fiber.interrupt));
      expect(begins).toBe(0);
    }),
  );
  it.effect("releases failed subscription occupancy before retrying in the same stream", () =>
    Effect.gen(function* () {
      let attempts = 0;
      let writes = 0;
      const client = withInputAdmission({
        [WS_METHODS.terminalAttach]: () => Stream.fail(new Error("retry")),
        [WS_METHODS.terminalWriteInput]: () =>
          Effect.sync(() => {
            writes++;
          }),
      } as unknown as WsRpcProtocolClient);
      const retry = (): Stream.Stream<unknown, Error> =>
        client[WS_METHODS.terminalAttach]({} as never).pipe(
          Stream.catch(() =>
            ++attempts < 56
              ? retry()
              : Stream.fromEffect(client[WS_METHODS.terminalWriteInput]({ data: "x" } as never)),
          ),
        );
      const fiber = yield* Stream.runDrain(retry()).pipe(Effect.forkChild);
      for (let i = 0; i < 100; i++) yield* Effect.yieldNow;
      expect(attempts).toBe(56);
      expect(writes).toBe(1);
      yield* Fiber.interrupt(fiber);
    }),
  );
  it.effect("bounds combined queued bytes and rejects oversized UTF-8 frames", () =>
    Effect.gen(function* () {
      let writes = 0;
      const client = withInputAdmission({
        [WS_METHODS.terminalWriteInput]: () =>
          Effect.sync(() => {
            writes++;
          }).pipe(Effect.andThen(Effect.never)),
      } as unknown as WsRpcProtocolClient);
      const fibers = yield* Effect.all(
        Array.from({ length: 80 }, () =>
          client[WS_METHODS.terminalWriteInput]({ data: "x".repeat(16 * 1024) } as never).pipe(
            Effect.forkChild,
          ),
        ),
      );
      yield* Effect.yieldNow;
      expect(writes).toBe(16);
      const overflow = yield* client[WS_METHODS.terminalWriteInput]({ data: "x" } as never).pipe(
        Effect.flip,
      );
      expect(overflow).toMatchObject({ _tag: "TerminalInputError", code: "capacity" });
      const oversized = yield* client[WS_METHODS.terminalWriteInput]({
        data: "😀".repeat(4097),
      } as never).pipe(Effect.flip);
      expect(oversized).toMatchObject({ _tag: "TerminalInputError", code: "capacity" });
      yield* Effect.all(fibers.map(Fiber.interrupt));
    }),
  );
});
