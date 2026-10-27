import {
  EnvironmentId,
  GitCommandError,
  type RelayClientInstallProgressEvent,
  type TerminalEvent,
  ThreadId,
  WS_METHODS,
} from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Cause from "effect/Cause";
import * as Clock from "effect/Clock";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Option from "effect/Option";
import * as Queue from "effect/Queue";
import * as Ref from "effect/Ref";
import * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import * as TestClock from "effect/testing/TestClock";
import * as Headers from "effect/http/Headers";
import { RpcClient, RpcClientError } from "effect/rpc";

import {
  AVAILABLE_CONNECTION_STATE,
  PrimaryConnectionTarget,
  type PreparedConnection,
  type SupervisorConnectionState,
} from "../connection/model.ts";
import * as EnvironmentSupervisor from "../connection/supervisor.ts";
import * as RpcSession from "../rpc/session.ts";
import type { WsRpcProtocolClient } from "../rpc/protocol.ts";
import {
  EnvironmentRpcRequestObserver,
  request,
  runStream,
  subscribe,
  subscribeInSession,
} from "./client.ts";

const TARGET = new PrimaryConnectionTarget({
  environmentId: EnvironmentId.make("environment-1"),
  label: "Test environment",
  httpBaseUrl: "https://environment.example.test",
  wsBaseUrl: "wss://environment.example.test",
});

const INSTALL_CHECKING: RelayClientInstallProgressEvent = {
  type: "progress",
  stage: "checking",
};
const INSTALL_DOWNLOADING: RelayClientInstallProgressEvent = {
  type: "progress",
  stage: "downloading",
};

function session(client: WsRpcProtocolClient): RpcSession.RpcSession {
  return {
    client,
    initialConfig: Effect.never,
    ready: Effect.void,
    probe: Effect.void,
    closed: Effect.never,
    e2eeAuthenticated: Effect.succeed(null),
  };
}

const makeHarness = Effect.fn("TestEnvironmentRpc.makeHarness")(function* () {
  const state = yield* SubscriptionRef.make<SupervisorConnectionState>(AVAILABLE_CONNECTION_STATE);
  const activeSession = yield* SubscriptionRef.make<Option.Option<RpcSession.RpcSession>>(
    Option.none(),
  );
  const prepared = yield* SubscriptionRef.make<Option.Option<PreparedConnection>>(Option.none());
  const retryCount = yield* Ref.make(0);
  const supervisor = EnvironmentSupervisor.EnvironmentSupervisor.of({
    target: TARGET,
    state,
    session: activeSession,
    prepared,
    connect: Effect.void,
    disconnect: Effect.void,
    retryNow: Ref.update(retryCount, (count) => count + 1),
  } satisfies EnvironmentSupervisor.EnvironmentSupervisor["Service"]);
  return {
    activeSession,
    retryCount,
    supervisor,
  };
});

