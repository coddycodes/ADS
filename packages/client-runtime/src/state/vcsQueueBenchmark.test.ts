import { EnvironmentId, type VcsStatusResult, WS_METHODS } from "@bibcode/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { Atom, AtomRegistry } from "effect/reactivity";

import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import { createVcsEnvironmentAtoms } from "./vcs.ts";

function configuredPositiveInteger(name: string, fallback: number): number {
  const value = process.env[name];
  if (value === undefined) return fallback;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed <= 0) throw new Error(`${name} must be positive.`);
  return parsed;
}

function status(refName: string): VcsStatusResult {
  return {
    isRepo: true,
    hasPrimaryRemote: true,
    isDefaultRef: false,
    refName,
    hasWorkingTreeChanges: false,
    workingTree: { files: [], insertions: 0, deletions: 0 },
    hasUpstream: true,
    aheadCount: 0,
    behindCount: 0,
    pr: null,
  };
}

function percentile(sorted: readonly number[], fraction: number): number {
  return sorted[Math.ceil(sorted.length * fraction) - 1]!;
}

describe("production VCS Atom queue benchmark", () => {
  it.effect("measures mutation execution start while refreshStatus is active", () =>
    Effect.gen(function* () {
      const warmupSamples = configuredPositiveInteger("BIBCODE_VCS_QUEUE_WARMUPS", 20);
      const measuredSamples = configuredPositiveInteger("BIBCODE_VCS_QUEUE_SAMPLES", 200);
      const environmentId = EnvironmentId.make("vcs-queue-benchmark");
      let refreshStarted = yield* Deferred.make<void>();
      let refreshResult = yield* Deferred.make<VcsStatusResult>();
      let scheduledAt = 0;
      let collecting = false;
      const delays: number[] = [];
      const session = yield* SubscriptionRef.make(
        Option.some({
          client: {
            [WS_METHODS.vcsRefreshStatus]: () =>
              Deferred.succeed(refreshStarted, undefined).pipe(
                Effect.andThen(Deferred.await(refreshResult)),
              ),
            [WS_METHODS.vcsStageFiles]: () =>
              Effect.sync(() => {
                if (collecting) delays.push(performance.now() - scheduledAt);
              }),
          },
        } as never),
      );
      const supervisor = EnvironmentSupervisor.of({
        target: { environmentId, label: "VCS queue benchmark" },
        session,
      } as never);
      const run: EnvironmentRegistry["Service"]["run"] = (_selectedEnvironmentId, effect) =>
        Effect.provideService(effect, EnvironmentSupervisor, supervisor);
      const environmentRegistry = EnvironmentRegistry.of({ run } as never);
      const atoms = createVcsEnvironmentAtoms(
        Atom.runtime(Layer.succeed(EnvironmentRegistry, environmentRegistry)),
      );
      const registry = AtomRegistry.make({
        scheduleTask: (task) => {
          task();
          return () => {};
        },
      });

      for (let index = 0; index < warmupSamples + measuredSamples; index += 1) {
        refreshStarted = yield* Deferred.make<void>();
        refreshResult = yield* Deferred.make<VcsStatusResult>();
        collecting = index >= warmupSamples;
        const cwd = `C:/vcs-queue-benchmark/${index}`;
        const refresh = atoms.refreshStatus.run(registry, { environmentId, input: { cwd } });
        yield* Deferred.await(refreshStarted);
        scheduledAt = performance.now();
        const mutation = atoms.stageFiles.run(registry, {
          environmentId,
          input: { cwd, filePaths: ["tracked.ts"] },
        });
        expect(yield* Effect.promise(() => mutation)).toMatchObject({ _tag: "Success" });
        yield* Deferred.succeed(refreshResult, status(`refresh-${index}`));
        expect(yield* Effect.promise(() => refresh)).toMatchObject({ _tag: "Success" });
      }
      registry.dispose();

      expect(delays).toHaveLength(measuredSamples);
      const sorted = delays.toSorted((left, right) => left - right);
      const summary = {
        warmupSamples,
        measuredSamples,
        clock: "performance.now",
        start: "immediately before actual stageFiles Atom command run",
        end: "actual vcs.stageFiles RPC effect execution start",
        activeRead: "actual refreshStatus Atom command deferred in its RPC effect",
        minMs: sorted[0],
        p50Ms: percentile(sorted, 0.5),
        p95Ms: percentile(sorted, 0.95),
        p99Ms: percentile(sorted, 0.99),
        maxMs: sorted.at(-1),
        meanMs: delays.reduce((total, delay) => total + delay, 0) / delays.length,
      };
      // @effect-diagnostics-next-line preferSchemaOverJson:off - Machine-local benchmark marker has a fixed internal shape and no wire boundary.
      const encoded = JSON.stringify(summary);
      yield* Effect.sync(() => {
        process.stdout.write(`VCS_QUEUE_BENCHMARK ${encoded}\n`);
      });
      expect(Number.isFinite(summary.p95Ms)).toBe(true);
    }),
  );
});
