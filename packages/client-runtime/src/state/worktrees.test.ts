import {
  CommandId,
  EnvironmentId,
  ORCHESTRATION_WS_METHODS,
  ProjectId,
  ProviderInstanceId,
  ThreadId,
  WorktreeAdoptionError,
  WorktreeKey,
  WorktreeRemovalError,
  WorktreeRemovalPlanToken,
  WS_METHODS,
  type VcsAdoptedWorktreeStatus,
  type VcsWorktreeCatalogSnapshot,
  type VcsWorktreeDescriptor,
  type WorktreeAdoptInput,
} from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Cause from "effect/Cause";
import * as Effect from "effect/Effect";
import * as Deferred from "effect/Deferred";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";

import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import type { WsRpcProtocolClient } from "../rpc/protocol.ts";
import type { RpcSession } from "../rpc/session.ts";
import {
  createWorktreeEnvironmentAtoms,
  deriveAdoptedWorkspaceStateByThreadId,
  deriveWorktreeDiscoveryState,
  isWorktreeCatalogSupported,
  selectWorktreeCatalogCapabilityPolicy,
  selectWorktreeWorkspaceActionsAvailable,
  WorktreeCatalogUnsupportedError,
} from "./worktrees.ts";

const ENVIRONMENT_ONE = EnvironmentId.make("environment-1");
const ENVIRONMENT_TWO = EnvironmentId.make("environment-2");
const PROJECT_ID = ProjectId.make("project-1");

function descriptor(
  path: string,
  overrides: Partial<VcsWorktreeDescriptor> = {},
): VcsWorktreeDescriptor {
  return {
    worktreeKey: `key:${path}`,
    path,
    branch: "feature/worktree",
    head: "abc123",
    isPrimary: false,
    isBare: false,
    locked: false,
    registrationState: "registered",
    directoryState: "present",
    adoptionState: "none",
    eligibleForAdoption: true,
    ...overrides,
  } as VcsWorktreeDescriptor;
}

function adoptedWorkspace(
  threadId: string,
  path: string,
  overrides: Partial<VcsAdoptedWorktreeStatus> = {},
): VcsAdoptedWorktreeStatus {
  return {
    threadId,
    worktreeKey: `key:${path}`,
    path,
    branch: "feature/worktree",
    availability: "present",
    registrationState: "registered",
    locked: false,
    ...overrides,
  } as VcsAdoptedWorktreeStatus;
}

function snapshot(input: {
  readonly generation?: number;
  readonly authoritative?: boolean;
  readonly scanStatus?: VcsWorktreeCatalogSnapshot["scanStatus"];
  readonly worktrees?: ReadonlyArray<VcsWorktreeDescriptor>;
  readonly adoptedWorkspaces?: ReadonlyArray<VcsAdoptedWorktreeStatus>;
  readonly repositoryKey?: string;
}): VcsWorktreeCatalogSnapshot {
  return {
    repositoryKey: input.repositoryKey ?? "repository-1",
    generation: input.generation ?? 1,
    authoritative: input.authoritative ?? true,
    observedAt: "2026-08-09T12:00:00.000Z",
    scanStatus: input.scanStatus ?? { _tag: "ready" },
    worktrees: input.worktrees ?? [],
    adoptedWorkspaces: input.adoptedWorkspaces ?? [],
  } as VcsWorktreeCatalogSnapshot;
}

const hiddenPolicy = {
  visibility: "hidden" as const,
  initialPromptDismissedAt: null,
  baselinePaths: [],
};

function session(
  client: WsRpcProtocolClient,
  worktreeCatalog = true,
  worktreeCatalogRefreshReason = true,
): RpcSession {
  return {
    client,
    initialConfig: Effect.succeed({
      environment: { capabilities: { worktreeCatalog, worktreeCatalogRefreshReason } },
    } as never),
    ready: Effect.void,
    probe: Effect.void,
    closed: Effect.never,
    e2eeAuthenticated: Effect.succeed(null),
  };
}

const makeAtomHarnessFromClients = Effect.fn("TestWorktrees.makeAtomHarnessFromClients")(function* (
  clients: ReadonlyMap<EnvironmentId, WsRpcProtocolClient>,
  catalogCapabilities: ReadonlyMap<EnvironmentId, boolean> = new Map(),
) {
  const supervisors = new Map<EnvironmentId, EnvironmentSupervisor["Service"]>();
  for (const [environmentId, client] of clients) {
    supervisors.set(
      environmentId,
      EnvironmentSupervisor.of({
        target: { environmentId, label: environmentId },
        session: yield* SubscriptionRef.make<Option.Option<RpcSession>>(
          Option.some(session(client, catalogCapabilities.get(environmentId) ?? true)),
        ),
      } as never),
    );
  }
  const environmentRegistry = EnvironmentRegistry.of({
    followStream: (environmentId: EnvironmentId, stream: Stream.Stream<unknown, unknown>) =>
      Stream.provideService(stream, EnvironmentSupervisor, supervisors.get(environmentId)!),
  } as never);
  const worktrees = createWorktreeEnvironmentAtoms(
    Atom.runtime(Layer.succeed(EnvironmentRegistry, environmentRegistry)),
  );
  return { worktrees, atomRegistry: AtomRegistry.make(), supervisors };
});

const makeAtomHarness = Effect.fn("TestWorktrees.makeAtomHarness")(function* (
  snapshotsByEnvironment: ReadonlyMap<EnvironmentId, ReadonlyArray<VcsWorktreeCatalogSnapshot>>,
) {
  return yield* makeAtomHarnessFromClients(
    new Map(
      [...snapshotsByEnvironment].map(([environmentId, snapshots]) => [
        environmentId,
        {
          [WS_METHODS.subscribeWorktreeCatalog]: () =>
            Stream.fromIterable(snapshots).pipe(Stream.concat(Stream.never)),
        } as unknown as WsRpcProtocolClient,
      ]),
    ),
  );
});

const makeCommandHarness = Effect.fn("TestWorktrees.makeCommandHarness")(function* (
  clients: ReadonlyMap<EnvironmentId, WsRpcProtocolClient>,
  catalogCapabilities: ReadonlyMap<EnvironmentId, boolean> = new Map(),
  refreshReasonCapabilities: ReadonlyMap<EnvironmentId, boolean> = new Map(),
) {
  const supervisors = new Map<EnvironmentId, EnvironmentSupervisor["Service"]>();
  for (const [environmentId, client] of clients) {
    const supervisorSession = yield* SubscriptionRef.make<Option.Option<RpcSession>>(
      Option.some(
        session(
          client,
          catalogCapabilities.get(environmentId) ?? true,
          refreshReasonCapabilities.get(environmentId) ?? true,
        ),
      ),
    );
    supervisors.set(
      environmentId,
      EnvironmentSupervisor.of({
        target: { environmentId, label: environmentId },
        session: supervisorSession,
      } as never),
    );
  }
  const environmentRegistry = EnvironmentRegistry.of({
    run: <A, E, R>(environmentId: EnvironmentId, effect: Effect.Effect<A, E, R>) =>
      Effect.provideService(effect, EnvironmentSupervisor, supervisors.get(environmentId)!),
  } as never);
  const worktrees = createWorktreeEnvironmentAtoms(
    Atom.runtime(Layer.succeed(EnvironmentRegistry, environmentRegistry)),
  );
  return { worktrees, atomRegistry: AtomRegistry.make(), supervisors };
});

