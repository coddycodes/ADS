import {
  EnvironmentId,
  WS_METHODS,
  type GitManagerCommitPage,
  type GitManagerGetCommitsInput,
  type GitManagerSignalEvent,
} from "@coddy/contracts";
import { afterEach, vi } from "vite-plus/test";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Deferred from "effect/Deferred";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as PubSub from "effect/PubSub";
import * as Wakeups from "../connection/wakeups.ts";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";

import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import type { RpcSession } from "../rpc/session.ts";
import { createGitManagerEnvironmentAtoms } from "./gitManager.ts";

const ENVIRONMENT_ID = EnvironmentId.make("git-manager-test");
const TARGET = { environmentId: ENVIRONMENT_ID, input: { cwd: "/repo" } };

const makeHarness = Effect.fn("TestGitManager.makeHarness")(function* (
  watcherDegraded?: boolean,
  getCommits?: (input: GitManagerGetCommitsInput) => Effect.Effect<GitManagerCommitPage>,
) {
  const calls: string[] = [];
  const wakeups = yield* PubSub.unbounded<void>();
  const wakeupSubscriptions = { active: 0 };
  const wakeupChanges = Stream.unwrap(
    Effect.sync(() => {
      wakeupSubscriptions.active += 1;
      return Stream.fromPubSub(wakeups).pipe(
        Stream.ensuring(
          Effect.sync(() => {
            wakeupSubscriptions.active -= 1;
          }),
        ),
      );
    }),
  );
  const health = yield* SubscriptionRef.make<GitManagerSignalEvent>({
    cwd: "/repo",
    generation: 1,
    ...(watcherDegraded === undefined ? {} : { watcherDegraded }),
  });
  const read = (tag: string, result: unknown) => (input: { cwd: string }) =>
    Effect.sync(() => {
      calls.push(`${tag}:${input.cwd}`);
      return result;
    });
  const session: RpcSession = {
    client: {
      [WS_METHODS.gitManagerGetRefs]: read("refs", {
        generation: 1,
        localBranches: [],
        remoteBranches: [],
        tags: [],
        worktrees: [],
        remotes: [],
        defaultBranch: null,
        headRef: null,
        detachedSha: null,
        isDirty: false,
        inProgressOperation: null,
        conflictedPaths: [],
      }),
      [WS_METHODS.gitManagerGetCommits]:
        getCommits ??
        read("commits", {
          generation: 1,
          pinnedTips: [],
          commits: [],
          nextOffset: null,
          exhausted: true,
          degradedToAllPaging: false,
        }),
      [WS_METHODS.gitManagerGetStashes]: read("stashes", []),
      [WS_METHODS.subscribeGitManagerSignal]: () => SubscriptionRef.changes(health),
    } as never,
    initialConfig: Effect.never,
    ready: Effect.void,
    probe: Effect.void,
    closed: Effect.never,
    e2eeAuthenticated: Effect.succeed(null),
  };
  const connection = yield* SubscriptionRef.make({ phase: "connected", generation: 1 });
  const supervisor = EnvironmentSupervisor.of({
    target: { environmentId: ENVIRONMENT_ID, label: "Git test" },
    session: yield* SubscriptionRef.make(Option.some(session)),
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
  const atoms = createGitManagerEnvironmentAtoms(
    Atom.runtime(
      Layer.mergeAll(
        Layer.succeed(EnvironmentRegistry, environment),
        Wakeups.layer({
          changes: Stream.never,
          focusVisibility: wakeupChanges.pipe(Stream.map(() => undefined)),
        }),
      ),
    ),
  );
  return {
    atoms,
    calls,
    connection,
    health,
    wakeups,
    wakeupSubscriptions,
    registry: AtomRegistry.make(),
  };
});

afterEach(() => vi.unstubAllGlobals());

describe("Git Manager query focus refresh", () => {
  it.effect(
    "plain signals do not subscribe to platform wakeups, while the explicit fallback has a shared lifetime",
    () =>
      Effect.gen(function* () {
        const h = yield* makeHarness(true);
        try {
          const signal = h.atoms.signal(TARGET);
          h.registry.mount(signal);
          yield* Effect.promise(() =>
            vi.waitFor(() => expect(AsyncResult.isSuccess(h.registry.get(signal))).toBe(true)),
          );
          expect(h.wakeupSubscriptions.active).toBe(0);
          h.registry.mount(h.atoms.signalWithDegradedFocusRefresh(TARGET));
          h.registry.mount(h.atoms.signalWithDegradedFocusRefresh({ ...TARGET }));
          yield* Effect.promise(() =>
            vi.waitFor(() => expect(h.wakeupSubscriptions.active).toBe(1)),
          );
        } finally {
          h.registry.dispose();
        }
        yield* Effect.promise(() => vi.waitFor(() => expect(h.wakeupSubscriptions.active).toBe(0)));
      }),
  );

  it.effect.each([true, false, undefined])(
    "refreshes each active query once only when degraded (%s)",
    (degraded) =>
      Effect.gen(function* () {
        const h = yield* makeHarness(degraded);
        try {
          const signal = h.atoms.signalWithDegradedFocusRefresh(TARGET);
          h.registry.mount(signal);
          h.registry.mount(h.atoms.getRefs(TARGET));
          // Multiple mounted consumers share the same query and focus subscription.
          h.registry.mount(h.atoms.getRefs({ ...TARGET }));
          h.registry.mount(h.atoms.getCommits(TARGET));
          h.registry.mount(h.atoms.getStashes(TARGET));
          const unrelated = h.atoms.getRefs({ ...TARGET, input: { cwd: "/other" } });
          h.registry.mount(unrelated);
          yield* Effect.promise(() =>
            vi.waitFor(() => {
              expect(h.calls).toHaveLength(4);
              expect(AsyncResult.isSuccess(h.registry.get(signal))).toBe(true);
            }),
          );
          yield* PubSub.publish(h.wakeups, undefined);
          yield* Effect.promise(() =>
            vi.waitFor(() => expect(h.calls).toHaveLength(degraded ? 7 : 4)),
          );
          expect(h.calls.filter((call) => call === "refs:/other")).toHaveLength(1);
          for (const tag of ["refs", "commits", "stashes"]) {
            expect(h.calls.filter((call) => call === `${tag}:/repo`)).toHaveLength(
              degraded ? 2 : 1,
            );
          }
          h.registry.refresh(h.atoms.getRefs(TARGET));
          yield* Effect.promise(() =>
            vi.waitFor(() =>
              expect(h.calls.filter((call) => call === "refs:/repo")).toHaveLength(
                degraded ? 3 : 2,
              ),
            ),
          );
        } finally {
          h.registry.dispose();
        }
      }),
  );
});

describe("Git Manager stash list lifetime", () => {
  it.effect("drops the stash list with its last consumer, so a reconnect does not re-read it", () =>
    Effect.gen(function* () {
      const h = yield* makeHarness(false);
      try {
        h.registry.mount(h.atoms.getRefs(TARGET));
        const closeStashes = h.registry.mount(h.atoms.getStashes(TARGET));
        yield* Effect.promise(() =>
          vi.waitFor(() => expect([...h.calls].sort()).toEqual(["refs:/repo", "stashes:/repo"])),
        );
        closeStashes();
        // Idle disposal runs after the last consumer leaves; a reconnect comes later.
        const stashNodeAlive = () =>
          [...h.registry.getNodes().values()].some((node) =>
            node.atom.label?.[0].includes("get-stashes"),
          );
        yield* Effect.promise(() => vi.waitFor(() => expect(stashNodeAlive()).toBe(false)));
        yield* SubscriptionRef.set(h.connection, { phase: "connected", generation: 2 });
        // The mounted refs query re-reads for the new connection; the closed stash list does not.
        yield* Effect.promise(() =>
          vi.waitFor(() => expect(h.calls.filter((call) => call === "refs:/repo")).toHaveLength(2)),
        );
        expect(h.calls.filter((call) => call === "stashes:/repo")).toHaveLength(1);
      } finally {
        h.registry.dispose();
      }
    }),
  );
});

describe("retained Git Manager history pages", () => {
  it.effect("reads a fresh first page per cache key and shares identical mounted requests", () =>
    Effect.gen(function* () {
      const requests: GitManagerGetCommitsInput[] = [];
      const h = yield* makeHarness(false, (input) =>
        Effect.sync(() => {
          requests.push(input);
          return {
            generation: requests.length,
            pinnedTips: [],
            commits: [],
            nextOffset: null,
            exhausted: true,
            degradedToAllPaging: false,
          };
        }),
      );
      try {
        const target = {
          environmentId: ENVIRONMENT_ID,
          input: { cwd: "/repo", limit: 100, refreshCacheKey: "_r_1_0" },
        };
        const first = h.atoms.getHistoryFirstPage(target);
        h.registry.mount(first);
        h.registry.mount(h.atoms.getHistoryFirstPage({ ...target, input: { ...target.input } }));
        yield* Effect.promise(() =>
          vi.waitFor(() => expect(AsyncResult.isSuccess(h.registry.get(first))).toBe(true)),
        );
        expect(requests).toEqual([{ cwd: "/repo", limit: 100 }]);
        const next = h.atoms.getHistoryFirstPage({
          ...target,
          input: { ...target.input, refreshCacheKey: "_r_1_1" },
        });
        h.registry.mount(next);
        yield* Effect.promise(() =>
          vi.waitFor(() => expect(AsyncResult.isSuccess(h.registry.get(next))).toBe(true)),
        );
        expect(requests).toEqual([
          { cwd: "/repo", limit: 100 },
          { cwd: "/repo", limit: 100 },
        ]);
        const result = h.registry.get(next);
        if (AsyncResult.isSuccess(result)) expect(result.value.generation).toBe(2);
      } finally {
        h.registry.dispose();
      }
    }),
  );

  it.effect("preserves pinned page requests, limits in-flight reads, and cancels on unmount", () =>
    Effect.gen(function* () {
      const releases = yield* Effect.forEach([0, 1, 2, 3], () => Deferred.make<void>());
      const started: GitManagerGetCommitsInput[] = [];
      let active = 0;
      let maximumActive = 0;
      const h = yield* makeHarness(
        false,
        Effect.fn("TestGitManager.readRetainedPage")(function* (input) {
          started.push(input);
          active += 1;
          maximumActive = Math.max(maximumActive, active);
          yield* Deferred.await(releases[(input.offset ?? 0) / 50]!).pipe(
            Effect.ensuring(
              Effect.sync(() => {
                active -= 1;
              }),
            ),
          );
          return {
            generation: 1,
            pinnedTips: input.pinnedTips ?? [],
            commits: [],
            nextOffset: null,
            exhausted: true,
            degradedToAllPaging: false,
          };
        }),
      );
      try {
        const atom = h.atoms.getRetainedCommitPages({
          environmentId: ENVIRONMENT_ID,
          input: {
            cwd: "/repo",
            pinnedTips: ["original-tip"],
            offsets: [0, 50, 100, 150],
            limit: 50,
            refreshCacheKey: "_r_1_2",
          },
        });
        const unmount = h.registry.mount(atom);
        yield* Effect.promise(() => vi.waitFor(() => expect(started).toHaveLength(2)));
        expect(active).toBe(2);
        yield* Deferred.succeed(releases[0]!, undefined);
        yield* Effect.promise(() => vi.waitFor(() => expect(started).toHaveLength(3)));
        expect(started).toEqual(
          [0, 50, 100].map((offset) => ({
            cwd: "/repo",
            pinnedTips: ["original-tip"],
            offset,
            limit: 50,
          })),
        );
        expect(maximumActive).toBe(2);
        unmount();
        yield* Effect.promise(() => vi.waitFor(() => expect(active).toBe(0)));
        expect(started).toHaveLength(3);
      } finally {
        h.registry.dispose();
      }
    }),
  );
});
