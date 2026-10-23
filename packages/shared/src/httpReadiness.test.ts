import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import { TestClock } from "effect/testing";
import { HttpClient, HttpClientResponse } from "effect/http";

import {
  DEFAULT_HTTP_READY_PROBE_TIMEOUT_MS,
  describeReadinessCause,
  waitForHttpReady,
} from "./httpReadiness.ts";

describe("describeReadinessCause", () => {
  it("preserves ordinary errors and recursively describes their causes", () => {
    const inner = new Error("inner");
    const outer = new Error("outer", { cause: inner });

    expect(describeReadinessCause(outer)).toEqual({
      name: "Error",
      message: "outer",
      cause: { name: "Error", message: "inner" },
    });
  });

  it("preserves tagged errors instead of their class name", () => {
    const error = Object.assign(new Error("unavailable"), { _tag: "Unavailable" });

    expect(describeReadinessCause(error)).toEqual({
      _tag: "Unavailable",
      message: "unavailable",
    });
  });

  it("returns primitive and null causes unchanged", () => {
    expect(describeReadinessCause("offline")).toBe("offline");
    expect(describeReadinessCause(null)).toBeNull();
  });

  it("selectively describes nested record fields", () => {
    expect(
      describeReadinessCause({
        _tag: "Outer",
        message: "failed",
        reason: { _tag: "Reason", cause: 42 },
        cause: "network",
        ignored: true,
      }),
    ).toEqual({
      _tag: "Outer",
      message: "failed",
      reason: { _tag: "Reason", cause: 42 },
      cause: "network",
    });
    expect(describeReadinessCause({ _tag: 1, message: false })).toEqual({});
  });
});