const threadDefaults: WorktreeAdoptInput["threadDefaults"] = {
  modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "gpt-5" },
  runtimeMode: "full-access",
  interactionMode: "default",
};

function addAllInput(
  environmentId: EnvironmentId,
  count: number,
  projectIdForIndex: (index: number) => ProjectId = (index) => ProjectId.make(`project-${index}`),
) {
  return {
    environmentId,
    input: {
      candidates: Array.from({ length: count }, (_, index) => ({
        commandId: CommandId.make(`command-${environmentId}-${index}`),
        projectId: projectIdForIndex(index),
        worktreeKey: WorktreeKey.make(`key-${environmentId}-${index}`),
        expectedGeneration: 7,
        threadDefaults,
      })),
    },
  } as const;
}

function readSnapshot<E>(
  atomRegistry: AtomRegistry.AtomRegistry,
  atom: Atom.Atom<AsyncResult.AsyncResult<VcsWorktreeCatalogSnapshot, E>>,
  generation: number,
) {
  return Effect.gen(function* () {
    for (let attempt = 0; attempt < 100; attempt += 1) {
      const result = atomRegistry.get(atom);
      if (AsyncResult.isSuccess(result) && result.value.generation === generation) {
        return result.value;
      }
      yield* Effect.yieldNow;
    }
    const result = atomRegistry.get(atom);
    return yield* Effect.die(
      `catalog generation ${generation} was not observed: ${AsyncResult.isFailure(result) ? Cause.pretty(result.cause) : result._tag}`,
    );
  });
}

describe("deriveWorktreeDiscoveryState", () => {
  it("derives catalog RPC and legacy detach strategies from one capability policy", () => {
    expect(
      selectWorktreeCatalogCapabilityPolicy({
        capabilities: { worktreeCatalog: true, worktreeCatalogRefreshReason: true },
      } as never),
    ).toEqual({ catalogRpc: "enabled", refreshReason: "enabled", removal: "catalog" });
    expect(
      selectWorktreeCatalogCapabilityPolicy({
        capabilities: { worktreeCatalog: true },
      } as never),
    ).toEqual({ catalogRpc: "enabled", refreshReason: "disabled", removal: "catalog" });
    expect(
      selectWorktreeCatalogCapabilityPolicy({
        capabilities: { worktreeCatalog: false },
      } as never),
    ).toEqual({
      catalogRpc: "disabled",
      refreshReason: "disabled",
      removal: "legacy-detach-only",
    });
  });

  it("gates catalog support from the decoded environment capability", () => {
    expect(
      isWorktreeCatalogSupported({
        capabilities: { worktreeCatalog: false },
      } as never),
    ).toBe(false);
    expect(
      isWorktreeCatalogSupported({
        capabilities: { worktreeCatalog: true },
      } as never),
    ).toBe(true);
  });

  it.each([
    [null, true],
    ["present", true],
    ["verification-unavailable", true],
    ["missing-registered", false],
    ["missing-unregistered", false],
    ["removing", false],
  ] as const)(
    "treats cold and retained degraded rows as usable while blocking only proven %s state",
    (availability, expected) => {
      expect(
        selectWorktreeWorkspaceActionsAvailable(
          availability === null ? null : ({ availability } as VcsAdoptedWorktreeStatus),
        ),
      ).toBe(expected);
    },
  );

  it("expands hidden initial candidates", () => {
    const candidate = descriptor("/repo-worktrees/first");

    const state = deriveWorktreeDiscoveryState({
      snapshot: snapshot({ worktrees: [candidate] }),
      policy: hiddenPolicy,
    });

    expect(state.newCandidates).toEqual([candidate]);
    expect(state.acknowledgedCandidates).toEqual([]);
    expect(state.showInitialPrompt).toBe(true);
    expect(state.showCollapsedHiddenLine).toBe(false);
    expect(state.shownCandidates).toEqual([]);
  });

  it("collapses acknowledged candidates and re-expands for a server-normalized path outside baseline", () => {
    const acknowledged = descriptor("C:\\Repo\\worktrees\\acknowledged");
    const next = descriptor("C:\\REPO\\worktrees\\next");
    const acknowledgedState = deriveWorktreeDiscoveryState({
      snapshot: snapshot({ worktrees: [acknowledged] }),
      policy: {
        visibility: "hidden",
        initialPromptDismissedAt: "2026-08-09T12:00:00.000Z",
        baselinePaths: ["C:\\Repo\\worktrees\\acknowledged"],
      },
    });

    expect(acknowledgedState.newCandidates).toEqual([]);
    expect(acknowledgedState.acknowledgedCandidates).toEqual([acknowledged]);
    expect(acknowledgedState.showInitialPrompt).toBe(false);
    expect(acknowledgedState.showCollapsedHiddenLine).toBe(true);

    const expandedState = deriveWorktreeDiscoveryState({
      snapshot: snapshot({ worktrees: [acknowledged, next] }),
      policy: {
        visibility: "hidden",
        initialPromptDismissedAt: "2026-08-09T12:00:00.000Z",
        baselinePaths: ["C:\\Repo\\worktrees\\acknowledged"],
      },
    });

    expect(expandedState.newCandidates).toEqual([next]);
    expect(expandedState.showInitialPrompt).toBe(true);
    expect(expandedState.showCollapsedHiddenLine).toBe(false);
  });

  it("exposes discovered rows in shown mode without manufacturing workspace threads", () => {
    const candidate = descriptor("/repo-worktrees/shown");

    const state = deriveWorktreeDiscoveryState({
      snapshot: snapshot({ worktrees: [candidate] }),
      policy: { ...hiddenPolicy, visibility: "shown" },
    });

    expect(state.shownCandidates).toEqual([candidate]);
    expect(state.showInitialPrompt).toBe(false);
    expect(state.showCollapsedHiddenLine).toBe(false);
    expect(deriveAdoptedWorkspaceStateByThreadId(snapshot({ worktrees: [candidate] }))).toEqual(
      new Map(),
    );
  });

  it("trusts server eligibility for active, archived, and panel-only joins", () => {
    const active = descriptor("/repo-worktrees/active", {
      adoptionState: "active",
      adoptedThreadId: ThreadId.make("thread-active"),
      eligibleForAdoption: false,
    });
    const archived = descriptor("/repo-worktrees/archived", {
      adoptionState: "archived",
      adoptedThreadId: ThreadId.make("thread-archived"),
      eligibleForAdoption: false,
    });
    const panelOnly = descriptor("/repo-worktrees/panel-only", {
      adoptionState: "none",
      eligibleForAdoption: true,
    });

    const state = deriveWorktreeDiscoveryState({
      snapshot: snapshot({ worktrees: [active, archived, panelOnly] }),
      policy: hiddenPolicy,
    });

    expect(state.newCandidates).toEqual([panelOnly]);
  });
});

