// @effect-diagnostics nodeBuiltinImport:off - Cross-language parity reads generated RPC fixtures.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import {
  EnvironmentId,
  GitCommandError,
  GitManagerError,
  WS_METHODS,
  type VcsStatusSummary,
} from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Clock from "effect/Clock";
import * as Cause from "effect/Cause";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import * as TestClock from "effect/testing/TestClock";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import { RpcClientError } from "effect/rpc";

import { AVAILABLE_CONNECTION_STATE, type SupervisorConnectionState } from "../connection/model.ts";
import type { ConnectionCatalogEntry } from "../connection/catalog.ts";
import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import type { WsRpcProtocolClient } from "../rpc/protocol.ts";
import type { RpcSession } from "../rpc/session.ts";
import { isAtomCommandInterrupted, runAtomCommand } from "./runtime.ts";
import { createVcsEnvironmentAtoms, projectVcsStatusEvent } from "./vcs.ts";

const ENVIRONMENT_ID = EnvironmentId.make("environment-1");
const decodeGitCommandError = Schema.decodeUnknownSync(GitCommandError);
const TARGET = {
  environmentId: ENVIRONMENT_ID,
  input: { cwd: "/repo" },
} as const;

function summary(refName: string): VcsStatusSummary {
  return {
    isRepo: true,
    refName,
    detachedHead: null,
    hasWorkingTreeChanges: false,
    sourceControlProvider: null,
    pr: null,
    observedAt: "2026-08-20T12:00:00.000Z",
    stale: false,
  };
}

const legacySnapshot = {
  _tag: "snapshot" as const,
  local: {
    isRepo: true,
    hasPrimaryRemote: false,
    isDefaultRef: false,
    refName: "legacy/main",
    hasWorkingTreeChanges: false,
    workingTree: { files: [], insertions: 0, deletions: 0 },
  },
  remote: null,
};

function session(client: WsRpcProtocolClient, summaryCapability: boolean | undefined): RpcSession {
  const capabilities =
    summaryCapability === undefined ? {} : { vcsStatusSummary: summaryCapability };
  return {
    client,
    initialConfig: Effect.succeed({ environment: { capabilities } } as never),
    ready: Effect.void,
    probe: Effect.void,
    closed: Effect.never,
    e2eeAuthenticated: Effect.succeed(null),
  };
}

const makeHarness = Effect.fn("TestVcs.makeHarness")(function* (initialSession: RpcSession) {
  const clock = yield* Clock.Clock;
  const sessionRef = yield* SubscriptionRef.make<Option.Option<RpcSession>>(
    Option.some(initialSession),
  );
  const supervisor = EnvironmentSupervisor.of({
    target: { environmentId: ENVIRONMENT_ID, label: "Environment" },
    session: sessionRef,
  } as never);
  const environmentRegistry = EnvironmentRegistry.of({
    followStream: (_environmentId: EnvironmentId, stream: Stream.Stream<unknown, unknown>) =>
      Stream.provideService(stream, EnvironmentSupervisor, supervisor),
  } as never);
  const vcs = createVcsEnvironmentAtoms(
    Atom.runtime(
      Layer.mergeAll(
        Layer.succeed(EnvironmentRegistry, environmentRegistry),
        Layer.succeed(Clock.Clock, clock),
      ),
    ),
  );
  return { atomRegistry: AtomRegistry.make(), sessionRef, vcs };
});

function readRefName<A extends { readonly refName: string | null }, E>(
  atomRegistry: AtomRegistry.AtomRegistry,
  atom: Atom.Atom<AsyncResult.AsyncResult<A, E>>,
  refName: string,
) {
  return Effect.gen(function* () {
    for (let attempt = 0; attempt < 100; attempt += 1) {
      const result = atomRegistry.get(atom);
      if (AsyncResult.isSuccess(result) && result.value.refName === refName) {
        return result.value;
      }
      yield* Effect.yieldNow;
    }
    return yield* Effect.die(`VCS ref ${refName} was not observed`);
  });
}

function waitFor(predicate: () => boolean, message: string) {
  return Effect.gen(function* () {
    for (let attempt = 0; attempt < 100; attempt += 1) {
      if (predicate()) return;
      yield* Effect.yieldNow;
    }
    return yield* Effect.die(message);
  });
}

