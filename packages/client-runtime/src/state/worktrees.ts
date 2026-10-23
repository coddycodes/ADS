import {
  EnvironmentId,
  ORCHESTRATION_WS_METHODS,
  type ExecutionEnvironmentDescriptor,
  type ProjectWorktreeDiscoveryPolicy,
  type ThreadId,
  type VcsAdoptedWorktreeStatus,
  type VcsWorktreeCatalogSnapshot,
  type VcsWorktreeDescriptor,
  type WorktreeAdoptInput,
  type WorktreeAdoptResult,
  type WorktreeCreateManagedInput,
  type WorktreeCreatePanelInput,
  type WorktreeKey,
  type WorktreeRetargetInput,
  type WorktreeRemovalPlan,
  type WorktreeRemovalResult,
  WS_METHODS,
} from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as Semaphore from "effect/Semaphore";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { Atom } from "effect/reactivity";

import type { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import {
  EnvironmentRpcUnavailableError,
  requestInSession,
  subscribeInSession,
  type EnvironmentRpcInput,
  type EnvironmentUnaryRpcTag,
} from "../rpc/client.ts";
import {
  createAtomCommandScheduler,
  createEnvironmentSubscriptionAtomFamily,
  createRuntimeCommand,
  runInEnvironment,
} from "./runtime.ts";

export interface WorktreeDiscoveryStateInput {
  readonly snapshot: VcsWorktreeCatalogSnapshot;
  readonly policy: ProjectWorktreeDiscoveryPolicy;
}

export interface WorktreeDiscoveryState {
  readonly newCandidates: ReadonlyArray<VcsWorktreeDescriptor>;
  readonly acknowledgedCandidates: ReadonlyArray<VcsWorktreeDescriptor>;
  readonly showInitialPrompt: boolean;
  readonly showCollapsedHiddenLine: boolean;
  readonly shownCandidates: ReadonlyArray<VcsWorktreeDescriptor>;
}

export interface WorktreeAddAllInput {
  readonly candidates: ReadonlyArray<WorktreeAdoptInput>;
}

export type WorktreeAddAllItemResult =
  | {
      readonly _tag: "Success";
      readonly worktreeKey: WorktreeKey;
      readonly result: WorktreeAdoptResult;
    }
  | {
      readonly _tag: "Failure";
      readonly worktreeKey: WorktreeKey;
      readonly error: unknown;
    };

export interface WorktreeAddAllResult {
  readonly results: ReadonlyArray<WorktreeAddAllItemResult>;
}

export type WorktreeRemoveCommandResult =
  | {
      readonly _tag: "Removed";
      readonly result: WorktreeRemovalResult;
    }
  | {
      readonly _tag: "PlanChanged";
      readonly plan: WorktreeRemovalPlan;
    };

export function deriveWorktreeDiscoveryState(
  input: WorktreeDiscoveryStateInput,
): WorktreeDiscoveryState {
  const candidates = input.snapshot.worktrees.filter((worktree) => worktree.eligibleForAdoption);
  const baselinePaths = new Set(input.policy.baselinePaths);
  const newCandidates = candidates.filter((worktree) => !baselinePaths.has(worktree.path));
  const acknowledgedCandidates = candidates.filter((worktree) => baselinePaths.has(worktree.path));
  const showInitialPrompt =
    input.policy.visibility === "hidden" &&
    (input.policy.initialPromptDismissedAt === null
      ? candidates.length > 0
      : newCandidates.length > 0);

  return {
    newCandidates,
    acknowledgedCandidates,
    showInitialPrompt,
    showCollapsedHiddenLine:
      input.policy.visibility === "hidden" && candidates.length > 0 && !showInitialPrompt,
    shownCandidates: input.policy.visibility === "shown" ? candidates : [],
  };
}

export function deriveAdoptedWorkspaceStateByThreadId(
  snapshot: VcsWorktreeCatalogSnapshot,
): ReadonlyMap<ThreadId, VcsAdoptedWorktreeStatus> {
  return new Map(snapshot.adoptedWorkspaces.map((workspace) => [workspace.threadId, workspace]));
}

export type WorktreeCatalogCapabilityPolicy =
  | {
      readonly catalogRpc: "enabled";
      readonly refreshReason: "enabled" | "disabled";
      readonly removal: "catalog";
    }
  | {
      readonly catalogRpc: "disabled";
      readonly refreshReason: "disabled";
      readonly removal: "legacy-detach-only";
    };

export function selectWorktreeCatalogCapabilityPolicy(
  environmentDescriptor: ExecutionEnvironmentDescriptor | null | undefined,
): WorktreeCatalogCapabilityPolicy {
  return environmentDescriptor?.capabilities?.worktreeCatalog === true
    ? {
        catalogRpc: "enabled",
        refreshReason:
          environmentDescriptor.capabilities.worktreeCatalogRefreshReason === true
            ? "enabled"
            : "disabled",
        removal: "catalog",
      }
    : {
        catalogRpc: "disabled",
        refreshReason: "disabled",
        removal: "legacy-detach-only",
      };
}

export function isWorktreeCatalogSupported(
  environmentDescriptor: ExecutionEnvironmentDescriptor | null | undefined,
): boolean {
  return selectWorktreeCatalogCapabilityPolicy(environmentDescriptor).catalogRpc === "enabled";
}

export function selectWorktreeWorkspaceActionsAvailable(
  workspace: Pick<VcsAdoptedWorktreeStatus, "availability"> | null | undefined,
): boolean {
  return (
    workspace === null ||
    workspace === undefined ||
    workspace.availability === "present" ||
    workspace.availability === "verification-unavailable"
  );
}

export class WorktreeCatalogUnsupportedError extends Schema.TaggedError<WorktreeCatalogUnsupportedError>()(
  "WorktreeCatalogUnsupportedError",
  {
    environmentId: EnvironmentId,
  },
) {
  override get message(): string {
    return `Environment ${this.environmentId} does not support the worktree catalog.`;
  }
}

const negotiatedWorktreeCatalogPolicy = Effect.fn("Worktrees.negotiatedCapabilityPolicy")(
  function* () {
    const supervisor = yield* EnvironmentSupervisor;
    const session = yield* SubscriptionRef.get(supervisor.session).pipe(
      Effect.flatMap(
        Option.match({
          onNone: () =>
            Effect.fail(
              new EnvironmentRpcUnavailableError({
                environmentId: supervisor.target.environmentId,
                message: `${supervisor.target.label} is not connected.`,
              }),
            ),
          onSome: Effect.succeed,
        }),
      ),
    );
    const serverConfig = yield* session.initialConfig;
    return {
      environmentId: supervisor.target.environmentId,
      session,
      policy: selectWorktreeCatalogCapabilityPolicy(serverConfig.environment),
    } as const;
  },
);

const requireWorktreeCatalog = Effect.fn("Worktrees.requireCatalog")(function* () {
  const negotiated = yield* negotiatedWorktreeCatalogPolicy();
  if (negotiated.policy.catalogRpc === "disabled") {
    return yield* new WorktreeCatalogUnsupportedError({
      environmentId: negotiated.environmentId,
    });
  }
  return negotiated;
});

function requestWithWorktreeCatalogCapability<TTag extends EnvironmentUnaryRpcTag>(
  tag: TTag,
  input: EnvironmentRpcInput<TTag>,
) {
  return Effect.gen(function* () {
    const negotiated = yield* requireWorktreeCatalog();
    return yield* requestInSession(negotiated.session, negotiated.environmentId, tag, input);
  });
}

function requestWorktreeCatalogRefresh(
  input: EnvironmentRpcInput<typeof WS_METHODS.vcsRefreshWorktreeCatalog>,
) {
  return Effect.gen(function* () {
    const negotiated = yield* requireWorktreeCatalog();
    const wireInput =
      negotiated.policy.refreshReason === "enabled" ? input : { projectId: input.projectId };
    return yield* requestInSession(
      negotiated.session,
      negotiated.environmentId,
      WS_METHODS.vcsRefreshWorktreeCatalog,
      wireInput,
    );
  });
}

function subscribeToWorktreeCatalog(
  input: EnvironmentRpcInput<typeof WS_METHODS.subscribeWorktreeCatalog>,
) {
  return Stream.unwrap(
    EnvironmentSupervisor.pipe(
      Effect.map((supervisor) =>
        SubscriptionRef.changes(supervisor.session).pipe(
          Stream.switchMap(
            Option.match({
              onNone: () => Stream.empty,
              onSome: (session) =>
                Stream.fromEffect(session.initialConfig).pipe(
                  Stream.flatMap((serverConfig) =>
                    selectWorktreeCatalogCapabilityPolicy(serverConfig.environment).catalogRpc ===
                    "enabled"
                      ? subscribeInSession(
                          session,
                          supervisor.target.environmentId,
                          WS_METHODS.subscribeWorktreeCatalog,
                          input,
                        )
                      : Stream.empty,
                  ),
                ),
            }),
          ),
        ),
      ),
    ),
  );
}

function retainLastUsableWorktreeCatalogRows<E, R>(
  stream: Stream.Stream<VcsWorktreeCatalogSnapshot, E, R>,
): Stream.Stream<VcsWorktreeCatalogSnapshot, E, R> {
  return stream.pipe(
    Stream.mapAccum(
      () => null as VcsWorktreeCatalogSnapshot | null,
      (lastAuthoritative, current) => {
        const retained =
          !current.authoritative &&
          current.scanStatus._tag === "degraded" &&
          lastAuthoritative !== null
            ? {
                ...current,
                worktrees: lastAuthoritative.worktrees,
                adoptedWorkspaces: lastAuthoritative.adoptedWorkspaces,
              }
            : current;
        return [current.authoritative ? current : lastAuthoritative, [retained]] as const;
      },
    ),
  );
}

interface EffectMutexLane {
  readonly semaphore: Semaphore.Semaphore;
  users: number;
}

function createEffectMutexLanes() {
  const lanes = new Map<string, EffectMutexLane>();

  return <A, E, R>(key: string, effect: Effect.Effect<A, E, R>): Effect.Effect<A, E, R> =>
    Effect.acquireUseRelease(
      Effect.sync(() => {
        const existing = lanes.get(key);
        const lane = existing ?? {
          semaphore: Semaphore.makeUnsafe(1),
          users: 0,
        };
        lane.users += 1;
        if (existing === undefined) {
          lanes.set(key, lane);
        }
        return lane;
      }),
      (lane) => lane.semaphore.withPermit(effect),
      (lane) =>
        Effect.sync(() => {
          lane.users -= 1;
          if (lane.users === 0 && lanes.get(key) === lane) {
            lanes.delete(key);
          }
        }),
    );
}

export function createWorktreeEnvironmentAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, E>,
) {
  const refreshScheduler = createAtomCommandScheduler();
  const withProjectMutationLane = createEffectMutexLanes();
  const withEnvironmentBulkLane = createEffectMutexLanes();
  const projectKey = ({
    environmentId,
    input,
  }: {
    readonly environmentId: string;
    readonly input: { readonly projectId: string };
  }) => JSON.stringify([environmentId, input.projectId]);
  const refreshKey = ({
    environmentId,
    input,
  }: {
    readonly environmentId: string;
    readonly input: EnvironmentRpcInput<typeof WS_METHODS.vcsRefreshWorktreeCatalog>;
  }) =>
    JSON.stringify([
      environmentId,
      input.projectId,
      input.reason === "focus" ? "focus" : "explicit",
    ]);

  const executeAdoption = (target: {
    readonly environmentId: EnvironmentId;
    readonly input: WorktreeAdoptInput;
  }) =>
    withProjectMutationLane(
      projectKey(target),
      runInEnvironment(
        target.environmentId,
        requestWithWorktreeCatalogCapability(WS_METHODS.worktreeAdopt, target.input),
      ),
    );
  const addOne = createRuntimeCommand(runtime, {
    label: "environment-data:worktrees:add-one",
    execute: executeAdoption,
  });
  const adoptCandidate = Effect.fn("Worktrees.adoptCandidate")(function* (
    environmentId: EnvironmentId,
    candidate: WorktreeAdoptInput,
  ) {
    const result = yield* Effect.exit(
      executeAdoption({
        environmentId,
        input: candidate,
      }),
    );
    if (Exit.isSuccess(result)) {
      return {
        _tag: "Success",
        worktreeKey: candidate.worktreeKey,
        result: result.value,
      } satisfies WorktreeAddAllItemResult;
    }
    if (Cause.hasInterruptsOnly(result.cause)) {
      return yield* Effect.failCause(result.cause);
    }
    return {
      _tag: "Failure",
      worktreeKey: candidate.worktreeKey,
      error: Cause.squash(result.cause),
    } satisfies WorktreeAddAllItemResult;
  });
  const addAll = createRuntimeCommand(runtime, {
    label: "environment-data:worktrees:add-all",
    execute: (target: {
      readonly environmentId: EnvironmentId;
      readonly input: WorktreeAddAllInput;
    }) =>
      runInEnvironment(target.environmentId, requireWorktreeCatalog()).pipe(
        Effect.andThen(
          withEnvironmentBulkLane(
            target.environmentId,
            Effect.forEach(
              target.input.candidates,
              (candidate) => adoptCandidate(target.environmentId, candidate),
              { concurrency: 4 },
            ).pipe(Effect.map((results): WorktreeAddAllResult => ({ results }))),
          ),
        ),
      ),
  });
  const getRemovalPlanEffect = (target: {
    readonly environmentId: EnvironmentId;
    readonly input: EnvironmentRpcInput<typeof WS_METHODS.worktreeGetRemovalPlan>;
  }) =>
    runInEnvironment(
      target.environmentId,
      requestWithWorktreeCatalogCapability(WS_METHODS.worktreeGetRemovalPlan, target.input),
    );

  return {
    catalog: createEnvironmentSubscriptionAtomFamily(runtime, {
      label: "environment-data:worktrees:catalog",
      idleTtlMs: 0,
      subscribe: (input: EnvironmentRpcInput<typeof WS_METHODS.subscribeWorktreeCatalog>) =>
        retainLastUsableWorktreeCatalogRows(subscribeToWorktreeCatalog(input)),
    }),
    refresh: createRuntimeCommand(runtime, {
      label: "environment-data:worktrees:refresh",
      scheduler: refreshScheduler,
      concurrency: { mode: "singleFlight", key: refreshKey },
      execute: (target: {
        readonly environmentId: EnvironmentId;
        readonly input: EnvironmentRpcInput<typeof WS_METHODS.vcsRefreshWorktreeCatalog>;
      }) => runInEnvironment(target.environmentId, requestWorktreeCatalogRefresh(target.input)),
    }),
    updatePolicy: createRuntimeCommand(runtime, {
      label: "environment-data:worktrees:update-policy",
      execute: (target: {
        readonly environmentId: EnvironmentId;
        readonly input: EnvironmentRpcInput<typeof WS_METHODS.worktreeUpdateDiscoveryPolicy>;
      }) =>
        withProjectMutationLane(
          projectKey(target),
          runInEnvironment(
            target.environmentId,
            requestWithWorktreeCatalogCapability(
              WS_METHODS.worktreeUpdateDiscoveryPolicy,
              target.input,
            ),
          ),
        ),
    }),
    createManaged: createRuntimeCommand(runtime, {
      label: "environment-data:worktrees:create-managed",
      execute: (target: {
        readonly environmentId: EnvironmentId;
        readonly input: WorktreeCreateManagedInput;
      }) =>
        withProjectMutationLane(
          projectKey(target),
          runInEnvironment(
            target.environmentId,
            requestWithWorktreeCatalogCapability(WS_METHODS.worktreeCreateManaged, target.input),
          ),
        ),
    }),
    createPanel: createRuntimeCommand(runtime, {
      label: "environment-data:worktrees:create-panel",
      execute: (target: {
        readonly environmentId: EnvironmentId;
        readonly input: WorktreeCreatePanelInput;
      }) =>
        withProjectMutationLane(
          JSON.stringify([target.environmentId, target.input.hostThreadId]),
          runInEnvironment(
            target.environmentId,
            requestWithWorktreeCatalogCapability(WS_METHODS.worktreeCreatePanel, target.input),
          ),
        ),
    }),
    retarget: createRuntimeCommand(runtime, {
      label: "environment-data:worktrees:retarget",
      execute: (target: {
        readonly environmentId: EnvironmentId;
        readonly input: WorktreeRetargetInput;
      }) =>
        withProjectMutationLane(
          projectKey(target),
          runInEnvironment(
            target.environmentId,
            requestWithWorktreeCatalogCapability(WS_METHODS.worktreeRetarget, target.input),
          ),
        ),
    }),
    addOne,
    addAll,
    getRemovalPlan: createRuntimeCommand(runtime, {
      label: "environment-data:worktrees:get-removal-plan",
      execute: getRemovalPlanEffect,
    }),
    removeFromBibCode: createRuntimeCommand(runtime, {
      label: "environment-data:worktrees:remove-from-bibcode",
      execute: (target: {
        readonly environmentId: EnvironmentId;
        readonly input: EnvironmentRpcInput<typeof WS_METHODS.worktreeRemoveFromBibCode>;
      }) =>
        withProjectMutationLane(
          projectKey(target),
          runInEnvironment(
            target.environmentId,
            Effect.gen(function* () {
              const negotiated = yield* negotiatedWorktreeCatalogPolicy();
              if (negotiated.policy.removal === "catalog") {
                return yield* requestInSession(
                  negotiated.session,
                  negotiated.environmentId,
                  WS_METHODS.worktreeRemoveFromBibCode,
                  target.input,
                );
              }
              yield* requestInSession(
                negotiated.session,
                negotiated.environmentId,
                ORCHESTRATION_WS_METHODS.dispatchCommand,
                {
                  type: "thread.delete",
                  commandId: target.input.commandId,
                  threadId: target.input.threadId,
                },
              );
              return {
                threadRemoved: true,
                gitOutcome: "not-requested",
                orphanCleanupPending: false,
              } satisfies WorktreeRemovalResult;
            }),
          ),
        ),
    }),
    remove: createRuntimeCommand(runtime, {
      label: "environment-data:worktrees:remove",
      execute: (target: {
        readonly environmentId: EnvironmentId;
        readonly input: EnvironmentRpcInput<typeof WS_METHODS.worktreeRemove>;
      }) =>
        withProjectMutationLane(
          projectKey(target),
          runInEnvironment(
            target.environmentId,
            requestWithWorktreeCatalogCapability(WS_METHODS.worktreeRemove, target.input),
          ).pipe(
            Effect.map((result): WorktreeRemoveCommandResult => ({ _tag: "Removed", result })),
            Effect.catchTag("WorktreeRemovalError", (error) =>
              error.reason === "stale-plan"
                ? getRemovalPlanEffect({
                    environmentId: target.environmentId,
                    input: {
                      projectId: target.input.projectId,
                      threadId: target.input.threadId,
                    },
                  }).pipe(
                    Effect.map((plan): WorktreeRemoveCommandResult => ({
                      _tag: "PlanChanged",
                      plan,
                    })),
                  )
                : Effect.fail(error),
            ),
          ),
        ),
    }),
  };
}