describe("worktree catalog atoms", () => {
  it.effect("starts no catalog subscription when the negotiated capability is false", () =>
    Effect.gen(function* () {
      let subscriptionCalls = 0;
      const client = {
        [WS_METHODS.subscribeWorktreeCatalog]: () => {
          subscriptionCalls += 1;
          return Stream.never;
        },
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeAtomHarnessFromClients(
        new Map([[ENVIRONMENT_ONE, client]]),
        new Map([[ENVIRONMENT_ONE, false]]),
      );
      const catalog = harness.worktrees.catalog({
        environmentId: ENVIRONMENT_ONE,
        input: { projectId: PROJECT_ID },
      });

      const unmount = harness.atomRegistry.mount(catalog);
      yield* Effect.yieldNow;
      yield* Effect.yieldNow;

      expect(subscriptionCalls).toBe(0);
      unmount();
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("finalizes the live catalog across reconnect, final unmount, and remount", () =>
    Effect.gen(function* () {
      const subscriptions = { initial: 0, reconnected: 0 };
      let active = 0;
      let maximumActive = 0;
      const finalizers = { initial: 0, reconnected: 0 };
      const initial = snapshot({ worktrees: [descriptor("/repo-worktrees/live")] });
      const reconnected = snapshot({
        generation: 2,
        worktrees: [descriptor("/repo-worktrees/live")],
      });
      const client = (kind: keyof typeof subscriptions, current: VcsWorktreeCatalogSnapshot) =>
        ({
          [WS_METHODS.subscribeWorktreeCatalog]: () =>
            Stream.fromEffect(
              Effect.sync(() => {
                subscriptions[kind] += 1;
                active += 1;
                maximumActive = Math.max(maximumActive, active);
                return current;
              }),
            ).pipe(
              Stream.concat(Stream.never),
              Stream.ensuring(
                Effect.sync(() => {
                  active -= 1;
                  finalizers[kind] += 1;
                }),
              ),
            ),
        }) as unknown as WsRpcProtocolClient;
      const harness = yield* makeAtomHarnessFromClients(
        new Map([[ENVIRONMENT_ONE, client("initial", initial)]]),
      );
      const catalog = harness.worktrees.catalog({
        environmentId: ENVIRONMENT_ONE,
        input: { projectId: PROJECT_ID },
      });

      const unmountFirst = harness.atomRegistry.mount(catalog);
      yield* readSnapshot(harness.atomRegistry, catalog, initial.generation);
      expect({ subscriptions, active, maximumActive, finalizers }).toEqual({
        subscriptions: { initial: 1, reconnected: 0 },
        active: 1,
        maximumActive: 1,
        finalizers: { initial: 0, reconnected: 0 },
      });

      yield* SubscriptionRef.set(
        harness.supervisors.get(ENVIRONMENT_ONE)!.session,
        Option.some(session(client("reconnected", reconnected))),
      );
      yield* readSnapshot(harness.atomRegistry, catalog, reconnected.generation);
      expect({ subscriptions, active, maximumActive, finalizers }).toEqual({
        subscriptions: { initial: 1, reconnected: 1 },
        active: 1,
        maximumActive: 1,
        finalizers: { initial: 1, reconnected: 0 },
      });

      unmountFirst();
      yield* Effect.yieldNow;
      yield* Effect.yieldNow;
      expect({ active, finalizers }).toEqual({
        active: 0,
        finalizers: { initial: 1, reconnected: 1 },
      });

      const unmountSecond = harness.atomRegistry.mount(catalog);
      yield* readSnapshot(harness.atomRegistry, catalog, reconnected.generation);
      expect({ subscriptions, active, maximumActive }).toEqual({
        subscriptions: { initial: 1, reconnected: 2 },
        active: 1,
        maximumActive: 1,
      });

      unmountSecond();
      yield* Effect.yieldNow;
      yield* Effect.yieldNow;
      expect({ active, finalizers }).toEqual({
        active: 0,
        finalizers: { initial: 1, reconnected: 2 },
      });
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("retains last usable rows when a degraded snapshot arrives", () =>
    Effect.gen(function* () {
      const candidate = descriptor("/repo-worktrees/retained");
      const adopted = adoptedWorkspace("thread-1", "/repo-worktrees/adopted");
      const degraded = snapshot({
        generation: 2,
        authoritative: false,
        scanStatus: {
          _tag: "degraded",
          reason: "git-failed",
          message: "git temporarily unavailable",
          failedAt: "2026-08-09T12:01:00.000Z",
          lastAuthoritativeAt: "2026-08-09T12:00:00.000Z",
        },
      });
      const harness = yield* makeAtomHarness(
        new Map([
          [
            ENVIRONMENT_ONE,
            [snapshot({ worktrees: [candidate], adoptedWorkspaces: [adopted] }), degraded],
          ],
        ]),
      );
      const atom = harness.worktrees.catalog({
        environmentId: ENVIRONMENT_ONE,
        input: { projectId: PROJECT_ID },
      });
      const unmount = harness.atomRegistry.mount(atom);

      const retained = yield* readSnapshot(harness.atomRegistry, atom, 2);

      expect(retained.scanStatus._tag).toBe("degraded");
      expect(retained.worktrees).toEqual([candidate]);
      expect(retained.adoptedWorkspaces).toEqual([adopted]);
      unmount();
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("keeps the same grouped project id isolated by environment", () =>
    Effect.gen(function* () {
      const first = snapshot({
        repositoryKey: "repository-1",
        worktrees: [descriptor("/first/worktree")],
      });
      const second = snapshot({
        repositoryKey: "repository-2",
        worktrees: [descriptor("/second/worktree")],
      });
      const harness = yield* makeAtomHarness(
        new Map([
          [ENVIRONMENT_ONE, [first]],
          [ENVIRONMENT_TWO, [second]],
        ]),
      );
      const firstAtom = harness.worktrees.catalog({
        environmentId: ENVIRONMENT_ONE,
        input: { projectId: PROJECT_ID },
      });
      const secondAtom = harness.worktrees.catalog({
        environmentId: ENVIRONMENT_TWO,
        input: { projectId: PROJECT_ID },
      });
      const unmountFirst = harness.atomRegistry.mount(firstAtom);
      const unmountSecond = harness.atomRegistry.mount(secondAtom);

      expect((yield* readSnapshot(harness.atomRegistry, firstAtom, 1)).worktrees[0]?.path).toBe(
        "/first/worktree",
      );
      expect((yield* readSnapshot(harness.atomRegistry, secondAtom, 1)).worktrees[0]?.path).toBe(
        "/second/worktree",
      );

      unmountFirst();
      unmountSecond();
      harness.atomRegistry.dispose();
    }),
  );
});

describe("worktree catalog capability enforcement", () => {
  it.effect(
    "makes no new-method calls and uses only generic thread deletion for legacy detach",
    () =>
      Effect.gen(function* () {
        const worktreeMethodCalls: string[] = [];
        const genericCommands: unknown[] = [];
        const unexpectedUnary = (method: string) => (_input: unknown) =>
          Effect.sync(() => {
            worktreeMethodCalls.push(method);
            throw new Error(`unexpected ${method}`);
          });
        const client = {
          [WS_METHODS.vcsRefreshWorktreeCatalog]: unexpectedUnary(
            WS_METHODS.vcsRefreshWorktreeCatalog,
          ),
          [WS_METHODS.worktreeUpdateDiscoveryPolicy]: unexpectedUnary(
            WS_METHODS.worktreeUpdateDiscoveryPolicy,
          ),
          [WS_METHODS.worktreeAdopt]: unexpectedUnary(WS_METHODS.worktreeAdopt),
          [WS_METHODS.worktreeCreateManaged]: unexpectedUnary(WS_METHODS.worktreeCreateManaged),
          [WS_METHODS.worktreeCreatePanel]: unexpectedUnary(WS_METHODS.worktreeCreatePanel),
          [WS_METHODS.worktreeRetarget]: unexpectedUnary(WS_METHODS.worktreeRetarget),
          [WS_METHODS.worktreeGetRemovalPlan]: unexpectedUnary(WS_METHODS.worktreeGetRemovalPlan),
          [WS_METHODS.worktreeRemoveFromCoddyStudio]: unexpectedUnary(
            WS_METHODS.worktreeRemoveFromCoddyStudio,
          ),
          [WS_METHODS.worktreeRemove]: unexpectedUnary(WS_METHODS.worktreeRemove),
          [ORCHESTRATION_WS_METHODS.dispatchCommand]: (command: unknown) =>
            Effect.sync(() => {
              genericCommands.push(command);
              return { status: "accepted" } as never;
            }),
        } as unknown as WsRpcProtocolClient;
        const harness = yield* makeCommandHarness(
          new Map([[ENVIRONMENT_ONE, client]]),
          new Map([[ENVIRONMENT_ONE, false]]),
        );

        const unsupportedResults = [
          yield* Effect.promise(() =>
            harness.worktrees.refresh.run(harness.atomRegistry, {
              environmentId: ENVIRONMENT_ONE,
              input: { projectId: PROJECT_ID },
            }),
          ),
          yield* Effect.promise(() =>
            harness.worktrees.updatePolicy.run(harness.atomRegistry, {
              environmentId: ENVIRONMENT_ONE,
              input: {
                commandId: CommandId.make("command-policy-legacy"),
                projectId: PROJECT_ID,
                visibility: "hidden",
              },
            }),
          ),
          yield* Effect.promise(() =>
            harness.worktrees.addOne.run(harness.atomRegistry, {
              environmentId: ENVIRONMENT_ONE,
              input: {
                commandId: CommandId.make("command-adopt-legacy"),
                projectId: PROJECT_ID,
                worktreeKey: WorktreeKey.make("worktree-legacy"),
                expectedGeneration: 7,
                threadDefaults,
              },
            }),
          ),
          yield* Effect.promise(() =>
            harness.worktrees.addAll.run(
              harness.atomRegistry,
              addAllInput(ENVIRONMENT_ONE, 1, () => PROJECT_ID),
            ),
          ),
          yield* Effect.promise(() =>
            harness.worktrees.createManaged.run(harness.atomRegistry, {
              environmentId: ENVIRONMENT_ONE,
              input: {
                commandId: CommandId.make("command-create-managed-legacy"),
                projectId: PROJECT_ID,
                threadId: ThreadId.make("thread-create-managed-legacy"),
                title: "Managed worktree",
                refName: "HEAD",
                newRefName: "feature/managed",
                baseRefName: "HEAD",
                threadDefaults,
              },
            }),
          ),
          yield* Effect.promise(() =>
            harness.worktrees.createPanel.run(harness.atomRegistry, {
              environmentId: ENVIRONMENT_ONE,
              input: {
                commandId: CommandId.make("command-create-panel-legacy"),
                hostThreadId: ThreadId.make("thread-host-legacy"),
                threadId: ThreadId.make("thread-panel-legacy"),
                title: "Panel",
                threadDefaults,
              },
            }),
          ),
          yield* Effect.promise(() =>
            harness.worktrees.retarget.run(harness.atomRegistry, {
              environmentId: ENVIRONMENT_ONE,
              input: {
                commandId: CommandId.make("command-retarget-legacy"),
                projectId: PROJECT_ID,
                threadId: ThreadId.make("thread-retarget-legacy"),
                worktreeKey: WorktreeKey.make("worktree-retarget-legacy"),
                expectedGeneration: 7,
              },
            }),
          ),
          yield* Effect.promise(() =>
            harness.worktrees.getRemovalPlan.run(harness.atomRegistry, {
              environmentId: ENVIRONMENT_ONE,
              input: {
                projectId: PROJECT_ID,
                threadId: ThreadId.make("thread-legacy"),
              },
            }),
          ),
          yield* Effect.promise(() =>
            harness.worktrees.remove.run(harness.atomRegistry, {
              environmentId: ENVIRONMENT_ONE,
              input: {
                commandId: CommandId.make("command-remove-legacy"),
                projectId: PROJECT_ID,
                threadId: ThreadId.make("thread-legacy"),
                mode: "delete-git-worktree",
                expectedGeneration: 7,
                planToken: WorktreeRemovalPlanToken.make("plan-legacy"),
                forceDirty: false,
                confirmRepositoryWidePrune: false,
              },
            }),
          ),
        ];
        const detach = yield* Effect.promise(() =>
          harness.worktrees.removeFromCoddyStudio.run(harness.atomRegistry, {
            environmentId: ENVIRONMENT_ONE,
            input: {
              commandId: CommandId.make("command-detach-legacy"),
              projectId: PROJECT_ID,
              threadId: ThreadId.make("thread-legacy"),
            },
          }),
        );

        expect(unsupportedResults.every((result) => result._tag === "Failure")).toBe(true);
        const firstFailure = unsupportedResults[0];
        expect(firstFailure?._tag).toBe("Failure");
        if (firstFailure?._tag === "Failure") {
          expect(Cause.squash(firstFailure.cause as Cause.Cause<unknown>)).toBeInstanceOf(
            WorktreeCatalogUnsupportedError,
          );
        }
        expect(detach).toMatchObject({
          _tag: "Success",
          value: {
            threadRemoved: true,
            gitOutcome: "not-requested",
            orphanCleanupPending: false,
          },
        });
        expect(worktreeMethodCalls).toEqual([]);
        expect(genericCommands).toEqual([
          {
            type: "thread.delete",
            commandId: "command-detach-legacy",
            threadId: "thread-legacy",
          },
        ]);
        expect(genericCommands[0]).not.toHaveProperty("path");
        expect(genericCommands[0]).not.toHaveProperty("cwd");
        harness.atomRegistry.dispose();
      }),
  );

  it.effect("binds a catalog request to the session whose capability was negotiated", () =>
    Effect.gen(function* () {
      let supportedCalls = 0;
      let unsupportedCalls = 0;
      const supportedClient = {
        [WS_METHODS.vcsRefreshWorktreeCatalog]: () =>
          Effect.sync(() => {
            supportedCalls += 1;
            return undefined as never;
          }),
      } as unknown as WsRpcProtocolClient;
      const unsupportedClient = {
        [WS_METHODS.vcsRefreshWorktreeCatalog]: () =>
          Effect.sync(() => {
            unsupportedCalls += 1;
            return undefined as never;
          }),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, supportedClient]]));
      const supervisor = harness.supervisors.get(ENVIRONMENT_ONE)!;
      const unsupportedSession = session(unsupportedClient, false);
      const transitioningSession = {
        ...session(supportedClient, true),
        initialConfig: SubscriptionRef.set(
          supervisor.session,
          Option.some(unsupportedSession),
        ).pipe(Effect.as({ environment: { capabilities: { worktreeCatalog: true } } } as never)),
      } satisfies RpcSession;
      yield* SubscriptionRef.set(supervisor.session, Option.some(transitioningSession));

      const result = yield* Effect.promise(() =>
        harness.worktrees.refresh.run(harness.atomRegistry, {
          environmentId: ENVIRONMENT_ONE,
          input: { projectId: PROJECT_ID },
        }),
      );

      expect(result._tag).toBe("Success");
      expect(supportedCalls).toBe(1);
      expect(unsupportedCalls).toBe(0);
      harness.atomRegistry.dispose();
    }),
  );

  it.effect(
    "routes dedicated create and retarget commands through the negotiated catalog session",
    () =>
      Effect.gen(function* () {
        const calls: Array<{ readonly method: string; readonly input: unknown }> = [];
        const client = {
          [WS_METHODS.worktreeCreateManaged]: (input: unknown) =>
            Effect.sync(() => {
              calls.push({ method: WS_METHODS.worktreeCreateManaged, input });
              return {
                threadId: "thread-managed",
                path: "/repo/.worktrees/managed",
                refName: "feature/managed",
              } as never;
            }),
          [WS_METHODS.worktreeCreatePanel]: (input: unknown) =>
            Effect.sync(() => {
              calls.push({ method: WS_METHODS.worktreeCreatePanel, input });
              return { threadId: "thread-panel" } as never;
            }),
          [WS_METHODS.worktreeRetarget]: (input: unknown) =>
            Effect.sync(() => {
              calls.push({ method: WS_METHODS.worktreeRetarget, input });
              return { threadId: "thread-retarget" } as never;
            }),
        } as unknown as WsRpcProtocolClient;
        const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));
        const managedInput = {
          commandId: CommandId.make("command-managed"),
          projectId: PROJECT_ID,
          threadId: ThreadId.make("thread-managed"),
          title: "Managed worktree",
          refName: "HEAD",
          newRefName: "feature/managed",
          baseRefName: "HEAD",
          threadDefaults,
        } as const;
        const panelInput = {
          commandId: CommandId.make("command-panel"),
          hostThreadId: ThreadId.make("thread-host"),
          threadId: ThreadId.make("thread-panel"),
          title: "Panel",
          threadDefaults,
        } as const;
        const retargetInput = {
          commandId: CommandId.make("command-retarget"),
          projectId: PROJECT_ID,
          threadId: ThreadId.make("thread-retarget"),
          worktreeKey: WorktreeKey.make("worktree-target"),
          expectedGeneration: 12,
        } as const;

        const managed = yield* Effect.promise(() =>
          harness.worktrees.createManaged.run(harness.atomRegistry, {
            environmentId: ENVIRONMENT_ONE,
            input: managedInput,
          }),
        );
        const panel = yield* Effect.promise(() =>
          harness.worktrees.createPanel.run(harness.atomRegistry, {
            environmentId: ENVIRONMENT_ONE,
            input: panelInput,
          }),
        );
        const retarget = yield* Effect.promise(() =>
          harness.worktrees.retarget.run(harness.atomRegistry, {
            environmentId: ENVIRONMENT_ONE,
            input: retargetInput,
          }),
        );

        expect([managed._tag, panel._tag, retarget._tag]).toEqual([
          "Success",
          "Success",
          "Success",
        ]);
        expect(calls).toEqual([
          { method: WS_METHODS.worktreeCreateManaged, input: managedInput },
          { method: WS_METHODS.worktreeCreatePanel, input: panelInput },
          { method: WS_METHODS.worktreeRetarget, input: retargetInput },
        ]);
        for (const call of calls) {
          expect(call.input).not.toHaveProperty("cwd");
          expect(call.input).not.toHaveProperty("path");
          expect(call.input).not.toHaveProperty("worktreePath");
          expect(call.input).not.toHaveProperty("kind");
        }
        harness.atomRegistry.dispose();
      }),
  );
});

describe("worktree adoption commands", () => {
  it.effect("sends the focus reason when the negotiated server supports it", () =>
    Effect.gen(function* () {
      const inputs: unknown[] = [];
      const client = {
        [WS_METHODS.vcsRefreshWorktreeCatalog]: (input: unknown) =>
          Effect.sync(() => {
            inputs.push(input);
            return snapshot({ generation: 8 });
          }),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));

      yield* Effect.promise(() =>
        harness.worktrees.refresh.run(harness.atomRegistry, {
          environmentId: ENVIRONMENT_ONE,
          input: { projectId: PROJECT_ID, reason: "focus" },
        }),
      );

      expect(inputs).toEqual([{ projectId: PROJECT_ID, reason: "focus" }]);
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("omits the focus reason for a catalog server that predates reason support", () =>
    Effect.gen(function* () {
      const inputs: unknown[] = [];
      const client = {
        [WS_METHODS.vcsRefreshWorktreeCatalog]: (input: unknown) =>
          Effect.sync(() => {
            inputs.push(input);
            return snapshot({ generation: 8 });
          }),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(
        new Map([[ENVIRONMENT_ONE, client]]),
        new Map([[ENVIRONMENT_ONE, true]]),
        new Map([[ENVIRONMENT_ONE, false]]),
      );

      yield* Effect.promise(() =>
        harness.worktrees.refresh.run(harness.atomRegistry, {
          environmentId: ENVIRONMENT_ONE,
          input: { projectId: PROJECT_ID, reason: "focus" },
        }),
      );

      expect(inputs).toEqual([{ projectId: PROJECT_ID }]);
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("does not coalesce an explicit refresh with an in-flight focus refresh", () =>
    Effect.gen(function* () {
      const gate = yield* Deferred.make<void>();
      const firstEntered = yield* Deferred.make<void>();
      const inputs: unknown[] = [];
      const refreshed = snapshot({ generation: 8 });
      const client = {
        [WS_METHODS.vcsRefreshWorktreeCatalog]: (input: unknown) =>
          Effect.sync(() => {
            inputs.push(input);
          }).pipe(
            Effect.tap(() => Deferred.succeed(firstEntered, undefined)),
            Effect.andThen(Deferred.await(gate)),
            Effect.as(refreshed),
          ),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(
        new Map([[ENVIRONMENT_ONE, client]]),
        new Map([[ENVIRONMENT_ONE, true]]),
        new Map([[ENVIRONMENT_ONE, false]]),
      );

      const focus = harness.worktrees.refresh.run(harness.atomRegistry, {
        environmentId: ENVIRONMENT_ONE,
        input: { projectId: PROJECT_ID, reason: "focus" },
      });
      yield* Deferred.await(firstEntered);
      const explicit = harness.worktrees.refresh.run(harness.atomRegistry, {
        environmentId: ENVIRONMENT_ONE,
        input: { projectId: PROJECT_ID },
      });
      yield* Effect.yieldNow;
      const callsBeforeRelease = inputs.length;
      yield* Deferred.succeed(gate, undefined);
      yield* Effect.promise(() => Promise.all([focus, explicit]));

      expect(callsBeforeRelease).toBe(2);
      expect(inputs).toEqual([{ projectId: PROJECT_ID }, { projectId: PROJECT_ID }]);
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("coalesces concurrent focus refreshes for one physical project", () =>
    Effect.gen(function* () {
      const gate = yield* Deferred.make<void>();
      const entered = yield* Deferred.make<void>();
      let calls = 0;
      const refreshed = snapshot({ generation: 8 });
      const client = {
        [WS_METHODS.vcsRefreshWorktreeCatalog]: () =>
          Effect.sync(() => {
            calls += 1;
          }).pipe(
            Effect.tap(() => Deferred.succeed(entered, undefined)),
            Effect.andThen(Deferred.await(gate)),
            Effect.as(refreshed),
          ),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));
      const input = {
        environmentId: ENVIRONMENT_ONE,
        input: { projectId: PROJECT_ID, reason: "focus" as const },
      };

      const first = harness.worktrees.refresh.run(harness.atomRegistry, input);
      const second = harness.worktrees.refresh.run(harness.atomRegistry, input);
      yield* Deferred.await(entered);

      expect(calls).toBe(1);
      yield* Deferred.succeed(gate, undefined);
      expect(yield* Effect.promise(() => first)).toMatchObject({
        _tag: "Success",
        value: { generation: 8 },
      });
      expect(yield* Effect.promise(() => second)).toMatchObject({
        _tag: "Success",
        value: { generation: 8 },
      });
      expect(calls).toBe(1);
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("serializes policy and adoption commands for one physical project", () =>
    Effect.gen(function* () {
      const gate = yield* Deferred.make<void>();
      const policyEntered = yield* Deferred.make<void>();
      const events: string[] = [];
      const client = {
        [WS_METHODS.worktreeUpdateDiscoveryPolicy]: () =>
          Effect.sync(() => events.push("policy:start")).pipe(
            Effect.tap(() => Deferred.succeed(policyEntered, undefined)),
            Effect.andThen(Deferred.await(gate)),
            Effect.as(hiddenPolicy),
          ),
        [WS_METHODS.worktreeAdopt]: () =>
          Effect.sync(() => events.push("adopt:start")).pipe(
            Effect.as({ threadId: "thread-adopted", disposition: "created" as const }),
          ),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));
      const policy = harness.worktrees.updatePolicy.run(harness.atomRegistry, {
        environmentId: ENVIRONMENT_ONE,
        input: {
          commandId: CommandId.make("command-policy"),
          projectId: PROJECT_ID,
          visibility: "hidden",
        },
      });
      const adoption = harness.worktrees.addOne.run(harness.atomRegistry, {
        environmentId: ENVIRONMENT_ONE,
        input: {
          commandId: CommandId.make("command-adopt"),
          projectId: PROJECT_ID,
          worktreeKey: WorktreeKey.make("worktree-one"),
          expectedGeneration: 7,
          threadDefaults,
        },
      });
      yield* Deferred.await(policyEntered);

      expect(events).toEqual(["policy:start"]);
      yield* Deferred.succeed(gate, undefined);
      expect((yield* Effect.promise(() => policy))._tag).toBe("Success");
      expect((yield* Effect.promise(() => adoption))._tag).toBe("Success");
      expect(events).toEqual(["policy:start", "adopt:start"]);
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("bounds adoption to four concurrent requests within each environment", () =>
    Effect.gen(function* () {
      const gates = new Map([
        [ENVIRONMENT_ONE, yield* Deferred.make<void>()],
        [ENVIRONMENT_TWO, yield* Deferred.make<void>()],
      ]);
      const fourStarted = new Map([
        [ENVIRONMENT_ONE, yield* Deferred.make<void>()],
        [ENVIRONMENT_TWO, yield* Deferred.make<void>()],
      ]);
      const active = new Map<EnvironmentId, number>();
      const maximum = new Map<EnvironmentId, number>();
      const started = new Map<EnvironmentId, number>();
      const clients = new Map<EnvironmentId, WsRpcProtocolClient>();
      for (const environmentId of [ENVIRONMENT_ONE, ENVIRONMENT_TWO]) {
        clients.set(environmentId, {
          [WS_METHODS.worktreeAdopt]: (input: { worktreeKey: string }) =>
            Effect.acquireUseRelease(
              Effect.sync(() => {
                const nextActive = (active.get(environmentId) ?? 0) + 1;
                const nextStarted = (started.get(environmentId) ?? 0) + 1;
                active.set(environmentId, nextActive);
                maximum.set(environmentId, Math.max(maximum.get(environmentId) ?? 0, nextActive));
                started.set(environmentId, nextStarted);
                return nextStarted;
              }).pipe(
                Effect.tap((nextStarted) =>
                  nextStarted === 4
                    ? Deferred.succeed(fourStarted.get(environmentId)!, undefined)
                    : Effect.void,
                ),
              ),
              () => Deferred.await(gates.get(environmentId)!),
              () =>
                Effect.sync(() => {
                  active.set(environmentId, (active.get(environmentId) ?? 1) - 1);
                }),
            ).pipe(
              Effect.as({
                threadId: `thread-${input.worktreeKey}`,
                disposition: "created" as const,
              }),
            ),
        } as unknown as WsRpcProtocolClient);
      }
      const harness = yield* makeCommandHarness(clients);

      const first = harness.worktrees.addAll.run(
        harness.atomRegistry,
        addAllInput(ENVIRONMENT_ONE, 5),
      );
      const second = harness.worktrees.addAll.run(
        harness.atomRegistry,
        addAllInput(ENVIRONMENT_TWO, 5),
      );
      yield* Deferred.await(fourStarted.get(ENVIRONMENT_ONE)!);
      yield* Deferred.await(fourStarted.get(ENVIRONMENT_TWO)!);

      expect(started.get(ENVIRONMENT_ONE)).toBe(4);
      expect(started.get(ENVIRONMENT_TWO)).toBe(4);
      expect(maximum.get(ENVIRONMENT_ONE)).toBe(4);
      expect(maximum.get(ENVIRONMENT_TWO)).toBe(4);

      yield* Deferred.succeed(gates.get(ENVIRONMENT_ONE)!, undefined);
      yield* Deferred.succeed(gates.get(ENVIRONMENT_TWO)!, undefined);
      expect((yield* Effect.promise(() => first))._tag).toBe("Success");
      expect((yield* Effect.promise(() => second))._tag).toBe("Success");
      expect(started.get(ENVIRONMENT_ONE)).toBe(5);
      expect(started.get(ENVIRONMENT_TWO)).toBe(5);
      harness.atomRegistry.dispose();
    }),
  );

  it.effect(
    "interrupts active bulk adoption and never starts queued candidates after registry unmount",
    () =>
      Effect.gen(function* () {
        const fourStarted = yield* Deferred.make<void>();
        const fourFinalized = yield* Deferred.make<void>();
        const gates = new Map<string, Deferred.Deferred<void>>();
        for (let index = 0; index < 8; index += 1) {
          gates.set(`key-${ENVIRONMENT_ONE}-${index}`, yield* Deferred.make<void>());
        }
        const started: string[] = [];
        const finalized: string[] = [];
        const completed: string[] = [];
        let allowFresh = false;
        const client = {
          [WS_METHODS.worktreeAdopt]: (input: { worktreeKey: string }) => {
            if (allowFresh) {
              return Effect.succeed({
                threadId: `thread-${input.worktreeKey}`,
                disposition: "created" as const,
              });
            }
            return Effect.acquireUseRelease(
              Effect.sync(() => {
                started.push(input.worktreeKey);
                return started.length;
              }).pipe(
                Effect.tap((count) =>
                  count === 4 ? Deferred.succeed(fourStarted, undefined) : Effect.void,
                ),
              ),
              () => Deferred.await(gates.get(input.worktreeKey)!),
              () =>
                Effect.sync(() => {
                  finalized.push(input.worktreeKey);
                  return finalized.length;
                }).pipe(
                  Effect.tap((count) =>
                    count === 4 ? Deferred.succeed(fourFinalized, undefined) : Effect.void,
                  ),
                ),
            ).pipe(
              Effect.tap(() =>
                Effect.sync(() => {
                  completed.push(input.worktreeKey);
                }),
              ),
              Effect.as({
                threadId: `thread-${input.worktreeKey}`,
                disposition: "created" as const,
              }),
            );
          },
        } as unknown as WsRpcProtocolClient;
        const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));
        const pending = harness.worktrees.addAll.run(
          harness.atomRegistry,
          addAllInput(ENVIRONMENT_ONE, 8),
        );
        const cancelled = yield* Deferred.make<Awaited<typeof pending>>();
        void pending.then((result) => {
          Deferred.doneUnsafe(cancelled, Effect.succeed(result));
        });

        yield* Deferred.await(fourStarted);
        expect(started).toEqual([
          `key-${ENVIRONMENT_ONE}-0`,
          `key-${ENVIRONMENT_ONE}-1`,
          `key-${ENVIRONMENT_ONE}-2`,
          `key-${ENVIRONMENT_ONE}-3`,
        ]);

        harness.atomRegistry.reset();
        yield* Deferred.await(fourFinalized);
        const cancelledResult = yield* Deferred.await(cancelled);
        expect(cancelledResult._tag).toBe("Failure");
        if (cancelledResult._tag === "Failure") {
          expect(Cause.hasInterruptsOnly(cancelledResult.cause)).toBe(true);
        }
        expect(finalized.toSorted()).toEqual(started.toSorted());
        expect(harness.atomRegistry.getNodes().size).toBe(0);

        for (const gate of gates.values()) {
          yield* Deferred.succeed(gate, undefined);
        }
        yield* Effect.yieldNow;
        expect(started).toHaveLength(4);
        expect(completed).toEqual([]);

        allowFresh = true;
        const freshResult = yield* Effect.promise(() =>
          harness.worktrees.addAll.run(harness.atomRegistry, addAllInput(ENVIRONMENT_ONE, 1)),
        );
        expect(freshResult).toMatchObject({
          _tag: "Success",
          value: {
            results: [
              {
                _tag: "Success",
                worktreeKey: `key-${ENVIRONMENT_ONE}-0`,
              },
            ],
          },
        });
        expect(started).toHaveLength(4);
        expect(completed).toEqual([]);
        harness.atomRegistry.dispose();
      }),
  );

  it.effect("serializes candidates that share an environment and project", () =>
    Effect.gen(function* () {
      const gate = yield* Deferred.make<void>();
      const firstStarted = yield* Deferred.make<void>();
      let active = 0;
      let maximum = 0;
      let started = 0;
      const client = {
        [WS_METHODS.worktreeAdopt]: (input: { worktreeKey: string }) =>
          Effect.acquireUseRelease(
            Effect.sync(() => {
              active += 1;
              maximum = Math.max(maximum, active);
              started += 1;
              return started;
            }).pipe(
              Effect.tap((nextStarted) =>
                nextStarted === 1 ? Deferred.succeed(firstStarted, undefined) : Effect.void,
              ),
            ),
            () => Deferred.await(gate),
            () =>
              Effect.sync(() => {
                active -= 1;
              }),
          ).pipe(
            Effect.as({ threadId: `thread-${input.worktreeKey}`, disposition: "created" as const }),
          ),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));

      const pending = harness.worktrees.addAll.run(
        harness.atomRegistry,
        addAllInput(ENVIRONMENT_ONE, 3, () => PROJECT_ID),
      );
      yield* Deferred.await(firstStarted);

      expect(started).toBe(1);
      expect(maximum).toBe(1);
      yield* Deferred.succeed(gate, undefined);
      expect((yield* Effect.promise(() => pending))._tag).toBe("Success");
      expect(started).toBe(3);
      expect(maximum).toBe(1);
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("retains successes and stale-generation failures by worktree key", () =>
    Effect.gen(function* () {
      const client = {
        [WS_METHODS.worktreeAdopt]: (input: { worktreeKey: string }) =>
          input.worktreeKey === `key-${ENVIRONMENT_ONE}-1`
            ? Effect.fail(
                new WorktreeAdoptionError({
                  reason: "stale-generation",
                  message: "catalog advanced",
                  currentGeneration: 8,
                }),
              )
            : Effect.succeed({
                threadId: `thread-${input.worktreeKey}`,
                disposition: "created" as const,
              }),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));

      const result = yield* Effect.promise(() =>
        harness.worktrees.addAll.run(harness.atomRegistry, addAllInput(ENVIRONMENT_ONE, 3)),
      );

      expect(result._tag).toBe("Success");
      if (result._tag === "Success") {
        expect(result.value.results.map((item) => [item.worktreeKey, item._tag])).toEqual([
          [`key-${ENVIRONMENT_ONE}-0`, "Success"],
          [`key-${ENVIRONMENT_ONE}-1`, "Failure"],
          [`key-${ENVIRONMENT_ONE}-2`, "Success"],
        ]);
        const failure = result.value.results[1];
        expect(failure?._tag).toBe("Failure");
        if (failure?._tag === "Failure") {
          expect(failure.error).toMatchObject({
            _tag: "WorktreeAdoptionError",
            reason: "stale-generation",
            currentGeneration: 8,
          });
        }
      }
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("returns the thread to navigate to for add-one", () =>
    Effect.gen(function* () {
      const client = {
        [WS_METHODS.worktreeAdopt]: () =>
          Effect.succeed({ threadId: "thread-adopted", disposition: "restored" as const }),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));

      const result = yield* Effect.promise(() =>
        harness.worktrees.addOne.run(harness.atomRegistry, {
          environmentId: ENVIRONMENT_ONE,
          input: {
            commandId: CommandId.make("command-add-one"),
            projectId: PROJECT_ID,
            worktreeKey: WorktreeKey.make("worktree-one"),
            expectedGeneration: 7,
            threadDefaults,
          },
        }),
      );

      expect(result).toMatchObject({
        _tag: "Success",
        value: { threadId: "thread-adopted", disposition: "restored" },
      });
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("does not request navigation after add-all", () =>
    Effect.gen(function* () {
      const client = {
        [WS_METHODS.worktreeAdopt]: (input: { worktreeKey: string }) =>
          Effect.succeed({
            threadId: `thread-${input.worktreeKey}`,
            disposition: "created" as const,
          }),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));

      const result = yield* Effect.promise(() =>
        harness.worktrees.addAll.run(harness.atomRegistry, addAllInput(ENVIRONMENT_ONE, 2)),
      );

      expect(result._tag).toBe("Success");
      if (result._tag === "Success") {
        expect(result.value.results).toHaveLength(2);
        expect("navigateTo" in result.value).toBe(false);
      }
      harness.atomRegistry.dispose();
    }),
  );
});

describe("worktree removal commands", () => {
  it.effect("serializes detach and destructive removal for one physical project", () =>
    Effect.gen(function* () {
      const detachEntered = yield* Deferred.make<void>();
      const detachGate = yield* Deferred.make<void>();
      const events: string[] = [];
      const client = {
        [WS_METHODS.worktreeRemoveFromCoddyStudio]: () =>
          Effect.sync(() => events.push("detach:start")).pipe(
            Effect.tap(() => Deferred.succeed(detachEntered, undefined)),
            Effect.andThen(Deferred.await(detachGate)),
            Effect.as({
              threadRemoved: true,
              gitOutcome: "not-requested" as const,
              orphanCleanupPending: false,
            }),
          ),
        [WS_METHODS.worktreeRemove]: () =>
          Effect.sync(() => events.push("remove:start")).pipe(
            Effect.as({
              threadRemoved: true,
              gitOutcome: "removed" as const,
              orphanCleanupPending: false,
            }),
          ),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));
      const detach = harness.worktrees.removeFromCoddyStudio.run(harness.atomRegistry, {
        environmentId: ENVIRONMENT_ONE,
        input: {
          commandId: CommandId.make("command-detach"),
          projectId: PROJECT_ID,
          threadId: ThreadId.make("thread-one"),
        },
      });
      const remove = harness.worktrees.remove.run(harness.atomRegistry, {
        environmentId: ENVIRONMENT_ONE,
        input: {
          commandId: CommandId.make("command-remove"),
          projectId: PROJECT_ID,
          threadId: ThreadId.make("thread-two"),
          mode: "delete-git-worktree",
          expectedGeneration: 9,
          planToken: WorktreeRemovalPlanToken.make("plan-one"),
          forceDirty: false,
          confirmRepositoryWidePrune: false,
        },
      });

      yield* Deferred.await(detachEntered);
      expect(events).toEqual(["detach:start"]);
      yield* Deferred.succeed(detachGate, undefined);
      expect((yield* Effect.promise(() => detach))._tag).toBe("Success");
      expect((yield* Effect.promise(() => remove))._tag).toBe("Success");
      expect(events).toEqual(["detach:start", "remove:start"]);
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("detaches without requiring a catalog snapshot", () =>
    Effect.gen(function* () {
      const client = {
        [WS_METHODS.worktreeRemoveFromCoddyStudio]: () =>
          Effect.succeed({
            threadRemoved: true,
            gitOutcome: "not-requested" as const,
            orphanCleanupPending: false,
          }),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));

      const result = yield* Effect.promise(() =>
        harness.worktrees.removeFromCoddyStudio.run(harness.atomRegistry, {
          environmentId: ENVIRONMENT_ONE,
          input: {
            commandId: CommandId.make("command-detach-missing"),
            projectId: PROJECT_ID,
            threadId: ThreadId.make("thread-missing"),
          },
        }),
      );

      expect(result).toMatchObject({
        _tag: "Success",
        value: { threadRemoved: true, gitOutcome: "not-requested" },
      });
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("returns a fresh plan after a stale plan without retrying removal", () =>
    Effect.gen(function* () {
      let removalCalls = 0;
      let planCalls = 0;
      const client = {
        [WS_METHODS.worktreeRemove]: () => {
          removalCalls += 1;
          return Effect.fail(
            new WorktreeRemovalError({ reason: "stale-plan", message: "plan changed" }),
          );
        },
        [WS_METHODS.worktreeGetRemovalPlan]: () => {
          planCalls += 1;
          return Effect.succeed({
            planToken: WorktreeRemovalPlanToken.make("plan-fresh"),
            generation: 10,
            availability: "missing-registered" as const,
            registered: true,
            locked: false,
            trackedChangeCount: 0,
            untrackedFileCount: 0,
            pruneImpact: [],
          });
        },
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));

      const result = yield* Effect.promise(() =>
        harness.worktrees.remove.run(harness.atomRegistry, {
          environmentId: ENVIRONMENT_ONE,
          input: {
            commandId: CommandId.make("command-stale-plan"),
            projectId: PROJECT_ID,
            threadId: ThreadId.make("thread-one"),
            mode: "cleanup-stale-registration",
            expectedGeneration: 9,
            planToken: WorktreeRemovalPlanToken.make("plan-stale"),
            forceDirty: false,
            confirmRepositoryWidePrune: false,
          },
        }),
      );

      expect(result).toMatchObject({
        _tag: "Success",
        value: {
          _tag: "PlanChanged",
          plan: { planToken: "plan-fresh", generation: 10 },
        },
      });
      expect({ removalCalls, planCalls }).toEqual({ removalCalls: 1, planCalls: 1 });
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("propagates a session-running refusal without refreshing the plan or retrying", () =>
    Effect.gen(function* () {
      let removalCalls = 0;
      let planCalls = 0;
      const failure = new WorktreeRemovalError({
        reason: "session-running",
        message: "Stop the running session before deleting this worktree.",
      });
      const client = {
        [WS_METHODS.worktreeRemove]: () => {
          removalCalls += 1;
          return Effect.fail(failure);
        },
        [WS_METHODS.worktreeGetRemovalPlan]: () => {
          planCalls += 1;
          return Effect.die("A running session must not refresh the plan.");
        },
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));

      const result = yield* Effect.promise(() =>
        harness.worktrees.remove.run(harness.atomRegistry, {
          environmentId: ENVIRONMENT_ONE,
          input: {
            commandId: CommandId.make("command-session-running"),
            projectId: PROJECT_ID,
            threadId: ThreadId.make("thread-one"),
            mode: "delete-git-worktree",
            expectedGeneration: 9,
            planToken: WorktreeRemovalPlanToken.make("plan-one"),
            forceDirty: false,
            confirmRepositoryWidePrune: false,
          },
        }),
      );

      expect(result._tag).toBe("Failure");
      if (result._tag === "Failure") {
        expect(Cause.squash(result.cause)).toBe(failure);
      }
      expect({ removalCalls, planCalls }).toEqual({ removalCalls: 1, planCalls: 0 });
      harness.atomRegistry.dispose();
    }),
  );

  it.effect("exposes partial stale-registration cleanup without restoring the row", () =>
    Effect.gen(function* () {
      const client = {
        [WS_METHODS.worktreeRemove]: () =>
          Effect.succeed({
            threadRemoved: true,
            gitOutcome: "failed" as const,
            detail: "Remove the stale registration manually with Git.",
            orphanCleanupPending: true,
          }),
      } as unknown as WsRpcProtocolClient;
      const harness = yield* makeCommandHarness(new Map([[ENVIRONMENT_ONE, client]]));

      const result = yield* Effect.promise(() =>
        harness.worktrees.remove.run(harness.atomRegistry, {
          environmentId: ENVIRONMENT_ONE,
          input: {
            commandId: CommandId.make("command-partial"),
            projectId: PROJECT_ID,
            threadId: ThreadId.make("thread-one"),
            mode: "cleanup-stale-registration",
            expectedGeneration: 9,
            planToken: WorktreeRemovalPlanToken.make("plan-one"),
            forceDirty: false,
            confirmRepositoryWidePrune: false,
          },
        }),
      );

      expect(result).toMatchObject({
        _tag: "Success",
        value: {
          _tag: "Removed",
          result: {
            threadRemoved: true,
            gitOutcome: "failed",
            orphanCleanupPending: true,
            detail: "Remove the stale registration manually with Git.",
          },
        },
      });
      harness.atomRegistry.dispose();
    }),
  );
});
