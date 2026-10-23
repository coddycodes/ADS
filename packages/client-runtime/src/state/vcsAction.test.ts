import {
  EnvironmentId,
  type GitActionProgressEvent,
  type GitRunStackedActionResult,
  type GitRunStackedActionInput,
  type VcsStatusResult,
  WS_METHODS,
} from "@bibcode/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Cause from "effect/Cause";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";

import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import type { RpcSession } from "../rpc/session.ts";
import type { AtomCommandResult } from "./runtime.ts";
import { createVcsEnvironmentAtoms } from "./vcs.ts";
import {
  vcsCommandConcurrency,
  vcsCommandScheduler,
  vcsStatusRefreshConcurrency,
  vcsStatusRefreshScheduler,
} from "./vcsCommandScheduler.ts";
import {
  applyVcsActionProgressEvent,
  beginVcsActionState,
  consumeVcsActionProgress,
  createVcsActionManager,
  createVcsActionTransportId,
  EMPTY_VCS_ACTION_ATOM,
  EMPTY_VCS_ACTION_STATE,
  failVcsActionState,
  getVcsActionStateAtom,
  getVcsActionTargetKey,
  normalizeVcsActionProgressEvent,
  parseVcsActionTargetKey,
  VcsActionMissingTerminalEventError,
  VcsActionRemoteFailureError,
  VcsActionTargetKeyParseError,
  VcsActionUnavailableError,
} from "./vcsAction.ts";

const actionId = "action-123";
const action = "commit_push" as const;
const cwd = "/repo";
const environmentId = EnvironmentId.make("environment-1");
const isVcsActionUnavailableError = Schema.is(VcsActionUnavailableError);

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

function deferred<T>(): {
  readonly promise: Promise<T>;
  readonly resolve: (value: T) => void;
} {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}
const result: GitRunStackedActionResult = {
  action,
  branch: {
    status: "skipped_not_requested",
  },
  commit: {
    status: "created",
    commitSha: "abc123",
    subject: "Test commit",
  },
  push: {
    status: "pushed",
    branch: "feature",
  },
  pr: {
    status: "skipped_not_requested",
  },
  toast: {
    title: "Changes pushed",
    cta: {
      kind: "none",
    },
  },
};

function progress<T extends GitActionProgressEvent>(event: T): T {
  return event;
}

function stackedActionSession(
  branchSelection: boolean | undefined,
  sent: GitRunStackedActionInput[],
): RpcSession {
  return {
    initialConfig: Effect.succeed({
      environment: {
        capabilities:
          branchSelection === undefined ? {} : { gitPullRequestBranchSelection: branchSelection },
      },
    } as never),
    client: {
      [WS_METHODS.gitRunStackedAction]: (input: GitRunStackedActionInput) => {
        sent.push(input);
        return Stream.make({
          kind: "action_finished",
          actionId: input.actionId,
          cwd: input.cwd,
          action: input.action,
          result: { ...result, action: input.action },
        });
      },
    } as never,
    ready: Effect.void,
    probe: Effect.void,
    closed: Effect.never,
    e2eeAuthenticated: Effect.succeed(null),
  };
}

const makeStackedActionHarness = Effect.fn("makeStackedActionHarness")(function* (
  initialSession: RpcSession,
) {
  const session = yield* SubscriptionRef.make(Option.some(initialSession));
  const supervisor = EnvironmentSupervisor.of({ target: { environmentId }, session } as never);
  const runStream: EnvironmentRegistry["Service"]["runStream"] = (selected, stream) => {
    expect(selected).toBe(environmentId);
    return Stream.provideService(stream, EnvironmentSupervisor, supervisor);
  };
  const manager = createVcsActionManager(
    Atom.runtime(
      Layer.succeed(EnvironmentRegistry, EnvironmentRegistry.of({ runStream } as never)),
    ),
  );
  const registry = AtomRegistry.make();
  return { session, manager, registry, command: manager.runStackedAction({ environmentId, cwd }) };
});