function readFellBehindFailure(kind: "live" | "setup"): GitCommandError {
  const fixture = JSON.parse(
    NodeFS.readFileSync(
      NodePath.resolve(
        import.meta.dirname,
        "../../../contracts/fixtures/rpc-wire/contract-shapes",
        `subscribeVcsStatus__fell-behind-${kind}-failure.json`,
      ),
      "utf8",
    ),
  ) as {
    readonly exit: { readonly cause: readonly [{ readonly error: unknown }] };
  };
  return decodeGitCommandError(fixture.exit.cause[0].error);
}

describe("VCS status projection", () => {
  // Every reconnect resends the snapshot. A new status object re-renders every reader, and each
  // render's closures pinned another copy of a large workspace's file list (#96 follow-up).
  it("keeps the status when an event leaves it unchanged", () => {
    const [first, emitted] = projectVcsStatusEvent(null, legacySnapshot);
    expect(emitted).toEqual([first]);

    const [resent, resentEmitted] = projectVcsStatusEvent(first, structuredClone(legacySnapshot));
    expect(resent).toBe(first);
    expect(resentEmitted).toEqual([]);

    const changed = {
      ...legacySnapshot,
      local: { ...legacySnapshot.local, hasWorkingTreeChanges: true },
    };
    const [next, nextEmitted] = projectVcsStatusEvent(first, changed);
    expect(next).not.toBe(first);
    expect(nextEmitted).toEqual([next]);
  });
});

