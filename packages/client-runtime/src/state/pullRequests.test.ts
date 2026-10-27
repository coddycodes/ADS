import {
  createOneShotBypass,
  createPullRequestsEnvironmentAtoms,
  createStoredContextFirst,
} from "@coddy/client-runtime/state/pull-requests";
import {
  EnvironmentId,
  PullRequestsOperationError,
  type PullRequestsContext,
} from "@coddy/contracts";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import { Atom } from "effect/reactivity";
import { RpcClientError } from "effect/rpc";
import { describe, expect, it } from "@effect/vitest";

import { EnvironmentRegistry } from "../connection/registry.ts";

describe("Pull Requests environment atoms", () => {
  it("exports all twelve factories through the public subpath and keys reads by environment and input", () => {
    const runtime = Atom.runtime(
      Layer.effect(
        EnvironmentRegistry,
        Effect.die("No RPC should execute while constructing unmounted atoms."),
      ),
    );
    const atoms = createPullRequestsEnvironmentAtoms(runtime);
    expect(Object.keys(atoms).sort()).toEqual([
      "checkout",
      "get",
      "getChecks",
      "getCommits",
      "getContext",
      "getCreateDefaults",
      "getFiles",
      "getTimeline",
      "getVocabulary",
      "list",
      "readSnapshot",
      "requestContextRescan",
      "requestListTotalsRefresh",
      "runAction",
      "subscribe",
    ]);

    const environmentId = EnvironmentId.make("env-1");
    const context = atoms.getContext({ environmentId, input: { cwd: "/repo" } });
    expect(Atom.isAtom(context)).toBe(true);
    expect(atoms.getContext({ environmentId, input: { cwd: "/repo" } })).toBe(context);
    expect(atoms.getContext({ environmentId, input: { cwd: "/other" } })).not.toBe(context);
    expect(
      atoms.getContext({ environmentId: EnvironmentId.make("env-2"), input: { cwd: "/repo" } }),
    ).not.toBe(context);
    atoms.requestContextRescan({ environmentId, input: { cwd: "/repo" } });
    expect(atoms.getContext({ environmentId, input: { cwd: "/repo" } })).toBe(context);
  });

  it.effect(
    "sends a requested bypass until a read is answered, for that environment and input only",
    () =>
      Effect.gen(function* () {
        const bypass = createOneShotBypass("rescan");
        const environmentId = EnvironmentId.make("env-1");
        const input = { cwd: "/repo" };
        const sent: object[] = [];
        const record = (payload: object) => Effect.sync(() => void sent.push(payload));
        const read = (
          target: EnvironmentId,
          value: { readonly cwd: string },
          send: (
            payload: object,
          ) => Effect.Effect<
            void,
            RpcClientError.RpcClientError | PullRequestsOperationError
          > = record,
        ) => Effect.exit(bypass.run(target, value, send));

        yield* read(environmentId, input);
        bypass.request({ environmentId, input: { cwd: "/repo" } });
        yield* read(EnvironmentId.make("env-2"), input);
        yield* read(environmentId, { cwd: "/other" });
        // An interrupted read and a lost connection are not answers: the next read still bypasses.
        yield* read(environmentId, input, (payload) =>
          record(payload).pipe(Effect.andThen(Effect.interrupt)),
        );
        yield* read(environmentId, input, (payload) =>
          record(payload).pipe(
            Effect.andThen(
              Effect.fail(
                new RpcClientError.RpcClientError({
                  reason: new RpcClientError.RpcClientDefect({
                    message: "socket closed",
                    cause: new Error("socket closed"),
                  }),
                }),
              ),
            ),
          ),
        );
        // The server's typed failure is an answer, like a success.
        yield* read(environmentId, input, (payload) =>
          record(payload).pipe(
            Effect.andThen(
              Effect.fail(
                new PullRequestsOperationError({
                  operation: "pullRequests.getContext",
                  code: "timeout",
                  message: "The read timed out.",
                  hostDetail: null,
                  retryable: true,
                }),
              ),
            ),
          ),
        );
        yield* read(environmentId, input);
        expect(sent).toEqual([
          { cwd: "/repo" },
          { cwd: "/repo" },
          { cwd: "/other" },
          { cwd: "/repo", rescan: true },
          { cwd: "/repo", rescan: true },
          { cwd: "/repo", rescan: true },
          { cwd: "/repo" },
        ]);

        const totals = createOneShotBypass("refreshTotals");
        const page = { cwd: "/repo", state: "open", cursor: null as string | null };
        const pages: object[] = [];
        const list = (value: typeof page) =>
          totals.run(environmentId, value, (payload) =>
            Effect.sync(() => void pages.push(payload)),
          );
        totals.request({ environmentId, input: { ...page } });
        yield* list({ ...page, cursor: "2" });
        yield* list(page);
        yield* list(page);
        expect(pages).toEqual([{ ...page, cursor: "2" }, { ...page, refreshTotals: true }, page]);
      }),
  );

  it.effect("asks for the stored context first, then reads live after a stale answer", () =>
    Effect.gen(function* () {
      const stored = createStoredContextFirst();
      const environmentId = EnvironmentId.make("env-1");
      const input = { cwd: "/repo" };
      const sent: boolean[] = [];
      const answer = (stale: boolean) => (allowStale: boolean) =>
        Effect.sync(() => {
          sent.push(allowStale);
          return { status: "available", ...(stale ? { stale } : {}) } as PullRequestsContext;
        });

      yield* stored.run(environmentId, input, answer(true));
      yield* stored.run(environmentId, input, answer(false));
      // The next open asks for the stored copy again; other environments are separate.
      yield* stored.run(environmentId, input, answer(false));
      yield* stored.run(EnvironmentId.make("env-2"), input, answer(false));
      expect(sent).toEqual([true, false, true, true]);

      // A failed live read is not remembered: once the cached context is gone, the
      // next open paints the stored copy again instead of waiting on the host.
      yield* stored.run(environmentId, input, answer(true));
      yield* Effect.exit(
        stored.run(environmentId, input, (allowStale) =>
          Effect.sync(() => void sent.push(allowStale)).pipe(
            Effect.andThen(Effect.fail("offline")),
          ),
        ),
      );
      yield* stored.run(environmentId, input, answer(false));
      expect(sent.slice(4)).toEqual([true, false, true]);
    }),
  );
});