describe("vcsActionState", () => {
  for (const capability of [undefined, false]) {
    for (const requestAction of ["create_pr", "commit_push_pr"] as const) {
      it.effect(
        `rejects ${requestAction} before mutation when branch selection is ${String(capability)}`,
        () =>
          Effect.gen(function* () {
            const sent: GitRunStackedActionInput[] = [];
            const h = yield* makeStackedActionHarness(stackedActionSession(capability, sent));
            const outcome = yield* Effect.promise(() =>
              h.command.run(h.registry, {
                actionId,
                action: requestAction,
                pullRequestBaseBranch: "release/next",
                pullRequestHeadBranch: "feature/other",
              }),
            );
            expect(outcome._tag).toBe("Failure");
            if (AsyncResult.isFailure(outcome)) {
              const error = Cause.squash(outcome.cause);
              expect(error).toMatchObject({
                _tag: "VcsPullRequestBranchSelectionUnsupportedError",
                environmentId,
              });
              expect(error).toBeInstanceOf(Error);
              expect((error as Error).message).toMatch(/update.*environment.*server/i);
            }
            expect(sent).toEqual([]);
            expect(h.registry.get(h.manager.stateAtom({ environmentId, cwd })).isRunning).toBe(
              false,
            );
            h.registry.dispose();
          }),
      );
    }
  }

  const createOptions = {
    draft: true,
    assignees: ["7"],
    reviewers: [],
    labels: [],
    milestone: null,
    removeSourceBranch: null,
    squash: null,
  };
  const withCreateOptions = (session: RpcSession, supported: boolean): RpcSession => ({
    ...session,
    initialConfig: Effect.succeed({
      environment: {
        capabilities: { gitPullRequestBranchSelection: true, pullRequestCreateOptions: supported },
      },
    } as never),
  });

  it.effect("rejects create options before mutation when the server lacks them", () =>
    Effect.gen(function* () {
      const sent: GitRunStackedActionInput[] = [];
      const h = yield* makeStackedActionHarness(
        withCreateOptions(stackedActionSession(true, sent), false),
      );
      const outcome = yield* Effect.promise(() =>
        h.command.run(h.registry, {
          actionId,
          action: "create_pr",
          pullRequestBaseBranch: "release/next",
          pullRequestOptions: createOptions,
        }),
      );
      expect(outcome._tag).toBe("Failure");
      if (AsyncResult.isFailure(outcome)) {
        expect(Cause.squash(outcome.cause)).toMatchObject({
          _tag: "VcsPullRequestCreateOptionsUnsupportedError",
          environmentId,
        });
      }
      expect(sent).toEqual([]);
      h.registry.dispose();
    }),
  );

  it.effect("sends create options to servers that support them", () =>
    Effect.gen(function* () {
      const sent: GitRunStackedActionInput[] = [];
      const h = yield* makeStackedActionHarness(
        withCreateOptions(stackedActionSession(true, sent), true),
      );
      yield* Effect.promise(() =>
        h.command.run(h.registry, {
          actionId,
          action: "create_pr",
          pullRequestBaseBranch: "release/next",
          pullRequestOptions: createOptions,
        }),
      );
      expect(sent[0]?.pullRequestOptions).toEqual(createOptions);
      h.registry.dispose();
    }),
  );

  it.effect("leaves commit and push actions available without branch-selection support", () =>
    Effect.gen(function* () {
      const sent: GitRunStackedActionInput[] = [];
      const h = yield* makeStackedActionHarness({
        ...stackedActionSession(undefined, sent),
        initialConfig: Effect.die("Commit and push must not negotiate branch selection."),
      });
      for (const action of ["commit", "push", "commit_push"] as const) {
        const outcome = yield* Effect.promise(() =>
          h.command.run(h.registry, { actionId, action }),
        );
        expect(outcome._tag).toBe("Success");
      }
      expect(sent.map((input) => input.action)).toEqual(["commit", "push", "commit_push"]);
      h.registry.dispose();
    }),
  );

  it.effect("rechecks branch-selection support after an environment reconnects", () =>
    Effect.gen(function* () {
      const supported: GitRunStackedActionInput[] = [];
      const unsupported: GitRunStackedActionInput[] = [];
      const h = yield* makeStackedActionHarness(stackedActionSession(true, supported));
      const input = {
        actionId,
        action: "create_pr" as const,
        pullRequestBaseBranch: "release/next",
      };
      const first = yield* Effect.promise(() => h.command.run(h.registry, input));
      expect(first._tag).toBe("Success");
      yield* SubscriptionRef.set(h.session, Option.some(stackedActionSession(false, unsupported)));
      const next = yield* Effect.promise(() => h.command.run(h.registry, input));
      expect(next._tag).toBe("Failure");
      expect(supported).toHaveLength(1);
      expect(unsupported).toEqual([]);
      h.registry.dispose();
    }),
  );

  it.effect(
    "sends the action only to the session whose branch-selection capability was checked",
    () =>
      Effect.gen(function* () {
        const supported: GitRunStackedActionInput[] = [];
        const unsupported: GitRunStackedActionInput[] = [];
        const capableSession = stackedActionSession(true, supported);
        const h = yield* makeStackedActionHarness(capableSession);
        yield* SubscriptionRef.set(
          h.session,
          Option.some({
            ...capableSession,
            initialConfig: SubscriptionRef.set(
              h.session,
              Option.some(stackedActionSession(false, unsupported)),
            ).pipe(Effect.andThen(capableSession.initialConfig)),
          }),
        );
        const outcome = yield* Effect.promise(() =>
          h.command.run(h.registry, {
            actionId,
            action: "create_pr",
            pullRequestBaseBranch: "release/next",
          }),
        );
        expect(outcome._tag).toBe("Success");
        expect(supported).toHaveLength(1);
        expect(unsupported).toEqual([]);
        h.registry.dispose();
      }),
  );

  it.effect("sends the explicitly reviewed target to the selected environment's RPC", () =>
    Effect.gen(function* () {
      const sent: GitRunStackedActionInput[] = [];
      const { command, registry } = yield* makeStackedActionHarness(
        stackedActionSession(true, sent),
      );
      const outcome = yield* Effect.promise(() =>
        command.run(registry, {
          actionId,
          action: "create_pr",
          pullRequestTitle: "Reviewed",
          pullRequestBody: "Details",
          pullRequestBaseBranch: "release/next",
          pullRequestHeadBranch: "feature/other",
        }),
      );
      expect(AsyncResult.isSuccess(outcome)).toBe(true);
      expect(sent).toEqual([
        {
          actionId: createVcsActionTransportId({ environmentId, cwd }, actionId),
          cwd,
          action: "create_pr",
          pullRequestTitle: "Reviewed",
          pullRequestBody: "Details",
          pullRequestBaseBranch: "release/next",
          pullRequestHeadBranch: "feature/other",
        },
      ]);
      registry.dispose();
    }),
  );

  it.effect("wires actual status refreshes independently from actual mutations", () =>
    Effect.gen(function* () {
      const firstRefreshStarted = yield* Deferred.make<void>();
      const firstRefreshResult = yield* Deferred.make<VcsStatusResult>();
      let refreshExecutions = 0;
      let mutationExecutions = 0;
      const session = yield* SubscriptionRef.make(
        Option.some({
          client: {
            [WS_METHODS.vcsRefreshStatus]: () =>
              Effect.suspend(() => {
                refreshExecutions += 1;
                if (refreshExecutions === 1) {
                  return Deferred.succeed(firstRefreshStarted, undefined).pipe(
                    Effect.andThen(Deferred.await(firstRefreshResult)),
                  );
                }
                return Effect.succeed(status(`refresh-${refreshExecutions}`));
              }),
            [WS_METHODS.vcsStageFiles]: () =>
              Effect.sync(() => {
                mutationExecutions += 1;
              }),
          },
        } as never),
      );
      const supervisor = EnvironmentSupervisor.of({
        target: { environmentId, label: "VCS scheduler test" },
        session,
      } as never);
      const selectedEnvironments: string[] = [];
      const run: EnvironmentRegistry["Service"]["run"] = (selectedEnvironmentId, effect) => {
        selectedEnvironments.push(selectedEnvironmentId);
        return Effect.provideService(effect, EnvironmentSupervisor, supervisor);
      };
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
      const target = { environmentId, input: { cwd } };

      const first = atoms.refreshStatus.run(registry, target);
      yield* Deferred.await(firstRefreshStarted);
      const second = atoms.refreshStatus.run(registry, target);
      const third = atoms.refreshStatus.run(registry, target);
      const mutation = atoms.stageFiles.run(registry, {
        environmentId,
        input: { cwd, filePaths: ["tracked.ts"] },
      });

      expect(mutationExecutions).toBe(1);
      expect(yield* Effect.promise(() => mutation)).toMatchObject({ _tag: "Success" });
      expect(refreshExecutions).toBe(1);
      expect(mutationExecutions).toBe(1);

      yield* Deferred.succeed(firstRefreshResult, status("refresh-1"));
      const [firstResult, secondResult, thirdResult] = yield* Effect.promise(() =>
        Promise.all([first, second, third]),
      );
      expect(firstResult).toMatchObject({ _tag: "Success", value: status("refresh-1") });
      expect(secondResult).toMatchObject({ _tag: "Success", value: status("refresh-2") });
      expect(thirdResult).toEqual(secondResult);
      expect(refreshExecutions).toBe(2);

      expect(yield* Effect.promise(() => atoms.refreshStatus.run(registry, target))).toMatchObject({
        _tag: "Success",
        value: status("refresh-3"),
      });
      expect(refreshExecutions).toBe(3);
      expect(selectedEnvironments).toEqual([
        environmentId,
        environmentId,
        environmentId,
        environmentId,
      ]);
      registry.dispose();
    }),
  );

  it("starts a mutation while a status refresh is active", async () => {
    const registry = AtomRegistry.make();
    const refreshResult = deferred<AtomCommandResult<void, never>>();
    const events: string[] = [];
    const target = { environmentId, input: { cwd } };

    const refresh = vcsStatusRefreshScheduler.schedule(
      registry,
      vcsStatusRefreshConcurrency,
      target,
      () => {
        events.push("refresh");
        return refreshResult.promise;
      },
    );
    const mutation = vcsCommandScheduler.schedule(
      registry,
      vcsCommandConcurrency,
      target,
      async () => {
        events.push("mutation");
        return AsyncResult.success(undefined);
      },
    );

    expect(events).toEqual(["refresh", "mutation"]);
    await mutation;
    refreshResult.resolve(AsyncResult.success(undefined));
    await refresh;
    registry.dispose();
  });

  it("keeps one newest trailing status refresh and shares its result", async () => {
    const registry = AtomRegistry.make();
    const activeResult = deferred<AtomCommandResult<string, never>>();
    const executions: string[] = [];
    const target = { environmentId, input: { cwd } };

    const active = vcsStatusRefreshScheduler.schedule(
      registry,
      vcsStatusRefreshConcurrency,
      target,
      () => {
        executions.push("focus");
        return activeResult.promise;
      },
    );
    const visible = vcsStatusRefreshScheduler.schedule(
      registry,
      vcsStatusRefreshConcurrency,
      target,
      async () => {
        executions.push("visible");
        return AsyncResult.success("visible");
      },
    );
    const menu = vcsStatusRefreshScheduler.schedule(
      registry,
      vcsStatusRefreshConcurrency,
      target,
      async () => {
        executions.push("menu");
        return AsyncResult.success("menu");
      },
    );

    expect(executions).toEqual(["focus"]);
    activeResult.resolve(AsyncResult.success("focus"));
    expect(await active).toMatchObject({ _tag: "Success", value: "focus" });
    expect(await visible).toMatchObject({ _tag: "Success", value: "menu" });
    expect(await menu).toMatchObject({ _tag: "Success", value: "menu" });
    expect(executions).toEqual(["focus", "menu"]);
    registry.dispose();
  });

  it("runs an explicit post-mutation refresh after a pre-mutation read", async () => {
    const registry = AtomRegistry.make();
    const activeResult = deferred<AtomCommandResult<void, never>>();
    const events: string[] = [];
    const target = { environmentId, input: { cwd } };

    const active = vcsStatusRefreshScheduler.schedule(
      registry,
      vcsStatusRefreshConcurrency,
      target,
      () => {
        events.push("read");
        return activeResult.promise;
      },
    );
    await vcsCommandScheduler.schedule(registry, vcsCommandConcurrency, target, async () => {
      events.push("mutation");
      return AsyncResult.success(undefined);
    });
    const explicit = vcsStatusRefreshScheduler.schedule(
      registry,
      vcsStatusRefreshConcurrency,
      target,
      async () => {
        events.push("explicit");
        return AsyncResult.success(undefined);
      },
    );

    expect(events).toEqual(["read", "mutation"]);
    activeResult.resolve(AsyncResult.success(undefined));
    await Promise.all([active, explicit]);
    expect(events).toEqual(["read", "mutation", "explicit"]);
    registry.dispose();
  });

  it("preserves malformed target key diagnostics and the native cause without copying the key", () => {
    const key = "not-json-with-credential=do-not-log";
    let error: unknown;

    try {
      parseVcsActionTargetKey(key);
    } catch (cause) {
      error = cause;
    }

    expect(error).toBeInstanceOf(VcsActionTargetKeyParseError);
    expect(error).toMatchObject({ keyLength: key.length, cause: expect.any(SyntaxError) });
    expect(error).not.toHaveProperty("key");
    expect((error as Error).message).not.toContain(key);
  });

  it("rejects invalid target key shapes", () => {
    const key = JSON.stringify([environmentId]);

    expect(() => parseVcsActionTargetKey(key)).toThrowError(VcsActionTargetKeyParseError);
  });

  it("projects phase and hook progress without owning the async operation", () => {
    const initial = beginVcsActionState({
      operation: "run_change_request",
      label: "Running source control action",
      actionId,
    });
    const phase = applyVcsActionProgressEvent(
      initial,
      progress({
        actionId,
        action,
        cwd,
        kind: "phase_started",
        phase: "commit",
        label: "Committing...",
      }),
    );
    const hook = applyVcsActionProgressEvent(
      phase,
      progress({
        actionId,
        action,
        cwd,
        kind: "hook_started",
        hookName: "post-commit",
      }),
    );
    const output = applyVcsActionProgressEvent(
      hook,
      progress({
        actionId,
        action,
        cwd,
        kind: "hook_output",
        hookName: "post-commit",
        stream: "stdout",
        text: "hook output",
      }),
    );
    const finished = applyVcsActionProgressEvent(
      output,
      progress({
        actionId,
        action,
        cwd,
        kind: "hook_finished",
        hookName: "post-commit",
        exitCode: 0,
        durationMs: 12,
      }),
    );

    expect(phase).toMatchObject({
      isRunning: true,
      currentLabel: "Committing...",
      currentPhaseLabel: "Committing...",
    });
    expect(output).toMatchObject({
      currentLabel: "Running post-commit...",
      hookName: "post-commit",
      lastOutputLine: "hook output",
    });
    expect(finished).toMatchObject({
      currentLabel: "Committing...",
      hookName: null,
      lastOutputLine: null,
    });
  });

  it("projects action start and successful terminal progress", () => {
    const initial = beginVcsActionState({
      operation: "pull",
      label: "Pulling",
      actionId,
    });
    const started = applyVcsActionProgressEvent(initial, {
      actionId,
      action,
      cwd,
      kind: "action_started",
      phases: ["commit", "push"],
    });
    const finished = applyVcsActionProgressEvent(started, {
      actionId,
      action,
      cwd,
      kind: "action_finished",
      result,
    });

    expect(started).toMatchObject({
      isRunning: true,
      operation: "run_change_request",
      action,
    });
    expect(finished).toMatchObject({
      isRunning: false,
      operation: "run_change_request",
      error: null,
    });
  });

  it("creates local action ids and generic failure messages", () => {
    const first = beginVcsActionState({ operation: "pull", label: "Pulling" });
    const second = beginVcsActionState({ operation: "pull", label: "Pulling" });
    expect(first.actionId).toMatch(/^local-vcs-action:/);
    expect(second.actionId).not.toBe(first.actionId);
    expect(failVcsActionState("pull", first.actionId, "unknown").error).toBe(
      "Source control action failed.",
    );
    expect(failVcsActionState("pull", first.actionId, new Error("pull failed")).error).toBe(
      "pull failed",
    );
  });

  it("retains a terminal action error for presentation", () => {
    const initial = beginVcsActionState({
      operation: "run_change_request",
      label: "Running source control action",
      actionId,
    });
    const failed = applyVcsActionProgressEvent(
      initial,
      progress({
        actionId,
        action,
        cwd,
        kind: "action_failed",
        phase: null,
        message: "Push failed.",
      }),
    );

    expect(failed).toMatchObject({
      isRunning: false,
      operation: "run_change_request",
      actionId,
      action,
      error: "Push failed.",
    });
  });

  it("ignores progress after a newer action owns the target", () => {
    const current = beginVcsActionState({
      operation: "pull",
      label: "Pulling latest changes",
      actionId: "newer-action",
    });

    expect(
      applyVcsActionProgressEvent(
        current,
        progress({
          actionId,
          action,
          cwd,
          kind: "phase_started",
          phase: "push",
          label: "Pushing...",
        }),
      ),
    ).toBe(current);
  });

  it("keys presentation state only when the environment and repository are known", () => {
    expect(
      getVcsActionTargetKey({
        environmentId,
        cwd,
      }),
    ).toBe(JSON.stringify([environmentId, cwd]));
    expect(getVcsActionTargetKey({ environmentId: null, cwd })).toBeNull();
    expect(
      getVcsActionTargetKey({
        environmentId,
        cwd: null,
      }),
    ).toBeNull();
    expect(getVcsActionStateAtom({ environmentId: null, cwd })).toBe(EMPTY_VCS_ACTION_ATOM);
  });

  it("normalizes progress only for the matching environment-scoped action", () => {
    const target = { environmentId, cwd };
    const otherTarget = {
      environmentId: EnvironmentId.make("environment-2"),
      cwd,
    };
    const transportActionId = createVcsActionTransportId(target, actionId);
    const event = progress({
      actionId: createVcsActionTransportId(otherTarget, actionId),
      action,
      cwd,
      kind: "phase_started",
      phase: "push",
      label: "Pushing...",
    });

    expect(normalizeVcsActionProgressEvent(target, transportActionId, actionId, event)).toBeNull();
    expect(
      normalizeVcsActionProgressEvent(target, transportActionId, actionId, {
        ...event,
        actionId: transportActionId,
      }),
    ).toEqual({
      ...event,
      actionId,
    });
  });

  it.effect("consumes progress through the terminal event and returns its result", () =>
    Effect.gen(function* () {
      const target = { environmentId, cwd };
      const transportActionId = createVcsActionTransportId(target, actionId);
      const observed: GitActionProgressEvent[] = [];
      const events: GitActionProgressEvent[] = [
        {
          actionId: "unrelated-action",
          action,
          cwd,
          kind: "phase_started",
          phase: "commit",
          label: "Ignored",
        },
        {
          actionId: transportActionId,
          action,
          cwd,
          kind: "phase_started",
          phase: "push",
          label: "Pushing...",
        },
        {
          actionId: transportActionId,
          action,
          cwd,
          kind: "action_finished",
          result,
        },
      ];

      const actual = yield* consumeVcsActionProgress(Stream.fromIterable(events), {
        target,
        transportActionId,
        actionId,
        action,
        onProgress: (event) =>
          Effect.sync(() => {
            observed.push(event);
          }),
      });

      expect(actual).toEqual(result);
      expect(observed.map((event) => event.actionId)).toEqual([actionId, actionId]);
      expect(observed.map((event) => event.kind)).toEqual(["phase_started", "action_finished"]);
    }),
  );

  it.effect("retains structural remote failure context without copying the remote payload", () =>
    Effect.gen(function* () {
      const target = { environmentId, cwd };
      const transportActionId = createVcsActionTransportId(target, actionId);
      const remoteMessage = "The remote rejected the push with credential=do-not-log.";
      const error = yield* consumeVcsActionProgress(
        Stream.fromIterable<GitActionProgressEvent>([
          {
            actionId: transportActionId,
            action,
            cwd,
            kind: "action_failed",
            phase: "push",
            message: remoteMessage,
          },
        ]),
        {
          target,
          transportActionId,
          actionId,
          action,
          onProgress: () => Effect.void,
        },
      ).pipe(Effect.flip);

      expect(error).toBeInstanceOf(VcsActionRemoteFailureError);
      expect(error).toMatchObject({
        actionId,
        transportActionId,
        action,
        environmentId,
        cwd,
        phase: "push",
        remoteMessageLength: remoteMessage.length,
      });
      expect(error).not.toHaveProperty("detail");
      expect(error.message).toBe("Source control action 'commit_push' failed during push.");
      expect(error.message).not.toContain(remoteMessage);
    }),
  );

  it.effect("reports a missing terminal event as a protocol failure", () =>
    Effect.gen(function* () {
      const target = { environmentId, cwd };
      const transportActionId = createVcsActionTransportId(target, actionId);
      const error = yield* consumeVcsActionProgress(
        Stream.fromIterable<GitActionProgressEvent>([
          {
            actionId: transportActionId,
            action,
            cwd,
            kind: "phase_started",
            phase: "commit",
            label: "Committing...",
          },
        ]),
        {
          target,
          transportActionId,
          actionId,
          action,
          onProgress: () => Effect.void,
        },
      ).pipe(Effect.flip);

      expect(error).toBeInstanceOf(VcsActionMissingTerminalEventError);
      expect(error).toMatchObject({
        actionId,
        transportActionId,
        action,
        environmentId,
        cwd,
      });
      expect(error.message).toBe(
        "Source control action 'commit_push' ended without a terminal result.",
      );
    }),
  );

  it("keys mutation ownership by environment and cwd", () => {
    const runtime = Atom.runtime(Layer.empty) as unknown as Atom.AtomRuntime<
      EnvironmentRegistry,
      never
    >;
    const manager = createVcsActionManager(runtime);
    const registry = AtomRegistry.make();
    const target = { environmentId, cwd };
    const otherTarget = {
      environmentId: EnvironmentId.make("environment-2"),
      cwd,
    };

    expect(manager.runStackedAction(target)).toBe(manager.runStackedAction({ ...target }));
    expect(manager.runStackedAction(target)).not.toBe(manager.runStackedAction(otherTarget));
    expect(registry.get(manager.stateAtom(target))).toEqual(EMPTY_VCS_ACTION_STATE);

    registry.dispose();
  });

  it("retains the incomplete target and operation when tracking is unavailable", async () => {
    const runtime = Atom.runtime(Layer.empty) as unknown as Atom.AtomRuntime<
      EnvironmentRegistry,
      never
    >;
    const manager = createVcsActionManager(runtime);
    const registry = AtomRegistry.make();
    const result = await manager.track(
      registry,
      { environmentId, cwd: null },
      { operation: "pull", label: "Pulling latest changes" },
      async () => AsyncResult.success(undefined),
    );

    expect(AsyncResult.isFailure(result)).toBe(true);
    if (AsyncResult.isFailure(result)) {
      const error = Cause.squash(result.cause);
      expect(error).toBeInstanceOf(VcsActionUnavailableError);
      if (!isVcsActionUnavailableError(error)) {
        throw error;
      }
      expect(error).toMatchObject({
        operation: "pull",
        environmentId,
        cwd: null,
      });
      expect(error.message).toBe("Source control operation 'pull' is unavailable.");
    }

    registry.dispose();
  });

  it("fails an unavailable stacked action command without entering running state", async () => {
    const runtime = Atom.runtime(Layer.empty) as unknown as Atom.AtomRuntime<
      EnvironmentRegistry,
      never
    >;
    const manager = createVcsActionManager(runtime);
    const registry = AtomRegistry.make();
    const command = manager.runStackedAction({ environmentId: null, cwd });

    const commandResult = await command.run(registry, { actionId, action });

    expect(AsyncResult.isFailure(commandResult)).toBe(true);
    if (AsyncResult.isFailure(commandResult)) {
      expect(Cause.squash(commandResult.cause)).toBeInstanceOf(VcsActionUnavailableError);
    }
    registry.dispose();
  });

  it("builds the complete stacked-action input before surfacing a missing runtime service", async () => {
    const runtime = Atom.runtime(Layer.empty) as unknown as Atom.AtomRuntime<
      EnvironmentRegistry,
      never
    >;
    const manager = createVcsActionManager(runtime);
    const registry = AtomRegistry.make();
    const command = manager.runStackedAction({ environmentId, cwd });

    const commandResult = await command.run(registry, {
      actionId,
      action,
      commitMessage: "test commit",
      featureBranch: true,
      filePaths: ["src/a.ts"],
      commitStagedIndexAsIs: true,
    });

    expect(AsyncResult.isFailure(commandResult)).toBe(true);
    expect(registry.get(manager.stateAtom({ environmentId, cwd }))).toMatchObject({
      actionId,
      isRunning: true,
      operation: "run_change_request",
    });
    registry.dispose();
  });

  it("clears successful and interrupted tracked actions and resets matching errors", async () => {
    const runtime = Atom.runtime(Layer.empty) as unknown as Atom.AtomRuntime<
      EnvironmentRegistry,
      never
    >;
    const manager = createVcsActionManager(runtime);
    const registry = AtomRegistry.make();
    const target = { environmentId, cwd };

    await manager.track(registry, target, { operation: "pull", label: "Pulling" }, async () =>
      AsyncResult.success(undefined),
    );
    expect(registry.get(manager.stateAtom(target))).toBe(EMPTY_VCS_ACTION_STATE);

    await manager.track(registry, target, { operation: "pull", label: "Pulling" }, async () =>
      AsyncResult.failure(Cause.interrupt(1)),
    );
    expect(registry.get(manager.stateAtom(target))).toBe(EMPTY_VCS_ACTION_STATE);

    await manager.track(registry, target, { operation: "pull", label: "Pulling" }, async () =>
      AsyncResult.failure(Cause.fail(new Error("pull failed"))),
    );
    expect(registry.get(manager.stateAtom(target)).error).toBe("pull failed");
    manager.resetError(registry, target, "switch_ref");
    expect(registry.get(manager.stateAtom(target)).error).toBe("pull failed");
    manager.resetError(registry, target, "pull");
    expect(registry.get(manager.stateAtom(target))).toBe(EMPTY_VCS_ACTION_STATE);

    manager.resetError(registry, { environmentId: null, cwd }, "pull");
    registry.dispose();
  });

  it("tracks finite mutations without letting an older completion clear newer state", async () => {
    const runtime = Atom.runtime(Layer.empty) as unknown as Atom.AtomRuntime<
      EnvironmentRegistry,
      never
    >;
    const manager = createVcsActionManager(runtime);
    const registry = AtomRegistry.make();
    const target = { environmentId, cwd };
    let finishFirst!: () => void;
    let failSecond!: (error: Error) => void;
    const firstAction = new Promise<AtomCommandResult<void, never>>((resolve) => {
      finishFirst = () => resolve(AsyncResult.success(undefined));
    });
    const secondAction = new Promise<AtomCommandResult<void, Error>>((resolve) => {
      failSecond = (error) => resolve(AsyncResult.failure(Cause.fail(error)));
    });

    const first = manager.track(
      registry,
      target,
      { operation: "pull", label: "Pulling latest changes" },
      () => firstAction,
    );
    const firstActionId = registry.get(manager.stateAtom(target)).actionId;
    const second = manager.track(
      registry,
      target,
      { operation: "switch_ref", label: "Switching branch" },
      () => secondAction,
    );
    const secondActionId = registry.get(manager.stateAtom(target)).actionId;

    finishFirst();
    await first;
    expect(registry.get(manager.stateAtom(target))).toMatchObject({
      actionId: secondActionId,
      isRunning: true,
      operation: "switch_ref",
    });
    expect(secondActionId).not.toBe(firstActionId);

    failSecond(new Error("switch failed"));
    const secondFailure = await second;
    expect(AsyncResult.isFailure(secondFailure)).toBe(true);
    expect(registry.get(manager.stateAtom(target))).toMatchObject({
      actionId: secondActionId,
      error: "switch failed",
      isRunning: false,
      operation: "switch_ref",
    });

    registry.dispose();
  });
});