describe("VCS status atoms", () => {
  const fellBehind = readFellBehindFailure("live");

  it.effect.each(["live", "setup"] as const)(
    "retains its last status through a %s fell-behind wire failure and replaces it with the snapshot",
    (kind) =>
      Effect.gen(function* () {
        let calls = 0;
        const failure = yield* Deferred.make<never, GitCommandError>();
        const recovered = {
          ...legacySnapshot,
          local: {
            ...legacySnapshot.local,
            refName: "recovered/main",
            hasWorkingTreeChanges: true,
          },
        };
        const client = {
          [WS_METHODS.subscribeVcsStatus]: () => {
            calls += 1;
            return calls === 1
              ? Stream.make(legacySnapshot).pipe(
                  Stream.concat(Stream.fromEffect(Deferred.await(failure))),
                )
              : Stream.make(recovered);
          },
        } as unknown as WsRpcProtocolClient;
        const harness = yield* makeHarness(session(client, true));
        const atom = harness.vcs.status(TARGET);
        const unmount = harness.atomRegistry.mount(atom);
        const initial = yield* readRefName(harness.atomRegistry, atom, "legacy/main");
        const observed: string[] = [];
        const unsubscribe = harness.atomRegistry.subscribe(atom, (value) =>
          observed.push(value._tag),
        );

        yield* Deferred.fail(failure, readFellBehindFailure(kind));
        for (let attempt = 0; attempt < 10; attempt += 1) yield* Effect.yieldNow;
        yield* TestClock.adjust(249);
        expect(calls).toBe(1);
        const retained = harness.atomRegistry.get(atom);
        expect(AsyncResult.isSuccess(retained) ? retained.value : null).toBe(initial);
        yield* TestClock.adjust(1);
        const value = yield* readRefName(harness.atomRegistry, atom, "recovered/main");
        expect(value.hasWorkingTreeChanges).toBe(true);
        expect(calls).toBe(2);
        expect(observed.length).toBeGreaterThan(0);
        expect(observed.every((tag) => tag === "Success")).toBe(true);
        yield* TestClock.adjust("1 minute");
        expect(calls).toBe(2);

        unsubscribe();
        unmount();
        harness.atomRegistry.dispose();
      }),
  );

  it.effect.each(["Git.status", "GitStatusBroadcaster.subscribe"])(
    "fails without resubscribing for %s",
    (operation) =>
      Effect.gen(function* () {
        let calls = 0;
        const error = new GitCommandError({
          operation,
          command: "git",
          cwd: "/repo",
          detail: "failed",
        });
        const client = {
          [WS_METHODS.subscribeVcsStatus]: () => {
            calls += 1;
            return Stream.fail(error);
          },
        } as unknown as WsRpcProtocolClient;
        const harness = yield* makeHarness(session(client, true));
        const atom = harness.vcs.status(TARGET);
        const unmount = harness.atomRegistry.mount(atom);
        yield* waitFor(
          () => AsyncResult.isFailure(harness.atomRegistry.get(atom)),
          "status did not fail",
        );
        const result = harness.atomRegistry.get(atom);
        expect(AsyncResult.isFailure(result) ? Cause.squash(result.cause) : null).toBe(error);
        yield* TestClock.adjust("1 minute");
        expect(calls).toBe(1);
        unmount();
        harness.atomRegistry.dispose();
      }),
  );

  it.effect("does not resubscribe after a clean status stream end", () =>
    Effect.gen(function* () {
      let calls = 0;
      const client = {
        [WS_METHODS.subscribeVcsStatus]: () => {
          calls += 1;
          return Stream.make(legacySnapshot);
        },
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeHarness(session(client, true));
      const atom = harness.vcs.status(TARGET);
      const unmount = harness.atomRegistry.mount(atom);
      yield* readRefName(harness.atomRegistry, atom, "legacy/main");
      yield* TestClock.adjust("2 minutes");
      expect(calls).toBe(1);
      expect(AsyncResult.isSuccess(harness.atomRegistry.get(atom))).toBe(true);
      unmount();
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("doubles fell-behind waits up to 30 seconds and resets after 30 seconds up", () =>
    Effect.gen(function* () {
      const failures: Deferred.Deferred<never, GitCommandError>[] = [];
      const client = {
        [WS_METHODS.subscribeVcsStatus]: () =>
          Stream.unwrap(
            Effect.gen(function* () {
              const failure = yield* Deferred.make<never, GitCommandError>();
              failures.push(failure);
              return Stream.make({
                ...legacySnapshot,
                local: { ...legacySnapshot.local, refName: `attempt-${failures.length}` },
              }).pipe(Stream.concat(Stream.fromEffect(Deferred.await(failure))));
            }),
          ),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeHarness(session(client, true));
      const atom = harness.vcs.status(TARGET);
      const unmount = harness.atomRegistry.mount(atom);
      yield* readRefName(harness.atomRegistry, atom, "attempt-1");

      for (const [index, delay] of [
        250, 500, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000,
      ].entries()) {
        yield* Deferred.fail(failures[index]!, fellBehind);
        for (let attempt = 0; attempt < 10; attempt += 1) yield* Effect.yieldNow;
        yield* TestClock.adjust(delay - 1);
        expect(failures).toHaveLength(index + 1);
        expect(AsyncResult.isSuccess(harness.atomRegistry.get(atom))).toBe(true);
        yield* TestClock.adjust(1);
        yield* readRefName(harness.atomRegistry, atom, `attempt-${index + 2}`);
        expect(failures).toHaveLength(index + 2);
      }

      yield* TestClock.adjust(29_999);
      yield* Deferred.fail(failures[9]!, fellBehind);
      for (let attempt = 0; attempt < 10; attempt += 1) yield* Effect.yieldNow;
      yield* TestClock.adjust(29_999);
      expect(failures).toHaveLength(10);
      yield* TestClock.adjust(1);
      yield* readRefName(harness.atomRegistry, atom, "attempt-11");

      yield* TestClock.adjust(30_000);
      yield* Deferred.fail(failures[10]!, fellBehind);
      for (let attempt = 0; attempt < 10; attempt += 1) yield* Effect.yieldNow;
      yield* TestClock.adjust(249);
      expect(failures).toHaveLength(11);
      yield* TestClock.adjust(1);
      yield* readRefName(harness.atomRegistry, atom, "attempt-12");
      expect(failures).toHaveLength(12);
      unmount();
      harness.atomRegistry.dispose();
    }),
  );
});

describe("VCS summary atoms", () => {
  it.effect("retries an expected summary failure once at the passive freshness boundary", () =>
    Effect.gen(function* () {
      let calls = 0;
      const recovered = summary("summary/recovered");
      const expectedFailure = new GitManagerError({
        operation: "summary",
        cwd: "/sensitive/repository",
        detail: "sensitive provider output",
      });
      const client = {
        [WS_METHODS.subscribeVcsStatusSummary]: () => {
          calls += 1;
          return calls === 1
            ? Stream.fail(expectedFailure)
            : Stream.make(recovered).pipe(Stream.concat(Stream.fail(expectedFailure)));
        },
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeHarness(session(client, true));
      const atom = harness.vcs.summary(TARGET);
      const unmount = harness.atomRegistry.mount(atom);

      yield* waitFor(() => calls === 1, "first summary subscription did not start");
      for (let attempt = 0; attempt < 10; attempt += 1) yield* Effect.yieldNow;
      yield* TestClock.adjust("29 seconds");
      expect(calls).toBe(1);

      yield* TestClock.adjust("1 second");
      expect((yield* readRefName(harness.atomRegistry, atom, recovered.refName!)).refName).toBe(
        recovered.refName,
      );
      expect(calls).toBe(2);

      for (let attempt = 0; attempt < 10; attempt += 1) yield* Effect.yieldNow;
      yield* TestClock.adjust("29 seconds");
      const retained = harness.atomRegistry.get(atom);
      expect(AsyncResult.isSuccess(retained) ? retained.value : null).toBe(recovered);
      expect(calls).toBe(2);

      unmount();
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("cancels a pending summary retry when the session disconnects", () =>
    Effect.gen(function* () {
      const calls = { initial: 0, reconnected: 0 };
      const active = { count: 0, maximum: 0 };
      let finalized = 0;
      const expectedFailure = new GitManagerError({
        operation: "summary",
        cwd: "/sensitive/repository",
        detail: "sensitive provider output",
      });
      const initialClient = {
        [WS_METHODS.subscribeVcsStatusSummary]: () => {
          calls.initial += 1;
          return Stream.fail(expectedFailure);
        },
      } as unknown as WsRpcProtocolClient;
      const reconnected = summary("summary/reconnected-after-failure");
      const reconnectedClient = {
        [WS_METHODS.subscribeVcsStatusSummary]: () => {
          calls.reconnected += 1;
          return Stream.fromEffect(
            Effect.sync(() => {
              active.count += 1;
              active.maximum = Math.max(active.maximum, active.count);
              return reconnected;
            }),
          ).pipe(
            Stream.concat(Stream.never),
            Stream.ensuring(
              Effect.sync(() => {
                active.count -= 1;
                finalized += 1;
              }),
            ),
          );
        },
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeHarness(session(initialClient, true));
      const atom = harness.vcs.summary(TARGET);
      const unmount = harness.atomRegistry.mount(atom);

      yield* waitFor(() => calls.initial === 1, "initial summary subscription did not start");
      for (let attempt = 0; attempt < 10; attempt += 1) yield* Effect.yieldNow;
      yield* TestClock.adjust("29 seconds");
      yield* SubscriptionRef.set(harness.sessionRef, Option.none());
      for (let attempt = 0; attempt < 10; attempt += 1) yield* Effect.yieldNow;
      yield* TestClock.adjust("1 minute");
      expect(calls).toEqual({ initial: 1, reconnected: 0 });

      yield* SubscriptionRef.set(harness.sessionRef, Option.some(session(reconnectedClient, true)));
      expect((yield* readRefName(harness.atomRegistry, atom, reconnected.refName!)).refName).toBe(
        reconnected.refName,
      );
      const observed = { calls: { ...calls }, active: { ...active } };
      unmount();
      yield* waitFor(() => finalized === 1, "reconnected summary stream survived unmount");
      harness.atomRegistry.dispose();

      expect(observed).toEqual({
        calls: { initial: 1, reconnected: 1 },
        active: { count: 1, maximum: 1 },
      });
    }),
  );

  it.effect("releases VCS streams promptly while switching status to summary", () =>
    Effect.gen(function* () {
      const active = { status: 0, summary: 0 };
      const finalized = { status: 0, summary: 0 };
      const client = {
        [WS_METHODS.subscribeVcsStatus]: () =>
          Stream.fromEffect(
            Effect.sync(() => {
              active.status += 1;
              return legacySnapshot;
            }),
          ).pipe(
            Stream.concat(Stream.never),
            Stream.ensuring(
              Effect.sync(() => {
                active.status -= 1;
                finalized.status += 1;
              }),
            ),
          ),
        [WS_METHODS.subscribeVcsStatusSummary]: () =>
          Stream.fromEffect(
            Effect.sync(() => {
              active.summary += 1;
              return summary("summary/main");
            }),
          ).pipe(
            Stream.concat(Stream.never),
            Stream.ensuring(
              Effect.sync(() => {
                active.summary -= 1;
                finalized.summary += 1;
              }),
            ),
          ),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeHarness(session(client, true));
      const statusAtom = harness.vcs.status(TARGET);
      const summaryAtom = harness.vcs.summary(TARGET);
      const unmountStatus = harness.atomRegistry.mount(statusAtom);
      yield* readRefName(harness.atomRegistry, statusAtom, "legacy/main");
      const unmountSummary = harness.atomRegistry.mount(summaryAtom);
      yield* readRefName(harness.atomRegistry, summaryAtom, "summary/main");

      unmountStatus();
      yield* waitFor(
        () => finalized.status === 1,
        "full-status stream was not finalized after switching to summary",
      );
      const afterSwitch = { active: { ...active }, finalized: { ...finalized } };
      unmountSummary();
      yield* waitFor(() => finalized.summary === 1, "summary stream was not finalized on unmount");
      const afterUnmount = { active: { ...active }, finalized: { ...finalized } };
      harness.atomRegistry.dispose();

      expect(afterSwitch).toEqual({
        active: { status: 0, summary: 1 },
        finalized: { status: 1, summary: 0 },
      });
      expect(afterUnmount).toEqual({
        active: { status: 0, summary: 0 },
        finalized: { status: 1, summary: 1 },
      });
    }),
  );

  it.effect("uses the passive summary stream when the server advertises it", () =>
    Effect.gen(function* () {
      const calls = { summary: 0, status: 0 };
      const client = {
        [WS_METHODS.subscribeVcsStatusSummary]: () => {
          calls.summary += 1;
          return Stream.make(summary("summary/main")).pipe(Stream.concat(Stream.never));
        },
        [WS_METHODS.subscribeVcsStatus]: () => {
          calls.status += 1;
          return Stream.make(legacySnapshot).pipe(Stream.concat(Stream.never));
        },
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeHarness(session(client, true));
      const atom = harness.vcs.summary(TARGET);
      const unmount = harness.atomRegistry.mount(atom);

      expect((yield* readRefName(harness.atomRegistry, atom, "summary/main")).refName).toBe(
        "summary/main",
      );
      expect(calls).toEqual({ summary: 1, status: 0 });

      unmount();
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("does not open a payload stream while the session capability is unresolved", () =>
    Effect.gen(function* () {
      const calls = { summary: 0, status: 0 };
      const client = {
        [WS_METHODS.subscribeVcsStatusSummary]: () => {
          calls.summary += 1;
          return Stream.never;
        },
        [WS_METHODS.subscribeVcsStatus]: () => {
          calls.status += 1;
          return Stream.never;
        },
      } as unknown as WsRpcProtocolClient;
      const unresolvedSession = {
        ...session(client, true),
        initialConfig: Effect.never,
      } satisfies RpcSession;
      const harness = yield* makeHarness(unresolvedSession);
      const unmount = harness.atomRegistry.mount(harness.vcs.summary(TARGET));

      for (let attempt = 0; attempt < 10; attempt += 1) {
        yield* Effect.yieldNow;
      }
      const callsWhileUnknown = { ...calls };
      unmount();
      harness.atomRegistry.dispose();

      expect(callsWhileUnknown).toEqual({ summary: 0, status: 0 });
    }),
  );

  it.effect.each([undefined, false])(
    "uses the legacy full-status stream when the capability is %s",
    (summaryCapability) =>
      Effect.gen(function* () {
        const calls = { summary: 0, status: 0 };
        const client = {
          [WS_METHODS.subscribeVcsStatusSummary]: () => {
            calls.summary += 1;
            return Stream.make(summary("wrong/summary"));
          },
          [WS_METHODS.subscribeVcsStatus]: () => {
            calls.status += 1;
            return Stream.make(legacySnapshot).pipe(Stream.concat(Stream.never));
          },
        } as unknown as WsRpcProtocolClient;
        const harness = yield* makeHarness(session(client, summaryCapability));
        const atom = harness.vcs.summary(TARGET);
        const unmount = harness.atomRegistry.mount(atom);

        const value = yield* readRefName(harness.atomRegistry, atom, "legacy/main");
        expect("workingTree" in value ? value.workingTree.files : null).toEqual([]);
        expect(calls).toEqual({ summary: 0, status: 1 });

        unmount();
        harness.atomRegistry.dispose();
      }),
  );

  it.effect.each([undefined, false])(
    "shares one full-status owner between status and summary when the capability is %s",
    (summaryCapability) =>
      Effect.gen(function* () {
        const calls = { summary: 0, status: 0 };
        let finalized = 0;
        const client = {
          [WS_METHODS.subscribeVcsStatusSummary]: () => {
            calls.summary += 1;
            return Stream.make(summary("wrong/summary"));
          },
          [WS_METHODS.subscribeVcsStatus]: () => {
            calls.status += 1;
            return Stream.make(legacySnapshot).pipe(
              Stream.concat(Stream.never),
              Stream.ensuring(
                Effect.sync(() => {
                  finalized += 1;
                }),
              ),
            );
          },
        } as unknown as WsRpcProtocolClient;
        const harness = yield* makeHarness(session(client, summaryCapability));
        const statusAtom = harness.vcs.status(TARGET);
        const summaryAtom = harness.vcs.summary(TARGET);
        const unmountStatus = harness.atomRegistry.mount(statusAtom);
        const unmountSummary = harness.atomRegistry.mount(summaryAtom);

        const statusValue = yield* readRefName(harness.atomRegistry, statusAtom, "legacy/main");
        const summaryValue = yield* readRefName(harness.atomRegistry, summaryAtom, "legacy/main");
        const callsWhileMounted = { ...calls };
        unmountStatus();
        yield* Effect.yieldNow;
        yield* Effect.yieldNow;
        const finalizedAfterFirstUnmount = finalized;
        unmountSummary();
        yield* waitFor(() => finalized === 1, "shared status stream survived its last owner");
        const finalizedAfterLastUnmount = finalized;
        harness.atomRegistry.dispose();

        expect(summaryValue).toBe(statusValue);
        expect(callsWhileMounted).toEqual({ summary: 0, status: 1 });
        expect(finalizedAfterFirstUnmount).toBe(0);
        expect(finalizedAfterLastUnmount).toBe(1);
      }),
  );

  it.effect("retains the last summary while reconnecting without overlapping subscriptions", () =>
    Effect.gen(function* () {
      const active = { count: 0, maximum: 0 };
      const finalized = { initial: 0, reconnected: 0 };
      const client = (kind: keyof typeof finalized, value: VcsStatusSummary) =>
        ({
          [WS_METHODS.subscribeVcsStatusSummary]: () =>
            Stream.fromEffect(
              Effect.sync(() => {
                active.count += 1;
                active.maximum = Math.max(active.maximum, active.count);
                return value;
              }),
            ).pipe(
              Stream.concat(Stream.never),
              Stream.ensuring(
                Effect.sync(() => {
                  active.count -= 1;
                  finalized[kind] += 1;
                }),
              ),
            ),
        }) as unknown as WsRpcProtocolClient;
      const first = summary("first");
      const second = summary("second");
      const harness = yield* makeHarness(session(client("initial", first), true));
      const atom = harness.vcs.summary(TARGET);
      const unmount = harness.atomRegistry.mount(atom);

      expect((yield* readRefName(harness.atomRegistry, atom, "first")).refName).toBe("first");
      yield* SubscriptionRef.set(harness.sessionRef, Option.none());
      yield* Effect.yieldNow;
      yield* Effect.yieldNow;
      const disconnected = harness.atomRegistry.get(atom);
      expect(AsyncResult.isSuccess(disconnected) ? disconnected.value : null).toBe(first);
      expect({ active, finalized }).toEqual({
        active: { count: 0, maximum: 1 },
        finalized: { initial: 1, reconnected: 0 },
      });

      yield* SubscriptionRef.set(
        harness.sessionRef,
        Option.some(session(client("reconnected", second), true)),
      );
      expect((yield* readRefName(harness.atomRegistry, atom, "second")).refName).toBe("second");
      expect(active).toEqual({ count: 1, maximum: 1 });

      unmount();
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("waits for reconnect capability before switching payload streams", () =>
    Effect.gen(function* () {
      const active = { count: 0, maximum: 0 };
      const finalized = { status: 0, summary: 0 };
      const tracked = <A>(kind: keyof typeof finalized, value: A) =>
        Stream.fromEffect(
          Effect.sync(() => {
            active.count += 1;
            active.maximum = Math.max(active.maximum, active.count);
            return value;
          }),
        ).pipe(
          Stream.concat(Stream.never),
          Stream.ensuring(
            Effect.sync(() => {
              active.count -= 1;
              finalized[kind] += 1;
            }),
          ),
        );
      const legacyClient = {
        [WS_METHODS.subscribeVcsStatus]: () => tracked("status", legacySnapshot),
      } as unknown as WsRpcProtocolClient;
      const summaryClient = {
        [WS_METHODS.subscribeVcsStatusSummary]: () =>
          tracked("summary", summary("summary/reconnected")),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeHarness(session(legacyClient, false));
      const atom = harness.vcs.summary(TARGET);
      const unmount = harness.atomRegistry.mount(atom);

      const first = yield* readRefName(harness.atomRegistry, atom, "legacy/main");
      yield* SubscriptionRef.set(harness.sessionRef, Option.none());
      yield* waitFor(() => finalized.status === 1, "legacy stream survived disconnect");
      const disconnected = harness.atomRegistry.get(atom);
      expect(AsyncResult.isSuccess(disconnected) ? disconnected.value : null).toBe(first);

      yield* SubscriptionRef.set(harness.sessionRef, Option.some(session(summaryClient, true)));
      yield* readRefName(harness.atomRegistry, atom, "summary/reconnected");
      const observed = { active: { ...active }, finalized: { ...finalized } };
      unmount();
      harness.atomRegistry.dispose();

      expect(observed).toEqual({
        active: { count: 1, maximum: 1 },
        finalized: { status: 1, summary: 0 },
      });
    }),
  );
});

const LOST_SOCKET = new RpcClientError.RpcClientError({
  reason: new RpcClientError.RpcClientDefect({
    message: "socket closed",
    cause: new Error("socket closed"),
  }),
});

function cloneSession(
  client: Record<string, (input: unknown) => Effect.Effect<unknown, unknown>>,
): RpcSession {
  return {
    client: client as unknown as WsRpcProtocolClient,
    initialConfig: Effect.succeed({
      environment: { capabilities: { vcsCloneReattach: true } },
    } as never),
    ready: Effect.void,
    probe: Effect.void,
    closed: Effect.never,
    e2eeAuthenticated: Effect.succeed(null),
  };
}

const makeCommandHarness = Effect.fn("TestVcs.makeCommandHarness")(function* (initial: RpcSession) {
  const sessionRef = yield* SubscriptionRef.make<Option.Option<RpcSession>>(Option.some(initial));
  const stateRef = yield* SubscriptionRef.make<SupervisorConnectionState>({
    ...AVAILABLE_CONNECTION_STATE,
    desired: true,
    phase: "connected",
  });
  const target = { environmentId: ENVIRONMENT_ID, label: "Environment" };
  const supervisor = EnvironmentSupervisor.of({
    target,
    session: sessionRef,
    state: stateRef,
  } as never);
  const run: EnvironmentRegistry["Service"]["run"] = (_environmentId, effect) =>
    Effect.provideService(effect, EnvironmentSupervisor, supervisor);
  // The clone loops follow the registry's current supervisor; here it never changes.
  const followStream: EnvironmentRegistry["Service"]["followStream"] = (_environmentId, stream) =>
    Stream.provideService(stream, EnvironmentSupervisor, supervisor);
  const entries = yield* SubscriptionRef.make<ReadonlyMap<EnvironmentId, ConnectionCatalogEntry>>(
    new Map([[ENVIRONMENT_ID, { target } as ConnectionCatalogEntry]]),
  );
  const environmentRegistry = EnvironmentRegistry.of({ run, followStream, entries } as never);
  const vcs = createVcsEnvironmentAtoms(
    Atom.runtime(Layer.succeed(EnvironmentRegistry, environmentRegistry)),
  );
  return { atomRegistry: AtomRegistry.make(), sessionRef, vcs };
});

describe("clone commands", () => {
  it.effect("cancels on its own lane while the clone holds the destination's lane", () =>
    Effect.gen(function* () {
      let cancelInput: unknown = null;
      const harness = yield* makeCommandHarness(
        cloneSession({
          [WS_METHODS.vcsClone]: () => Effect.never,
          [WS_METHODS.vcsCancelClone]: (input) => {
            cancelInput = input;
            return Effect.succeed({ cancelled: true });
          },
        }),
      );
      const controller = new AbortController();
      const input = { url: "https://example.test/demo.git", parentDir: "/code" };
      let cloneSettled = false;
      const clone = runAtomCommand(
        harness.atomRegistry,
        harness.vcs.clone,
        { environmentId: ENVIRONMENT_ID, input },
        { signal: controller.signal },
      ).then((result) => {
        cloneSettled = true;
        return result;
      });

      const cancelled = yield* Effect.promise(() =>
        runAtomCommand(harness.atomRegistry, harness.vcs.cancelClone, {
          environmentId: ENVIRONMENT_ID,
          input,
        }),
      );

      expect(AsyncResult.isSuccess(cancelled) ? cancelled.value : null).toEqual({
        cancelled: true,
      });
      expect(cancelInput).toEqual(input);
      expect(cloneSettled).toBe(false);
      controller.abort();
      expect(isAtomCommandInterrupted(yield* Effect.promise(() => clone))).toBe(true);
      harness.atomRegistry.dispose();
    }),
  );

  it.effect(
    "a retry after a lost cancel acknowledgement and a remount waits for that cancel and is not cancelled",
    () =>
      Effect.gen(function* () {
        const events: Array<string> = [];
        const input = { url: "https://example.test/demo.git", parentDir: "/code" };
        const harness = yield* makeCommandHarness(
          cloneSession({
            [WS_METHODS.vcsClone]: () => {
              events.push("clone");
              return Effect.never;
            },
            // The host cancels the clone, but the answer is lost with the socket.
            [WS_METHODS.vcsCancelClone]: () => {
              events.push("cancel sent, answer lost");
              return Effect.fail(LOST_SOCKET);
            },
          }),
        );
        const firstDialog = new AbortController();
        const first = runAtomCommand(
          harness.atomRegistry,
          harness.vcs.clone,
          { environmentId: ENVIRONMENT_ID, input },
          { signal: firstDialog.signal },
        );
        yield* waitFor(() => events.includes("clone"), "the first clone was not dispatched");
        const cancel = runAtomCommand(harness.atomRegistry, harness.vcs.cancelClone, {
          environmentId: ENVIRONMENT_ID,
          input,
        });
        yield* waitFor(
          () => events.includes("cancel sent, answer lost"),
          "the cancel was not sent",
        );

        // The dialog unmounts: its clone wait ends; the cancel stays registered and keeps retrying.
        firstDialog.abort();
        expect(isAtomCommandInterrupted(yield* Effect.promise(() => first))).toBe(true);

        // A remounted dialog retries into the same folder: the retry waits for the pending cancel.
        const secondDialog = new AbortController();
        const retry = runAtomCommand(
          harness.atomRegistry,
          harness.vcs.clone,
          { environmentId: ENVIRONMENT_ID, input },
          { signal: secondDialog.signal },
        );
        for (let attempt = 0; attempt < 50; attempt += 1) yield* Effect.yieldNow;
        expect(events).toEqual(["clone", "cancel sent, answer lost"]);

        // The host is reachable again: the cancel goes out once more and is acknowledged, and
        // only then does the retry dispatch. Nothing cancels the retry.
        yield* SubscriptionRef.set(
          harness.sessionRef,
          Option.some(
            cloneSession({
              [WS_METHODS.vcsClone]: () => {
                events.push("retry");
                return Effect.never;
              },
              [WS_METHODS.vcsCancelClone]: () => {
                events.push("cancel acknowledged");
                return Effect.succeed({ cancelled: true });
              },
            }),
          ),
        );
        const acknowledged = yield* Effect.promise(() => cancel);
        expect(AsyncResult.isSuccess(acknowledged) ? acknowledged.value : null).toEqual({
          cancelled: true,
        });
        yield* waitFor(() => events.includes("retry"), "the retry was not dispatched");
        expect(events).toEqual([
          "clone",
          "cancel sent, answer lost",
          "cancel acknowledged",
          "retry",
        ]);

        secondDialog.abort();
        yield* Effect.promise(() => retry);
        harness.atomRegistry.dispose();
      }),
  );

  it.effect("a cancel the host refuses releases its pending record at once", () =>
    Effect.gen(function* () {
      const events: Array<string> = [];
      const input = {
        url: "https://example.test/demo.git",
        parentDir: "/code",
        directoryName: "a/../b",
      };
      const harness = yield* makeCommandHarness(
        cloneSession({
          [WS_METHODS.vcsClone]: () => {
            events.push("clone");
            return Effect.succeed({ path: "/code/b" });
          },
          // The server refuses a path as the folder name with a typed failure.
          [WS_METHODS.vcsCancelClone]: () => {
            events.push("cancel refused");
            return Effect.fail(
              new GitCommandError({
                operation: "vcs.cancelClone",
                command: "git",
                cwd: "/code",
                detail: 'The folder name "a/../b" must be a single name, without slashes or "..".',
              }),
            );
          },
        }),
      );

      const refused = yield* Effect.promise(() =>
        runAtomCommand(
          harness.atomRegistry,
          harness.vcs.cancelClone,
          { environmentId: ENVIRONMENT_ID, input },
          { reportFailure: false },
        ),
      );
      expect(refused._tag).toBe("Failure");
      // The refused cancel left no pending record, so a clone into the folder dispatches at once.
      const cloned = yield* Effect.promise(() =>
        runAtomCommand(harness.atomRegistry, harness.vcs.clone, {
          environmentId: ENVIRONMENT_ID,
          input,
        }),
      );
      expect(AsyncResult.isSuccess(cloned) ? cloned.value : null).toEqual({ path: "/code/b" });
      expect(events).toEqual(["cancel refused", "clone"]);
      harness.atomRegistry.dispose();
    }),
  );
});
