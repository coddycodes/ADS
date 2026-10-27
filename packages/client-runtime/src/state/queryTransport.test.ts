// @effect-diagnostics globalTimers:off - A bounded real-time pause proves no request was sent.
import { EnvironmentId } from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import { vi } from "vite-plus/test";
import * as Data from "effect/Data";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Scheduler from "effect/Scheduler";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { RpcClientError } from "effect/rpc";
import * as Socket from "effect/socket/Socket";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";

import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import {
  createEnvironmentQueryAtomFamily,
  isEnvironmentQueryAwaitingRetry,
  retryEnvironmentQuery,
  forwardEnvironmentQueryRetry,
} from "./runtime.ts";

const ENVIRONMENT_ID = EnvironmentId.make("query-transport-test");

const cutOff = () =>
  new RpcClientError.RpcClientError({
    reason: new Socket.SocketCloseError({ code: 4408, closeReason: "liveness timeout" }),
  });

class TypedQueryFailure extends Data.TaggedError("TypedQueryFailure")<{
  readonly message: string;
}> {}

const makeHarness = Effect.fn("TestQueryTransport.makeHarness")(function* (
  outcomes: ReadonlyArray<"cut" | "ok" | "blocked" | "held" | "typed">,
  refreshIntervalMs?: number,
  /** A tiny operation budget makes the query fibers yield to each other after almost every step. */
  maxOpsBeforeYield?: number,
  cache?: { readonly staleTimeMs: number; readonly idleTtlMs: number },
) {
  let calls = 0;
  // A "blocked" call waits here until the test releases it, so it can be interrupted in flight.
  const gate = yield* Deferred.make<void>();
  // A "held" call waits on a second gate, so it stays in flight after `release`.
  const hold = yield* Deferred.make<void>();
  const connection = yield* SubscriptionRef.make({ phase: "connected", generation: 1 });
  const supervisor = EnvironmentSupervisor.of({
    target: { environmentId: ENVIRONMENT_ID, label: "Query test" },
    session: yield* SubscriptionRef.make(Option.none()),
    state: connection,
  } as never);
  const environment = EnvironmentRegistry.of({
    run: <A, E>(_: EnvironmentId, effect: Effect.Effect<A, E, EnvironmentSupervisor>) =>
      Effect.provideService(effect, EnvironmentSupervisor, supervisor),
    followStream: (
      _: EnvironmentId,
      stream: Stream.Stream<unknown, unknown, EnvironmentSupervisor>,
    ) => Stream.provideService(stream, EnvironmentSupervisor, supervisor),
  } as never);
  const registryLayer = Layer.succeed(EnvironmentRegistry, environment);
  const query = createEnvironmentQueryAtomFamily(
    Atom.runtime(
      maxOpsBeforeYield === undefined
        ? registryLayer
        : Layer.merge(registryLayer, Layer.succeed(Scheduler.MaxOpsBeforeYield, maxOpsBeforeYield)),
    ),
    {
      label: "test:query-transport",
      staleTimeMs: 0, // Remount really attempts SWR revalidation in these regressions.
      ...cache,
      transportCutoffKey: (_input: { readonly refreshCacheKey: string }) => ({}),
      ...(refreshIntervalMs === undefined ? {} : { refreshIntervalMs }),
      execute: () =>
        Effect.suspend(
          (): Effect.Effect<string, RpcClientError.RpcClientError | TypedQueryFailure> => {
            const outcome = outcomes[calls] ?? "ok";
            calls += 1;
            switch (outcome) {
              case "cut":
                return Effect.fail(cutOff());
              case "typed":
                return Effect.fail(new TypedQueryFailure({ message: "Branch not found." }));
              case "blocked":
                return Deferred.await(gate).pipe(Effect.as("page"));
              case "held":
                return Deferred.await(hold).pipe(Effect.as("page"));
              case "ok":
                return Effect.succeed("page");
            }
          },
        ),
    },
  );
  return {
    atom: query({ environmentId: ENVIRONMENT_ID, input: { refreshCacheKey: "first" } }),
    remountAtom: (refreshCacheKey: string) =>
      query({ environmentId: ENVIRONMENT_ID, input: { refreshCacheKey } }),
    calls: () => calls,
    release: () => Deferred.succeed(gate, undefined),
    releaseHeld: () => Deferred.succeed(hold, undefined),
    reconnect: (generation: number) =>
      SubscriptionRef.set(connection, { phase: "connected", generation }),
    /** The session drops: the supervisor leaves `connected` before any view reacts. */
    loseSession: (generation: number) =>
      SubscriptionRef.set(connection, { phase: "backoff", generation }),
    registry: AtomRegistry.make(),
  };
});