describe("environment RPC", () => {
  it.effect("observes unary requests until they complete", () =>
    Effect.gen(function* () {
      const observations: string[] = [];
      const client = {
        [WS_METHODS.cloudGetRelayClientStatus]: () =>
          Effect.succeed({ status: "available", version: "2026.6.0" }),
      } as unknown as WsRpcProtocolClient;
      const { activeSession, supervisor } = yield* makeHarness();
      yield* SubscriptionRef.set(activeSession, Option.some(session(client)));

      const result = yield* request(WS_METHODS.cloudGetRelayClientStatus, {}).pipe(
        Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        Effect.provideService(
          EnvironmentRpcRequestObserver,
          EnvironmentRpcRequestObserver.of({
            observe: ({ environmentId, method }) =>
              Effect.sync(() => {
                observations.push(`start:${environmentId}:${method}`);
                return {
                  headers: {},
                  complete: Effect.sync(() => {
                    observations.push(`finish:${environmentId}:${method}`);
                  }),
                };
              }),
          }),
        ),
      );

      expect(result).toEqual({ status: "available", version: "2026.6.0" });
      expect(observations).toEqual([
        `start:${TARGET.environmentId}:${WS_METHODS.cloudGetRelayClientStatus}`,
        `finish:${TARGET.environmentId}:${WS_METHODS.cloudGetRelayClientStatus}`,
      ]);
    }),
  );

  it.effect("sends the observer's headers with the request only when it returns some", () =>
    Effect.gen(function* () {
      const sent: Array<string | undefined> = [];
      // The real client merges `CurrentHeaders` into the request frame it encodes.
      const client = {
        [WS_METHODS.cloudGetRelayClientStatus]: () =>
          Effect.gen(function* () {
            const headers = yield* RpcClient.CurrentHeaders;
            sent.push(Option.getOrUndefined(Headers.get(headers, "x-coddy-studio-profiler-rid")));
            return { status: "available", version: "2026.6.0" };
          }),
      } as unknown as WsRpcProtocolClient;
      const { activeSession, supervisor } = yield* makeHarness();
      yield* SubscriptionRef.set(activeSession, Option.some(session(client)));
      const requestWith = (headers: Record<string, string>) =>
        request(WS_METHODS.cloudGetRelayClientStatus, {}).pipe(
          Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
          Effect.provideService(
            EnvironmentRpcRequestObserver,
            EnvironmentRpcRequestObserver.of({
              observe: () => Effect.succeed({ headers, complete: Effect.void }),
            }),
          ),
        );

      yield* requestWith({ "x-coddy-studio-profiler-rid": "c1:7" });
      yield* requestWith({});

      expect(sent).toEqual(["c1:7", undefined]);
    }),
  );

  it.effect("binds finite streaming commands to one active session", () =>
    Effect.gen(function* () {
      const firstEvents = yield* Queue.unbounded<RelayClientInstallProgressEvent>();
      const secondEvents = yield* Queue.unbounded<RelayClientInstallProgressEvent>();
      const firstClient = {
        [WS_METHODS.cloudInstallRelayClient]: () => Stream.fromQueue(firstEvents),
      } as unknown as WsRpcProtocolClient;
      const secondClient = {
        [WS_METHODS.cloudInstallRelayClient]: () => Stream.fromQueue(secondEvents),
      } as unknown as WsRpcProtocolClient;
      const { activeSession, supervisor } = yield* makeHarness();

      yield* SubscriptionRef.set(activeSession, Option.some(session(firstClient)));
      const resultFiber = yield* runStream(WS_METHODS.cloudInstallRelayClient, {}).pipe(
        Stream.take(2),
        Stream.runCollect,
        Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        Effect.forkChild,
      );
      yield* Effect.yieldNow;

      yield* Queue.offer(firstEvents, INSTALL_CHECKING);
      yield* SubscriptionRef.set(activeSession, Option.some(session(secondClient)));
      yield* Queue.offer(secondEvents, INSTALL_DOWNLOADING);
      yield* Queue.offer(firstEvents, INSTALL_DOWNLOADING);

      expect(yield* Fiber.join(resultFiber)).toEqual([INSTALL_CHECKING, INSTALL_DOWNLOADING]);
    }),
  );

  it.effect("switches durable subscriptions when the supervisor replaces the session", () =>
    Effect.gen(function* () {
      const subscriptions: string[] = [];
      const firstClient = {
        [WS_METHODS.subscribeTerminalEvents]: () => {
          subscriptions.push("first");
          return Stream.never;
        },
      } as unknown as WsRpcProtocolClient;
      const secondClient = {
        [WS_METHODS.subscribeTerminalEvents]: () => {
          subscriptions.push("second");
          return Stream.never;
        },
      } as unknown as WsRpcProtocolClient;
      const { activeSession, retryCount, supervisor } = yield* makeHarness();
      const awaitSubscriptions = Effect.fn("TestEnvironmentRpc.awaitSubscriptions")(function* (
        count: number,
      ) {
        for (let attempt = 0; attempt < 100; attempt += 1) {
          if (subscriptions.length >= count) {
            return;
          }
          yield* Effect.yieldNow;
        }
        return yield* Effect.die(new Error(`Expected ${count} durable subscriptions.`));
      });

      const subscriptionFiber = yield* subscribe(WS_METHODS.subscribeTerminalEvents, {}).pipe(
        Stream.runDrain,
        Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        Effect.forkChild,
      );
      yield* SubscriptionRef.set(activeSession, Option.some(session(firstClient)));
      yield* awaitSubscriptions(1);
      yield* SubscriptionRef.set(activeSession, Option.some(session(secondClient)));
      yield* awaitSubscriptions(2);
      yield* Fiber.interrupt(subscriptionFiber);

      expect(subscriptions).toEqual(["first", "second"]);
      expect(yield* Ref.get(retryCount)).toBe(0);
    }),
  );

  it.effect("keeps durable subscriptions alive across a transport failure and new session", () =>
    Effect.gen(function* () {
      const subscriptions: string[] = [];
      const firstClient = {
        [WS_METHODS.subscribeTerminalEvents]: () => {
          subscriptions.push("first");
          return Stream.fail(
            new RpcClientError.RpcClientError({
              reason: new RpcClientError.RpcClientDefect({
                message: "socket closed",
                cause: new Error("socket closed"),
              }),
            }),
          );
        },
      } as unknown as WsRpcProtocolClient;
      const secondClient = {
        [WS_METHODS.subscribeTerminalEvents]: () => {
          subscriptions.push("second");
          return Stream.never;
        },
      } as unknown as WsRpcProtocolClient;
      const { activeSession, retryCount, supervisor } = yield* makeHarness();

      const subscriptionFiber = yield* subscribe(WS_METHODS.subscribeTerminalEvents, {}).pipe(
        Stream.runDrain,
        Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        Effect.forkChild,
      );
      yield* SubscriptionRef.set(activeSession, Option.some(session(firstClient)));
      for (let attempt = 0; attempt < 100 && subscriptions.length < 1; attempt += 1) {
        yield* Effect.yieldNow;
      }
      yield* SubscriptionRef.set(activeSession, Option.none());
      yield* SubscriptionRef.set(activeSession, Option.some(session(secondClient)));

      for (let attempt = 0; attempt < 100 && subscriptions.length < 2; attempt += 1) {
        yield* Effect.yieldNow;
      }
      yield* Fiber.interrupt(subscriptionFiber);

      expect(subscriptions).toEqual(["first", "second"]);
      expect(yield* Ref.get(retryCount)).toBe(0);
    }),
  );

  it.effect("surfaces domain subscription failures without reconnecting", () =>
    Effect.gen(function* () {
      const domainError = new Error("terminal subscription rejected");
      const client = {
        [WS_METHODS.subscribeTerminalEvents]: () => Stream.fail(domainError),
      } as unknown as WsRpcProtocolClient;
      const { activeSession, retryCount, supervisor } = yield* makeHarness();

      yield* SubscriptionRef.set(activeSession, Option.some(session(client)));
      const error = yield* subscribe(WS_METHODS.subscribeTerminalEvents, {}).pipe(
        Stream.runDrain,
        Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        Effect.flip,
      );

      expect(error).toBe(domainError);
      expect(yield* Ref.get(retryCount)).toBe(0);
    }),
  );

  it.effect("keeps handled domain failures dormant until a replacement session arrives", () =>
    Effect.gen(function* () {
      const domainError = new Error("terminal subscription rejected");
      const subscriptions: string[] = [];
      const observedFailures: Error[] = [];
      const firstClient = {
        [WS_METHODS.subscribeTerminalEvents]: () => {
          subscriptions.push("first");
          return Stream.fail(domainError);
        },
      } as unknown as WsRpcProtocolClient;
      const secondClient = {
        [WS_METHODS.subscribeTerminalEvents]: () => {
          subscriptions.push("second");
          return Stream.never;
        },
      } as unknown as WsRpcProtocolClient;
      const { activeSession, retryCount, supervisor } = yield* makeHarness();

      yield* SubscriptionRef.set(activeSession, Option.some(session(firstClient)));
      const subscriptionFiber = yield* subscribe(
        WS_METHODS.subscribeTerminalEvents,
        {},
        {
          onExpectedFailure: (cause) =>
            Effect.sync(() => {
              observedFailures.push(Cause.squash(cause) as Error);
            }),
        },
      ).pipe(
        Stream.runDrain,
        Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        Effect.forkChild,
      );
      for (let attempt = 0; attempt < 100 && observedFailures.length < 1; attempt += 1) {
        yield* Effect.yieldNow;
      }

      expect(subscriptions).toEqual(["first"]);
      expect(observedFailures).toEqual([domainError]);

      yield* SubscriptionRef.set(activeSession, Option.some(session(secondClient)));
      for (let attempt = 0; attempt < 100 && subscriptions.length < 2; attempt += 1) {
        yield* Effect.yieldNow;
      }
      yield* Fiber.interrupt(subscriptionFiber);

      expect(subscriptions).toEqual(["first", "second"]);
      expect(yield* Ref.get(retryCount)).toBe(0);
    }),
  );

  it.effect("retries handled domain failures within the same session when configured", () =>
    Effect.gen(function* () {
      const domainError = new Error("thread not found yet");
      const subscriptionCount = yield* Ref.make(0);
      const expectedFailureCount = yield* Ref.make(0);
      const client = {
        [WS_METHODS.subscribeTerminalEvents]: () =>
          Stream.unwrap(
            Ref.getAndUpdate(subscriptionCount, (count) => count + 1).pipe(
              Effect.map((count) => (count === 0 ? Stream.fail(domainError) : Stream.never)),
            ),
          ),
      } as unknown as WsRpcProtocolClient;
      const { activeSession, supervisor } = yield* makeHarness();

      yield* SubscriptionRef.set(activeSession, Option.some(session(client)));
      const subscriptionFiber = yield* subscribe(
        WS_METHODS.subscribeTerminalEvents,
        {},
        {
          onExpectedFailure: () => Ref.update(expectedFailureCount, (count) => count + 1),
          retryExpectedFailureAfter: "100 millis",
        },
      ).pipe(
        Stream.runDrain,
        Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        Effect.forkChild,
      );
      for (let attempt = 0; attempt < 100; attempt += 1) {
        if ((yield* Ref.get(expectedFailureCount)) >= 1) {
          break;
        }
        yield* Effect.yieldNow;
      }

      expect(yield* Ref.get(subscriptionCount)).toBe(1);
      expect(yield* Ref.get(expectedFailureCount)).toBe(1);

      yield* TestClock.adjust("100 millis");
      for (let attempt = 0; attempt < 100; attempt += 1) {
        if ((yield* Ref.get(subscriptionCount)) >= 2) {
          break;
        }
        yield* Effect.yieldNow;
      }
      yield* Fiber.interrupt(subscriptionFiber);

      expect(yield* Ref.get(subscriptionCount)).toBe(2);
      expect(yield* Ref.get(expectedFailureCount)).toBe(1);
    }),
  );

  it.effect("runs retry classification against one concrete session without a supervisor", () =>
    Effect.gen(function* () {
      const domainError = new Error("activity scope not ready");
      const subscriptionCount = yield* Ref.make(0);
      const expectedFailureCount = yield* Ref.make(0);
      const client = {
        [WS_METHODS.subscribeActivity]: () =>
          Stream.unwrap(
            Ref.getAndUpdate(subscriptionCount, (count) => count + 1).pipe(
              Effect.map((count) => (count === 0 ? Stream.fail(domainError) : Stream.never)),
            ),
          ),
      } as unknown as WsRpcProtocolClient;

      const subscriptionFiber = yield* subscribeInSession(
        session(client),
        TARGET.environmentId,
        WS_METHODS.subscribeActivity,
        { _tag: "thread", threadId: ThreadId.make("thread-1") },
        {
          onExpectedFailure: () => Ref.update(expectedFailureCount, (count) => count + 1),
          retryExpectedFailureAfter: "100 millis",
        },
      ).pipe(Stream.runDrain, Effect.forkChild);
      for (let attempt = 0; attempt < 100; attempt += 1) {
        if ((yield* Ref.get(expectedFailureCount)) === 1) {
          break;
        }
        yield* Effect.yieldNow;
      }

      expect(yield* Ref.get(subscriptionCount)).toBe(1);
      yield* TestClock.adjust("100 millis");
      for (let attempt = 0; attempt < 100; attempt += 1) {
        if ((yield* Ref.get(subscriptionCount)) === 2) {
          break;
        }
        yield* Effect.yieldNow;
      }
      yield* Fiber.interrupt(subscriptionFiber);

      expect(yield* Ref.get(subscriptionCount)).toBe(2);
      expect(yield* Ref.get(expectedFailureCount)).toBe(1);
    }),
  );

  it.effect("releases legacy retry bookkeeping scopes between repeated failures", () =>
    Effect.gen(function* () {
      const clock = yield* Clock.Clock;
      const liveScopes = new Set<Scope.Scope>();
      let maxLiveScopes = 0;
      let finalizedScopes = 0;
      // Track the attempt scope outside catchCause, which already closes the RPC stream itself.
      const trackedTime = clock.currentTimeMillis.pipe(
        Effect.tap(
          Effect.gen(function* () {
            const scope = yield* Effect.serviceOption(Scope.Scope);
            if (Option.isSome(scope) && !liveScopes.has(scope.value)) {
              liveScopes.add(scope.value);
              maxLiveScopes = Math.max(maxLiveScopes, liveScopes.size);
              yield* Scope.addFinalizer(
                scope.value,
                Effect.sync(() => {
                  liveScopes.delete(scope.value);
                  finalizedScopes += 1;
                }),
              );
            }
          }),
        ),
      );
      let subscriptions = 0;
      const domainError = new Error("thread not found yet");
      const client = {
        [WS_METHODS.subscribeTerminalEvents]: () => {
          subscriptions += 1;
          return Stream.fail(domainError);
        },
      } as unknown as WsRpcProtocolClient;
      const fiber = yield* subscribeInSession(
        session(client),
        TARGET.environmentId,
        WS_METHODS.subscribeTerminalEvents,
        {},
        {
          onExpectedFailure: () => Effect.void,
          retryExpectedFailureAfter: "1 millis",
        },
      ).pipe(
        Stream.runDrain,
        Effect.scoped,
        Effect.provideService(Clock.Clock, { ...clock, currentTimeMillis: trackedTime }),
        Effect.forkChild,
      );

      yield* TestClock.adjust(50);
      expect(subscriptions).toBe(51);
      expect(liveScopes.size).toBe(1);
      expect(maxLiveScopes).toBe(1);
      expect(finalizedScopes).toBe(50);
      yield* Fiber.interrupt(fiber);
      expect(liveScopes.size).toBe(0);
      expect(finalizedScopes).toBe(51);
    }),
  );

  it.effect("delivers legacy retry values in order and waits after each failure handler", () =>
    Effect.gen(function* () {
      const domainError = new Error("thread not found yet");
      const events: string[] = [];
      const observedFailures: unknown[] = [];
      const failureTimes: number[] = [];
      let subscriptions = 0;
      const client = {
        [WS_METHODS.subscribeTerminalEvents]: () => {
          subscriptions += 1;
          return Stream.fromIterable<TerminalEvent>([
            {
              type: "output",
              threadId: "thread-1",
              terminalId: "terminal-1",
              data: `${subscriptions}:first`,
            },
            {
              type: "output",
              threadId: "thread-1",
              terminalId: "terminal-1",
              data: `${subscriptions}:second`,
            },
          ]).pipe(Stream.concat(Stream.fail(domainError)));
        },
      } as unknown as WsRpcProtocolClient;
      const fiber = yield* subscribeInSession(
        session(client),
        TARGET.environmentId,
        WS_METHODS.subscribeTerminalEvents,
        {},
        {
          onExpectedFailure: (cause) =>
            Effect.gen(function* () {
              failureTimes.push(yield* Clock.currentTimeMillis);
              observedFailures.push(Cause.squash(cause));
              events.push(`${subscriptions}:failure`);
              yield* Effect.sleep("25 millis");
              events.push(`${subscriptions}:handled`);
            }),
          retryExpectedFailureAfter: "250 millis",
        },
      ).pipe(
        Stream.runForEach((event) =>
          Effect.sync(() => {
            if (event.type === "output") events.push(event.data);
          }),
        ),
        Effect.forkChild,
      );

      yield* TestClock.adjust(0);
      expect(events).toEqual(["1:first", "1:second", "1:failure"]);
      for (let attempt = 1; attempt <= 2; attempt += 1) {
        yield* TestClock.adjust("25 millis");
        expect(events.at(-1)).toBe(`${attempt}:handled`);
        yield* TestClock.adjust("249 millis");
        expect(subscriptions).toBe(attempt);
        yield* TestClock.adjust("1 millis");
        expect(subscriptions).toBe(attempt + 1);
        expect(observedFailures).toHaveLength(attempt + 1);
      }
      yield* TestClock.adjust("25 millis");
      expect(events).toEqual([
        "1:first",
        "1:second",
        "1:failure",
        "1:handled",
        "2:first",
        "2:second",
        "2:failure",
        "2:handled",
        "3:first",
        "3:second",
        "3:failure",
        "3:handled",
      ]);
      expect(failureTimes).toEqual([0, 275, 550]);
      expect(observedFailures).toEqual([domainError, domainError, domainError]);

      yield* Fiber.interrupt(fiber);
      yield* TestClock.adjust("1 minute");
      expect(subscriptions).toBe(3);
      expect(observedFailures).toHaveLength(3);
    }),
  );

  for (const outcome of ["completion", "transport", "defect", "mixed", "interruption"] as const) {
    it.effect(`stops legacy retries after ${outcome}`, () =>
      Effect.gen(function* () {
        const domainError = new Error("thread not found yet");
        const defect = new Error("subscription invariant failed");
        const causes = {
          completion: null,
          transport: Cause.fail(
            new RpcClientError.RpcClientError({
              reason: new RpcClientError.RpcClientDefect({
                message: "socket closed",
                cause: new Error("socket closed"),
              }),
            }),
          ),
          defect: Cause.die(defect),
          mixed: Cause.combine(Cause.fail(domainError), Cause.die(defect)),
          interruption: Cause.interrupt(),
        };
        const cause = causes[outcome];
        let subscriptions = 0;
        let observedFailures = 0;
        const client = {
          [WS_METHODS.subscribeTerminalEvents]: () => {
            subscriptions += 1;
            if (subscriptions === 1) return Stream.fail(domainError);
            return cause === null ? Stream.empty : Stream.failCause(cause);
          },
        } as unknown as WsRpcProtocolClient;
        const fiber = yield* subscribeInSession(
          session(client),
          TARGET.environmentId,
          WS_METHODS.subscribeTerminalEvents,
          {},
          {
            onExpectedFailure: () =>
              Effect.sync(() => {
                observedFailures += 1;
              }),
            retryExpectedFailureAfter: "250 millis",
          },
        ).pipe(Stream.runDrain, Effect.forkChild);

        yield* TestClock.adjust("250 millis");
        const exit = yield* Fiber.await(fiber);
        if (outcome === "completion" || outcome === "transport") {
          expect(exit).toEqual(Exit.void);
        } else {
          expect(Exit.isFailure(exit)).toBe(true);
          if (Exit.isFailure(exit)) expect(exit.cause.reasons).toEqual(cause?.reasons);
        }
        yield* TestClock.adjust("1 minute");
        expect(subscriptions).toBe(2);
        expect(observedFailures).toBe(1);
      }),
    );
  }

  it.effect("does not classify subscription defects as expected failures", () =>
    Effect.gen(function* () {
      const defect = new Error("subscription invariant failed");
      let expectedFailureCount = 0;
      const client = {
        [WS_METHODS.subscribeTerminalEvents]: () => Stream.die(defect),
      } as unknown as WsRpcProtocolClient;
      const { activeSession, supervisor } = yield* makeHarness();

      yield* SubscriptionRef.set(activeSession, Option.some(session(client)));
      const exit = yield* subscribe(
        WS_METHODS.subscribeTerminalEvents,
        {},
        {
          onExpectedFailure: () =>
            Effect.sync(() => {
              expectedFailureCount += 1;
            }),
        },
      ).pipe(
        Stream.runDrain,
        Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        Effect.exit,
      );

      expect(Exit.isFailure(exit)).toBe(true);
      if (Exit.isFailure(exit)) {
        expect(Cause.hasDies(exit.cause)).toBe(true);
      }
      expect(expectedFailureCount).toBe(0);
    }),
  );

  it.effect("retries only matching expected failures with bounded exponential backoff", () =>
    Effect.gen(function* () {
      const error = new GitCommandError({
        operation: "retryable",
        command: "git",
        cwd: "/repo",
        detail: "retry",
      });
      let subscriptions = 0;
      let observedFailures = 0;
      const client = {
        [WS_METHODS.subscribeVcsStatus]: () => {
          subscriptions += 1;
          return Stream.fail(error);
        },
      } as unknown as WsRpcProtocolClient;
      const subscriptionFiber = yield* subscribeInSession(
        session(client),
        TARGET.environmentId,
        WS_METHODS.subscribeVcsStatus,
        { cwd: "/repo" },
        {
          onExpectedFailure: () =>
            Effect.sync(() => {
              observedFailures += 1;
            }),
          retryExpectedFailure: {
            when: (failure) =>
              failure._tag === "GitCommandError" && failure.operation === "retryable",
            initialDelay: "10 millis",
            maxDelay: "40 millis",
            resetAfter: "100 millis",
          },
        },
      ).pipe(Stream.runDrain, Effect.forkChild);

      yield* TestClock.adjust(0);
      expect(subscriptions).toBe(1);
      for (const [index, delay] of [10, 20, 40, 40].entries()) {
        yield* TestClock.adjust(delay - 1);
        expect(subscriptions).toBe(index + 1);
        yield* TestClock.adjust(1);
        expect(subscriptions).toBe(index + 2);
        expect(observedFailures).toBe(subscriptions);
      }
      yield* Fiber.interrupt(subscriptionFiber);
      yield* TestClock.adjust("1 second");
      expect(subscriptions).toBe(5);
    }),
  );

  it.effect("propagates unmatched expected failures without handling or retrying them", () =>
    Effect.gen(function* () {
      const error = new GitCommandError({
        operation: "fatal",
        command: "git",
        cwd: "/repo",
        detail: "do not retry",
      });
      let subscriptions = 0;
      let observedFailures = 0;
      const client = {
        [WS_METHODS.subscribeVcsStatus]: () => {
          subscriptions += 1;
          return Stream.fail(error);
        },
      } as unknown as WsRpcProtocolClient;
      const exit = yield* subscribeInSession(
        session(client),
        TARGET.environmentId,
        WS_METHODS.subscribeVcsStatus,
        { cwd: "/repo" },
        {
          onExpectedFailure: () =>
            Effect.sync(() => {
              observedFailures += 1;
            }),
          retryExpectedFailure: {
            when: (failure) =>
              failure._tag === "GitCommandError" && failure.operation === "retryable",
            initialDelay: "10 millis",
            maxDelay: "40 millis",
            resetAfter: "100 millis",
          },
        },
      ).pipe(Stream.runDrain, Effect.exit);

      expect(Exit.isFailure(exit)).toBe(true);
      if (Exit.isFailure(exit)) expect(Cause.squash(exit.cause)).toBe(error);
      yield* TestClock.adjust("1 minute");
      expect(subscriptions).toBe(1);
      expect(observedFailures).toBe(0);
    }),
  );

  it.effect("releases retry bookkeeping scopes between repeated failures", () =>
    Effect.gen(function* () {
      const clock = yield* Clock.Clock;
      const liveScopes = new Set<Scope.Scope>();
      const trackedTime = clock.currentTimeMillis.pipe(
        Effect.tap(
          Effect.gen(function* () {
            const scope = yield* Effect.serviceOption(Scope.Scope);
            if (Option.isSome(scope) && !liveScopes.has(scope.value)) {
              liveScopes.add(scope.value);
              yield* Scope.addFinalizer(
                scope.value,
                Effect.sync(() => {
                  liveScopes.delete(scope.value);
                }),
              );
            }
          }),
        ),
      );
      let subscriptions = 0;
      const client = {
        [WS_METHODS.subscribeVcsStatus]: () => {
          subscriptions += 1;
          return Stream.fail(
            new GitCommandError({
              operation: "retryable",
              command: "git",
              cwd: "/repo",
              detail: "retry",
            }),
          );
        },
      } as unknown as WsRpcProtocolClient;
      const fiber = yield* subscribeInSession(
        session(client),
        TARGET.environmentId,
        WS_METHODS.subscribeVcsStatus,
        { cwd: "/repo" },
        {
          retryExpectedFailure: {
            when: () => true,
            initialDelay: 1,
            maxDelay: 1,
            resetAfter: "30 seconds",
          },
        },
      ).pipe(
        Stream.runDrain,
        Effect.scoped,
        Effect.provideService(Clock.Clock, { ...clock, currentTimeMillis: trackedTime }),
        Effect.forkChild,
      );
      yield* TestClock.adjust(100);
      expect(subscriptions).toBe(101);
      expect(liveScopes.size).toBeGreaterThan(0);
      expect(liveScopes.size).toBeLessThanOrEqual(2);
      yield* Fiber.interrupt(fiber);
      expect(liveScopes.size).toBe(0);
    }),
  );

  it.effect("leaves transport failures dormant even when the retry predicate matches", () =>
    Effect.gen(function* () {
      let subscriptions = 0;
      let observedFailures = 0;
      const client = {
        [WS_METHODS.subscribeVcsStatus]: () => {
          subscriptions += 1;
          return Stream.fail(
            new RpcClientError.RpcClientError({
              reason: new RpcClientError.RpcClientDefect({
                message: "socket closed",
                cause: new Error("socket closed"),
              }),
            }),
          );
        },
      } as unknown as WsRpcProtocolClient;
      yield* subscribeInSession(
        session(client),
        TARGET.environmentId,
        WS_METHODS.subscribeVcsStatus,
        { cwd: "/repo" },
        {
          onExpectedFailure: () =>
            Effect.sync(() => {
              observedFailures += 1;
            }),
          retryExpectedFailure: {
            when: () => true,
            initialDelay: "10 millis",
            maxDelay: "40 millis",
            resetAfter: "100 millis",
          },
        },
      ).pipe(Stream.runDrain);
      yield* TestClock.adjust("1 minute");
      expect(subscriptions).toBe(1);
      expect(observedFailures).toBe(0);
    }),
  );
});