describe("waitForHttpReady", () => {
  const makeResponse = (
    request: Parameters<Parameters<typeof HttpClient.make>[0]>[0],
    status = 200,
  ) => HttpClientResponse.fromWeb(request, new Response("ready", { status }));

  it.effect("uses normalized defaults and consumes a successful response", () =>
    Effect.gen(function* () {
      const requestUrls: Array<string> = [];
      const client = HttpClient.make((request) =>
        Effect.sync(() => {
          requestUrls.push(request.url);
          return makeResponse(request);
        }),
      );
      let makeErrorCalled = false;

      yield* waitForHttpReady({
        baseUrl: "http://example.test/nested",
        makeError: () => {
          makeErrorCalled = true;
          return { _tag: "ReadyError" as const };
        },
      }).pipe(Effect.provideService(HttpClient.HttpClient, client));

      expect(requestUrls).toEqual(["http://example.test/"]);
      expect(makeErrorCalled).toBe(false);
      expect(DEFAULT_HTTP_READY_PROBE_TIMEOUT_MS).toBe(1_000);
    }),
  );

  it.effect("retries a failed status and keeps the first failure diagnostics", () =>
    Effect.gen(function* () {
      let attempts = 0;
      const failures: Array<{
        readonly requestUrl: string;
        readonly probeTimeoutMs: number;
        readonly attempt: number;
        readonly cause: unknown;
      }> = [];
      const client = HttpClient.make((request) =>
        Effect.sync(() => {
          attempts += 1;
          return makeResponse(request, attempts === 1 ? 503 : 200);
        }),
      );
      const fiber = yield* waitForHttpReady({
        baseUrl: "http://example.test/base/",
        path: "health?full=1",
        timeoutMs: 100,
        intervalMs: 10,
        probeTimeoutMs: 25,
        makeError: (info) => {
          failures.push(info);
          return { _tag: "ReadyError" as const, info };
        },
      }).pipe(Effect.provideService(HttpClient.HttpClient, client), Effect.forkChild);

      yield* TestClock.adjust(10);
      yield* Fiber.join(fiber);

      expect(attempts).toBe(2);
      expect(failures).toHaveLength(1);
      expect(failures[0]).toMatchObject({
        requestUrl: "http://example.test/base/health?full=1",
        probeTimeoutMs: 25,
        attempt: 1,
      });
    }),
  );

  it.effect("retries after an individual probe times out", () =>
    Effect.gen(function* () {
      let attempts = 0;
      const failureCauses: Array<unknown> = [];
      const client = HttpClient.make((request) => {
        attempts += 1;
        return attempts === 1 ? Effect.never : Effect.succeed(makeResponse(request));
      });
      const fiber = yield* waitForHttpReady({
        baseUrl: "http://example.test",
        timeoutMs: 100,
        intervalMs: 10,
        probeTimeoutMs: 10,
        makeError: (info) => {
          failureCauses.push(info.cause);
          return { _tag: "ReadyError" as const, info };
        },
      }).pipe(Effect.provideService(HttpClient.HttpClient, client), Effect.forkChild);

      yield* TestClock.adjust(20);
      yield* Fiber.join(fiber);

      expect(attempts).toBe(2);
      expect(failureCauses).toEqual([
        {
          kind: "probe-timeout",
          attempt: 1,
          probeTimeoutMs: 10,
        },
      ]);
    }),
  );

  it.effect("fails with the last probe diagnostics when the overall timeout elapses", () =>
    Effect.gen(function* () {
      let attempts = 0;
      const failures: Array<{
        readonly attempt: number;
        readonly cause: unknown;
      }> = [];
      const client = HttpClient.make((request) =>
        Effect.sync(() => {
          attempts += 1;
          return makeResponse(request, 503);
        }),
      );
      const fiber = yield* waitForHttpReady({
        baseUrl: "http://example.test",
        timeoutMs: 25,
        intervalMs: 10,
        probeTimeoutMs: 100,
        makeError: (info) => {
          failures.push({ attempt: info.attempt, cause: info.cause });
          return { _tag: "ReadyError" as const, info };
        },
      }).pipe(Effect.provideService(HttpClient.HttpClient, client), Effect.flip, Effect.forkChild);

      yield* TestClock.adjust(25);
      const error = yield* Fiber.join(fiber);

      expect(attempts).toBeGreaterThan(0);
      const cause = error.info.cause as {
        readonly kind: string;
        readonly baseUrl: string;
        readonly timeoutMs: number;
        readonly lastFailure: unknown;
      };
      expect(cause).toMatchObject({
        kind: "overall-timeout",
        baseUrl: "http://example.test",
        timeoutMs: 25,
      });
      expect(cause.lastFailure).toMatchObject({ attempt: expect.any(Number) });
      expect(failures.at(-1)?.cause).toEqual(cause);
    }),
  );

  it.effect("reports an overall timeout even when the first probe never settles", () =>
    Effect.gen(function* () {
      const client = HttpClient.make(() => Effect.never);
      const fiber = yield* waitForHttpReady({
        baseUrl: "http://example.test",
        timeoutMs: 5,
        intervalMs: 1,
        probeTimeoutMs: 50,
        makeError: (info) => ({ _tag: "ReadyError" as const, info }),
      }).pipe(Effect.provideService(HttpClient.HttpClient, client), Effect.flip, Effect.forkChild);

      yield* TestClock.adjust(5);
      const error = yield* Fiber.join(fiber);

      expect(error.info.attempt).toBe(1);
      expect(error.info.cause).toMatchObject({
        kind: "overall-timeout",
        lastFailure: null,
      });
    }),
  );

  it.effect("returns a primitive caller error without wrapping it repeatedly", () =>
    Effect.gen(function* () {
      let makeErrorCalls = 0;
      const client = HttpClient.make((request) => Effect.succeed(makeResponse(request, 503)));

      const error = yield* waitForHttpReady({
        baseUrl: "http://example.test",
        timeoutMs: 100,
        intervalMs: -1,
        probeTimeoutMs: 10,
        makeError: () => {
          makeErrorCalls += 1;
          return "not-ready";
        },
      }).pipe(Effect.provideService(HttpClient.HttpClient, client), Effect.flip);

      expect(error).toBe("not-ready");
      expect(makeErrorCalls).toBe(1);
    }),
  );

  it.effect("normalizes a response body-read failure through makeError exactly once", () =>
    Effect.gen(function* () {
      let makeErrorCalls = 0;
      const bodyFailure = new Error("body read failed");
      const client = HttpClient.make((request) =>
        Effect.succeed(
          HttpClientResponse.fromWeb(
            request,
            new Response(
              new ReadableStream<Uint8Array>({
                start(controller) {
                  controller.error(bodyFailure);
                },
              }),
              { status: 200 },
            ),
          ),
        ),
      );

      const error = yield* waitForHttpReady({
        baseUrl: "http://example.test",
        timeoutMs: 100,
        intervalMs: -1,
        probeTimeoutMs: 10,
        makeError: (info) => {
          makeErrorCalls += 1;
          return { _tag: "ReadyError" as const, info };
        },
      }).pipe(Effect.provideService(HttpClient.HttpClient, client), Effect.flip);

      expect(error).toMatchObject({
        _tag: "ReadyError",
        info: {
          attempt: 1,
          cause: {
            _tag: "HttpClientError",
            reason: { _tag: "DecodeError" },
          },
        },
      });
      expect(makeErrorCalls).toBe(1);
    }),
  );
});