const settle = () => Effect.promise(() => new Promise<void>((resolve) => setTimeout(resolve, 100)));

describe("createEnvironmentQueryAtomFamily after transport cut-offs", () => {
  it.effect("re-issues a cut-off request once on the next connection", () =>
    Effect.gen(function* () {
      const h = yield* makeHarness(["cut", "ok"]);
      h.registry.mount(h.atom);
      yield* Effect.promise(() =>
        vi.waitFor(() => expect(AsyncResult.isFailure(h.registry.get(h.atom))).toBe(true)),
      );
      yield* h.reconnect(2);
      yield* Effect.promise(() =>
        vi.waitFor(() => expect(AsyncResult.isSuccess(h.registry.get(h.atom))).toBe(true)),
      );
      expect(h.calls()).toBe(2);
    }),
  );

  it.effect("waits for an explicit Retry after two cut-offs in a row", () =>
    Effect.gen(function* () {
      const h = yield* makeHarness(["cut", "cut", "ok"]);
      h.registry.mount(h.atom);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
      yield* h.reconnect(2);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(2)));
      yield* h.reconnect(3);
      yield* settle();
      expect(h.calls()).toBe(2);
      expect(AsyncResult.isFailure(h.registry.get(h.atom))).toBe(true);
      retryEnvironmentQuery(h.atom, () => h.registry.refresh(h.atom));
      yield* Effect.promise(() =>
        vi.waitFor(() => expect(AsyncResult.isSuccess(h.registry.get(h.atom))).toBe(true)),
      );
      expect(h.calls()).toBe(3);
    }),
  );

  it.effect("awaits an explicit Retry only while the latched failure is the rendered result", () =>
    Effect.gen(function* () {
      const h = yield* makeHarness(["cut", "cut", "ok"]);
      h.registry.mount(h.atom);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
      yield* h.reconnect(2);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(2)));
      yield* Effect.promise(() =>
        vi.waitFor(() =>
          expect(isEnvironmentQueryAwaitingRetry(h.atom, h.registry.get(h.atom))).toBe(true),
        ),
      );
      // Retry belongs to the failure on screen: a value or a load never offers it.
      expect(isEnvironmentQueryAwaitingRetry(h.atom, AsyncResult.success("page"))).toBe(false);
      expect(isEnvironmentQueryAwaitingRetry(h.atom, AsyncResult.initial(true))).toBe(false);
      h.registry.dispose();
    }),
  );

  it.effect("does not send a third request on stale revalidation or remount", () =>
    Effect.gen(function* () {
      const h = yield* makeHarness(["cut", "cut", "ok"]);
      const unmount = h.registry.mount(h.atom);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
      yield* h.reconnect(2);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(2)));
      unmount();
      const release = h.registry.mount(h.atom);
      h.registry.refresh(h.atom); // automatic invalidation does not authorize Retry
      yield* settle();
      expect(h.calls()).toBe(2);
      expect(AsyncResult.isFailure(h.registry.get(h.atom))).toBe(true);
      release();
      h.registry.dispose();
    }),
  );

  it.effect(
    "keeps exhaustion across new local cache keys and forwards explicit Retry through wrappers",
    () =>
      Effect.gen(function* () {
        const h = yield* makeHarness(["cut", "cut", "ok"]);
        const unmount = h.registry.mount(h.atom);
        yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
        yield* h.reconnect(2);
        yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(2)));
        unmount();
        const remounted = h.remountAtom("new-view");
        const wrapped = forwardEnvironmentQueryRetry(
          remounted,
          remounted.pipe(Atom.makeRefreshOnSignal(Atom.make(0))),
        );
        h.registry.mount(wrapped);
        yield* settle();
        expect(h.calls()).toBe(2);
        retryEnvironmentQuery(wrapped, () => h.registry.refresh(wrapped));
        yield* Effect.promise(() =>
          vi.waitFor(() => expect(AsyncResult.isSuccess(h.registry.get(wrapped))).toBe(true)),
        );
        expect(h.calls()).toBe(3);
        h.registry.dispose();
      }),
  );

  it.effect("suppresses periodic refresh after the automatic re-issue fails", () =>
    Effect.gen(function* () {
      const h = yield* makeHarness(["cut", "cut", "ok"], 20);
      h.registry.mount(h.atom);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
      yield* settle();
      expect(h.calls()).toBe(1); // first cut-off waits for a different connection
      yield* h.reconnect(2);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(2)));
      yield* settle(); // several periodic ticks
      yield* h.reconnect(3);
      yield* settle();
      expect(h.calls()).toBe(2);
      retryEnvironmentQuery(h.atom, () => h.registry.refresh(h.atom));
      yield* Effect.promise(() =>
        vi.waitFor(() => expect(AsyncResult.isSuccess(h.registry.get(h.atom))).toBe(true)),
      );
      h.registry.dispose();
    }),
  );

  it.effect("an automatic refresh during the re-issue does not use up the automatic budget", () =>
    Effect.gen(function* () {
      const h = yield* makeHarness(["cut", "blocked", "ok"]);
      h.registry.mount(h.atom);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
      yield* h.reconnect(2);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(2)));
      // The one re-issue is in flight; an automatic re-read interrupts and re-evaluates it.
      h.registry.refresh(h.atom);
      yield* Effect.promise(() =>
        vi.waitFor(() => expect(AsyncResult.isSuccess(h.registry.get(h.atom))).toBe(true)),
      );
      expect(h.calls()).toBe(3);
      expect(isEnvironmentQueryAwaitingRetry(h.atom, h.registry.get(h.atom))).toBe(false);
      h.registry.dispose();
    }),
  );

  it.effect("a typed failure of the re-issue waits for Retry like a second cut-off", () =>
    Effect.gen(function* () {
      const h = yield* makeHarness(["cut", "typed", "ok"]);
      h.registry.mount(h.atom);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
      yield* h.reconnect(2);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(2)));
      yield* Effect.promise(() =>
        vi.waitFor(() =>
          expect(isEnvironmentQueryAwaitingRetry(h.atom, h.registry.get(h.atom))).toBe(true),
        ),
      );
      h.registry.refresh(h.atom);
      yield* h.reconnect(3);
      yield* settle();
      expect(h.calls()).toBe(2);
      expect(AsyncResult.isFailure(h.registry.get(h.atom))).toBe(true);
      retryEnvironmentQuery(h.atom, () => h.registry.refresh(h.atom));
      yield* Effect.promise(() =>
        vi.waitFor(() => expect(AsyncResult.isSuccess(h.registry.get(h.atom))).toBe(true)),
      );
      expect(h.calls()).toBe(3);
      h.registry.dispose();
    }),
  );

  it.effect("runs one re-issue at a time for queries that share a request", () =>
    Effect.gen(function* () {
      const h = yield* makeHarness(["cut", "blocked", "ok"]);
      const first = h.remountAtom("first-view");
      const second = h.remountAtom("second-view");
      h.registry.mount(first);
      h.registry.mount(second);
      // One request goes out; the other view shows the same cut-off on this connection.
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
      yield* h.reconnect(2);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(2)));
      yield* settle();
      // The other view waits for the in-flight re-issue instead of starting another or failing.
      expect(h.calls()).toBe(2);
      for (const view of [first, second]) {
        const result = h.registry.get(view);
        expect(AsyncResult.isFailure(result) && !result.waiting).toBe(false);
      }
      yield* h.release();
      yield* Effect.promise(() =>
        vi.waitFor(() => {
          expect(AsyncResult.isSuccess(h.registry.get(first))).toBe(true);
          expect(AsyncResult.isSuccess(h.registry.get(second))).toBe(true);
        }),
      );
      expect(h.calls()).toBe(3);
      h.registry.dispose();
    }),
  );

  it.effect("views that wake together from one attempt start one follow-up", () =>
    Effect.gen(function* () {
      // With three operations per scheduler slice, two waiting views resume from the
      // same finished attempt and interleave step by step: a check and a separate,
      // later mark would let both start a request. (A budget below 3 never runs a
      // real step after a resume.)
      const h = yield* makeHarness(["blocked", "held", "held", "ok"], undefined, 3);
      const views = ["first-view", "second-view", "third-view"].map(h.remountAtom);
      for (const view of views) h.registry.mount(view);
      // One request goes out; the other two views wait for it.
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
      yield* settle();
      expect(h.calls()).toBe(1);
      // It succeeds; the two waiting views wake together. Only one of them sends.
      yield* h.release();
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBeGreaterThanOrEqual(2)));
      yield* settle();
      expect(h.calls()).toBe(2);
      yield* h.releaseHeld();
      yield* Effect.promise(() =>
        vi.waitFor(() => {
          for (const view of views) expect(AsyncResult.isSuccess(h.registry.get(view))).toBe(true);
        }),
      );
      h.registry.dispose();
    }),
  );

  it.effect("counts a view torn down by a lost session as a cut-off", () =>
    Effect.gen(function* () {
      // A view that unmounts when its environment disconnects (Git Manager shows
      // "Reconnecting…") interrupts the query before the RpcClientError arrives.
      const h = yield* makeHarness(["blocked", "blocked", "ok"]);
      const mountFresh = () => {
        const registry = AtomRegistry.make();
        registry.mount(h.atom);
        return registry;
      };
      let registry = mountFresh();
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
      yield* h.loseSession(1);
      registry.dispose();
      yield* h.reconnect(2);
      registry = mountFresh();
      // The first interrupted attempt was a cut-off: this is the one re-issue.
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(2)));
      yield* h.loseSession(2);
      registry.dispose();
      yield* h.reconnect(3);
      registry = mountFresh();
      yield* settle();
      expect(h.calls()).toBe(2);
      expect(AsyncResult.isFailure(registry.get(h.atom))).toBe(true);
      expect(isEnvironmentQueryAwaitingRetry(h.atom, registry.get(h.atom))).toBe(true);
      registry.dispose();
    }),
  );

  it.effect("does not count a refresh on the live session as a cut-off", () =>
    Effect.gen(function* () {
      const h = yield* makeHarness(["blocked", "ok"]);
      h.registry.mount(h.atom);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
      h.registry.refresh(h.atom);
      yield* Effect.promise(() =>
        vi.waitFor(() => expect(AsyncResult.isSuccess(h.registry.get(h.atom))).toBe(true)),
      );
      expect(h.calls()).toBe(2);
      expect(isEnvironmentQueryAwaitingRetry(h.atom, h.registry.get(h.atom))).toBe(false);
      h.registry.dispose();
    }),
  );

  it.effect("classifies an interrupt that races a session swap by its own generation", () =>
    Effect.gen(function* () {
      // The supervisor moves straight to a newer connected session; the old
      // attempt's session is gone even though the phase reads "connected".
      const h = yield* makeHarness(["blocked", "cut", "ok"]);
      h.registry.mount(h.atom);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
      yield* h.reconnect(2);
      // The interrupted attempt was a cut-off, so this is the one re-issue; it fails.
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(2)));
      yield* Effect.promise(() =>
        vi.waitFor(() =>
          expect(isEnvironmentQueryAwaitingRetry(h.atom, h.registry.get(h.atom))).toBe(true),
        ),
      );
      yield* h.reconnect(3);
      yield* settle();
      expect(h.calls()).toBe(2);
      h.registry.dispose();
    }),
  );

  it.effect("counts a request interrupted as its session closes, before the drop is reported", () =>
    Effect.gen(function* () {
      // The RPC client interrupts a closing session's requests while the supervisor
      // still reads "connected"; the next evaluation's newer generation settles it.
      const h = yield* makeHarness(["blocked", "blocked", "ok"]);
      const mountFresh = () => {
        const registry = AtomRegistry.make();
        registry.mount(h.atom);
        return registry;
      };
      let registry = mountFresh();
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
      registry.dispose();
      yield* h.reconnect(2);
      registry = mountFresh();
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(2)));
      registry.dispose();
      yield* h.reconnect(3);
      registry = mountFresh();
      yield* settle();
      expect(h.calls()).toBe(2);
      expect(AsyncResult.isFailure(registry.get(h.atom))).toBe(true);
      expect(isEnvironmentQueryAwaitingRetry(h.atom, registry.get(h.atom))).toBe(true);
      registry.dispose();
    }),
  );
});

// The Files panel's list (`fileTreeEntries`) relies on this: with no idle retention, a remount
// reads again even while the last value would still count as fresh.
describe("createEnvironmentQueryAtomFamily without idle retention", () => {
  it.effect("reads again on every mount", () =>
    Effect.gen(function* () {
      const h = yield* makeHarness([], undefined, undefined, {
        staleTimeMs: 60_000,
        idleTtlMs: 0,
      });
      const unmount = h.registry.mount(h.atom);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(1)));
      unmount();
      yield* settle();
      h.registry.mount(h.atom);
      yield* Effect.promise(() => vi.waitFor(() => expect(h.calls()).toBe(2)));
      h.registry.dispose();
    }),
  );
});
