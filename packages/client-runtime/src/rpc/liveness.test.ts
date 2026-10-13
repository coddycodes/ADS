import { describe, expect, it } from "@effect/vitest";
import * as Clock from "effect/Clock";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Random from "effect/Random";
import * as TestClock from "effect/testing/TestClock";

import { makeInboundActivity, runLivenessMonitor } from "./liveness.ts";

const withRandom =
  (value: number) =>
  <A, E, R>(effect: Effect.Effect<A, E, R>) =>
    effect.pipe(
      Effect.provideService(Random.Random, {
        nextDoubleUnsafe: () => value,
        nextIntUnsafe: () => 0,
      }),
    );

describe("runLivenessMonitor", () => {
  it.effect("pings after each silent interval and gives up after three", () =>
    Effect.gen(function* () {
      const activity = yield* makeInboundActivity;
      let pings = 0;
      const monitor = yield* Effect.forkChild(
        runLivenessMonitor({
          activity,
          sendPing: Effect.sync(() => {
            pings += 1;
          }),
        }),
      );
      yield* TestClock.adjust("9999 millis");
      expect(pings).toBe(0);
      yield* TestClock.adjust("1 millis");
      expect(pings).toBe(1);
      yield* TestClock.adjust("10 seconds");
      expect(pings).toBe(2);
      expect(monitor.pollUnsafe()).toBeUndefined();
      yield* TestClock.adjust("10 seconds");
      expect(yield* Fiber.join(monitor)).toBe(30_000);
      expect(pings).toBe(2);
    }).pipe(withRandom(0.5)),
  );

  it.effect("restarts the silent period at the latest inbound message", () =>
    Effect.gen(function* () {
      const activity = yield* makeInboundActivity;
      let pings = 0;
      const monitor = yield* Effect.forkChild(
        runLivenessMonitor({
          activity,
          sendPing: Effect.sync(() => {
            pings += 1;
          }),
        }),
      );
      yield* TestClock.adjust("5 seconds");
      activity.record();
      yield* TestClock.adjust("9 seconds");
      expect(pings).toBe(0);
      yield* TestClock.adjust("1 second");
      expect(pings).toBe(1);
      yield* TestClock.adjust("20 seconds");
      expect(yield* Fiber.join(monitor)).toBe(30_000);
    }).pipe(withRandom(0.5)),
  );

  it.effect("declares death no earlier than 27 seconds of silence", () =>
    Effect.gen(function* () {
      const activity = yield* makeInboundActivity;
      const monitor = yield* Effect.forkChild(
        runLivenessMonitor({ activity, sendPing: Effect.void }),
      );
      yield* TestClock.adjust("26999 millis");
      expect(monitor.pollUnsafe()).toBeUndefined();
      yield* TestClock.adjust("1 millis");
      expect(yield* Fiber.join(monitor)).toBe(27_000);
    }).pipe(withRandom(0)),
  );

  it.effect("declares death no later than 33 seconds of silence", () =>
    Effect.gen(function* () {
      const activity = yield* makeInboundActivity;
      const monitor = yield* Effect.forkChild(
        runLivenessMonitor({ activity, sendPing: Effect.void }),
      );
      yield* TestClock.adjust("32999 millis");
      expect(monitor.pollUnsafe()).toBeUndefined();
      yield* TestClock.adjust("1 millis");
      expect(yield* Fiber.join(monitor)).toBe(33_000);
    }).pipe(withRandom(0.999_999)),
  );

  it.effect("gives the server a full interval after the client was suspended", () =>
    Effect.gen(function* () {
      // A frozen tab: the wall clock runs on while no timer fires.
      const testClock = yield* Clock.Clock;
      let suspendedMs = 0;
      const wallClock: Clock.Clock = {
        ...testClock,
        currentTimeMillisUnsafe: () => testClock.currentTimeMillisUnsafe() + suspendedMs,
        currentTimeMillis: Effect.sync(() => testClock.currentTimeMillisUnsafe() + suspendedMs),
      };
      const activity = yield* makeInboundActivity.pipe(
        Effect.provideService(Clock.Clock, wallClock),
      );
      let pings = 0;
      const monitor = yield* Effect.forkChild(
        runLivenessMonitor({
          activity,
          sendPing: Effect.sync(() => {
            pings += 1;
          }),
        }).pipe(Effect.provideService(Clock.Clock, wallClock)),
      );
      yield* TestClock.adjust("5 seconds");
      suspendedMs = 30 * 60_000;
      // The first timer fires 30 minutes late: one silent interval, one Ping, no verdict.
      yield* TestClock.adjust("5 seconds");
      expect(pings).toBe(1);
      expect(monitor.pollUnsafe()).toBeUndefined();
      // The server answers within the interval it was given.
      yield* TestClock.adjust("2 seconds");
      activity.record();
      yield* TestClock.adjust("8 seconds");
      expect(pings).toBe(1);
      expect(monitor.pollUnsafe()).toBeUndefined();
      // Silence after the answer still ends the connection on the normal schedule.
      yield* TestClock.adjust("30 seconds");
      expect(yield* Fiber.join(monitor)).toBe(30_000);
    }).pipe(withRandom(0.5)),
  );

  it.effect("still declares death after three late wakes without an answer", () =>
    Effect.gen(function* () {
      // A throttled tab: every timer fires 40 seconds late.
      const testClock = yield* Clock.Clock;
      let lateMs = 0;
      const wallClock: Clock.Clock = {
        ...testClock,
        currentTimeMillisUnsafe: () => testClock.currentTimeMillisUnsafe() + lateMs,
        currentTimeMillis: Effect.sync(() => testClock.currentTimeMillisUnsafe() + lateMs),
      };
      const activity = yield* makeInboundActivity.pipe(
        Effect.provideService(Clock.Clock, wallClock),
      );
      let pings = 0;
      const monitor = yield* Effect.forkChild(
        runLivenessMonitor({
          activity,
          sendPing: Effect.sync(() => {
            pings += 1;
          }),
        }).pipe(Effect.provideService(Clock.Clock, wallClock)),
      );
      // The monitor arms its first timer on time.
      yield* Effect.yieldNow;
      for (const wake of [1, 2]) {
        lateMs += 40_000;
        yield* TestClock.adjust("10 seconds");
        expect(pings).toBe(wake);
        expect(monitor.pollUnsafe()).toBeUndefined();
      }
      lateMs += 40_000;
      yield* TestClock.adjust("10 seconds");
      expect(yield* Fiber.join(monitor)).toBe(150_000);
      expect(pings).toBe(2);
    }).pipe(withRandom(0.5)),
  );

  it.effect("counts silence from reset when the socket opens", () =>
    Effect.gen(function* () {
      const activity = yield* makeInboundActivity;
      yield* TestClock.adjust("7 seconds");
      activity.reset();
      const monitor = yield* Effect.forkChild(
        runLivenessMonitor({ activity, sendPing: Effect.void }),
      );
      yield* TestClock.adjust("29 seconds");
      expect(monitor.pollUnsafe()).toBeUndefined();
      yield* TestClock.adjust("1 second");
      expect(yield* Fiber.join(monitor)).toBe(30_000);
    }).pipe(withRandom(0.5)),
  );
});

describe("makeInboundActivity", () => {
  it.effect("awaitAfter completes at the next recorded message", () =>
    Effect.gen(function* () {
      const activity = yield* makeInboundActivity;
      const waiting = yield* Effect.forkChild(activity.awaitAfter(activity.sequence()));
      yield* Effect.yieldNow;
      expect(waiting.pollUnsafe()).toBeUndefined();
      activity.record();
      yield* Fiber.join(waiting);
      yield* activity.awaitAfter(0);
    }),
  );
});
