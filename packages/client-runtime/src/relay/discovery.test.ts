import { EnvironmentId } from "@coddy/contracts";
import type {
  RelayClientEnvironmentRecord,
  RelayEnvironmentStatusResponse,
} from "@coddy/contracts/relay";
import { describe, expect, it } from "@effect/vitest";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Ref from "effect/Ref";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";

import * as ManagedRelay from "./managedRelay.ts";
import * as ClientCapabilities from "../platform/capabilities.ts";
import * as Connectivity from "../connection/connectivity.ts";
import { ConnectionBlockedError, type NetworkStatus } from "../connection/model.ts";
import * as ConnectionWakeups from "../connection/wakeups.ts";
import * as RelayEnvironmentDiscovery from "./discovery.ts";

const environments = [
  {
    environmentId: EnvironmentId.make("environment-1"),
    label: "Environment One",
    endpoint: {
      httpBaseUrl: "https://one.example.test",
      wsBaseUrl: "wss://one.example.test",
      providerKind: "cloudflare_tunnel",
    },
    linkedAt: "2026-06-01T00:00:00.000Z",
  },
  {
    environmentId: EnvironmentId.make("environment-2"),
    label: "Environment Two",
    endpoint: {
      httpBaseUrl: "https://two.example.test",
      wsBaseUrl: "wss://two.example.test",
      providerKind: "cloudflare_tunnel",
    },
    linkedAt: "2026-06-01T00:00:00.000Z",
  },
] satisfies ReadonlyArray<RelayClientEnvironmentRecord>;

function status(
  environment: RelayClientEnvironmentRecord,
  value: "online" | "offline",
): RelayEnvironmentStatusResponse {
  return {
    environmentId: environment.environmentId,
    endpoint: environment.endpoint,
    status: value,
    checkedAt: "2026-06-01T00:00:00.000Z",
  };
}

function relayToken(subject: string): string {
  return `${Buffer.from("{}").toString("base64url")}.${Buffer.from(
    JSON.stringify({ sub: subject }),
  ).toString("base64url")}.signature`;
}

const makeHarness = Effect.fn("RelayDiscoveryTest.makeHarness")(function* () {
  const networkStatus = yield* SubscriptionRef.make<NetworkStatus>("online");
  const listCalls = yield* Ref.make(0);
  const listFailure = yield* Ref.make<ManagedRelay.ManagedRelayClientError | null>(null);
  const secondListCall = yield* Deferred.make<void>();
  const clerkToken = yield* Ref.make<string | null>("clerk-token");
  const wakeups = yield* SubscriptionRef.make<{
    readonly sequence: number;
    readonly reason: "application-active" | "credentials-changed";
  }>({
    sequence: 0,
    reason: "application-active",
  });
  const statusRequests = yield* Ref.make(
    new Map<
      string,
      Deferred.Deferred<RelayEnvironmentStatusResponse, ManagedRelay.ManagedRelayClientError>
    >(),
  );
  for (const environment of environments) {
    const request = yield* Deferred.make<
      RelayEnvironmentStatusResponse,
      ManagedRelay.ManagedRelayClientError
    >();
    yield* Ref.update(statusRequests, (current) => {
      const next = new Map(current);
      next.set(environment.environmentId, request);
      return next;
    });
  }

  const client = ManagedRelay.ManagedRelayClient.of({
    relayUrl: "https://relay.example.test",
    listEnvironments: () =>
      Effect.gen(function* () {
        const count = yield* Ref.updateAndGet(listCalls, (current) => current + 1);
        if (count >= 2) {
          yield* Deferred.succeed(secondListCall, undefined);
        }
        const failure = yield* Ref.get(listFailure);
        if (failure) {
          return yield* failure;
        }
        return environments;
      }),
    getEnvironmentStatus: ({ environmentId }) =>
      Ref.get(statusRequests).pipe(
        Effect.flatMap((requests) => Deferred.await(requests.get(environmentId)!)),
      ),
    createEnvironmentLinkChallenge: () => Effect.die("unused"),
    linkEnvironment: () => Effect.die("unused"),
    unlinkEnvironment: () => Effect.die("unused"),
    connectEnvironment: () => Effect.die("unused"),
    resetTokenCache: Effect.void,
  } satisfies ManagedRelay.ManagedRelayClient["Service"]);
  const connectivity = Connectivity.Connectivity.of({
    status: SubscriptionRef.get(networkStatus),
    changes: SubscriptionRef.changes(networkStatus),
  });
  const layer = RelayEnvironmentDiscovery.layer.pipe(
    Layer.provide(
      Layer.mergeAll(
        Layer.succeed(ManagedRelay.ManagedRelayClient, client),
        Layer.succeed(
          ClientCapabilities.CloudSession,
          ClientCapabilities.CloudSession.of({
            clerkToken: Ref.get(clerkToken).pipe(
              Effect.flatMap((token) =>
                token === null
                  ? Effect.fail(
                      new ConnectionBlockedError({
                        reason: "authentication",
                        detail: "Signed out.",
                      }),
                    )
                  : Effect.succeed(token),
              ),
            ),
          }),
        ),
        Layer.succeed(Connectivity.Connectivity, connectivity),
        Layer.succeed(
          ConnectionWakeups.ConnectionWakeups,
          ConnectionWakeups.ConnectionWakeups.of({
            focusVisibility: Stream.never,
            changes: SubscriptionRef.changes(wakeups).pipe(
              Stream.drop(1),
              Stream.map((event) => event.reason),
            ),
          }),
        ),
      ),
    ),
  );

  return {
    layer,
    listCalls,
    listFailure,
    clerkToken,
    networkStatus,
    secondListCall,
    statusRequests,
    wake: (reason: "application-active" | "credentials-changed") =>
      SubscriptionRef.update(wakeups, (event) => ({
        sequence: event.sequence + 1,
        reason,
      })),
  };
});

describe("RelayEnvironmentDiscovery", () => {
  it.effect("rejects relay status records that do not match the linked environment", () =>
    Effect.gen(function* () {
      const environment = environments[0]!;
      const valid = status(environment, "online");

      expect(yield* RelayEnvironmentDiscovery.validateStatus(environment, valid)).toBe(valid);

      const mismatches: ReadonlyArray<RelayEnvironmentStatusResponse> = [
        { ...valid, environmentId: EnvironmentId.make("different") },
        {
          ...valid,
          endpoint: { ...valid.endpoint, httpBaseUrl: "https://different.example.test" },
        },
        {
          ...valid,
          endpoint: { ...valid.endpoint, wsBaseUrl: "wss://different.example.test" },
        },
        {
          ...valid,
          endpoint: { ...valid.endpoint, providerKind: "manual" },
        },
        {
          ...valid,
          descriptor: {
            environmentId: EnvironmentId.make("different"),
          } as RelayEnvironmentStatusResponse["descriptor"],
        },
      ];

      for (const mismatch of mismatches) {
        const error = yield* Effect.flip(
          RelayEnvironmentDiscovery.validateStatus(environment, mismatch),
        );
        expect(error).toBeInstanceOf(ConnectionBlockedError);
      }
    }),
  );

  it("extracts only non-empty relay account subjects", () => {
    const token = (payload: object) =>
      `${Buffer.from("{}").toString("base64url")}.${Buffer.from(JSON.stringify(payload)).toString("base64url")}.signature`;

    expect(
      Option.getOrNull(RelayEnvironmentDiscovery.relayAccountId(token({ sub: "user-1" }))),
    ).toBe("user-1");
    expect(Option.isNone(RelayEnvironmentDiscovery.relayAccountId(token({})))).toBe(true);
    expect(Option.isNone(RelayEnvironmentDiscovery.relayAccountId(token({ sub: "" })))).toBe(true);
    expect(Option.isNone(RelayEnvironmentDiscovery.relayAccountId("not-a-token"))).toBe(true);
  });

  it.effect("publishes each environment status as soon as that lookup completes", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      yield* Effect.gen(function* () {
        const discovery = yield* RelayEnvironmentDiscovery.RelayEnvironmentDiscovery;
        const refreshFiber = yield* Effect.forkChild(discovery.refresh);

        const checking = yield* SubscriptionRef.changes(discovery.state).pipe(
          Stream.filter((state) => state.environments.size === 2),
          Stream.runHead,
          Effect.map(Option.getOrThrow),
        );
        expect(
          [...checking.environments.values()].every((entry) => entry.availability === "checking"),
        ).toBe(true);

        const requests = yield* Ref.get(harness.statusRequests);
        yield* Deferred.succeed(
          requests.get(environments[1]!.environmentId)!,
          status(environments[1]!, "online"),
        );

        const partiallyResolved = yield* SubscriptionRef.changes(discovery.state).pipe(
          Stream.filter(
            (state) =>
              state.environments.get(environments[1]!.environmentId)?.availability === "online",
          ),
          Stream.runHead,
          Effect.map(Option.getOrThrow),
        );
        expect(
          partiallyResolved.environments.get(environments[0]!.environmentId)?.availability,
        ).toBe("checking");

        yield* Deferred.succeed(
          requests.get(environments[0]!.environmentId)!,
          status(environments[0]!, "offline"),
        );
        yield* Fiber.join(refreshFiber);

        const complete = yield* SubscriptionRef.get(discovery.state);
        expect(complete.environments.get(environments[0]!.environmentId)?.availability).toBe(
          "offline",
        );
        expect(complete.refreshing).toBe(false);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect(
    "deduplicates offline reports, clears them on recovery, and tracks account changes",
    () =>
      Effect.gen(function* () {
        const harness = yield* makeHarness();
        yield* Ref.set(harness.clerkToken, relayToken("user-1"));
        yield* Effect.gen(function* () {
          const discovery = yield* RelayEnvironmentDiscovery.RelayEnvironmentDiscovery;
          const requests = yield* Ref.get(harness.statusRequests);
          yield* Deferred.succeed(
            requests.get(environments[0]!.environmentId)!,
            status(environments[0]!, "offline"),
          );
          yield* Deferred.succeed(
            requests.get(environments[1]!.environmentId)!,
            status(environments[1]!, "online"),
          );

          yield* discovery.refresh;
          yield* discovery.refresh;

          yield* Ref.set(harness.clerkToken, relayToken("user-2"));
          yield* discovery.refresh;

          const recovered = yield* Deferred.make<
            RelayEnvironmentStatusResponse,
            ManagedRelay.ManagedRelayClientError
          >();
          yield* Deferred.succeed(recovered, status(environments[0]!, "online"));
          yield* Ref.update(harness.statusRequests, (current) =>
            new Map(current).set(environments[0]!.environmentId, recovered),
          );
          yield* discovery.refresh;

          expect(
            (yield* SubscriptionRef.get(discovery.state)).environments.get(
              environments[0]!.environmentId,
            )?.availability,
          ).toBe("online");
        }).pipe(Effect.provide(harness.layer));
      }),
  );

  it.effect("stays offline and ignores wakeups before the first refresh", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      yield* SubscriptionRef.set(harness.networkStatus, "offline");
      yield* Effect.gen(function* () {
        const discovery = yield* RelayEnvironmentDiscovery.RelayEnvironmentDiscovery;
        yield* harness.wake("application-active");
        yield* harness.wake("credentials-changed");
        for (let index = 0; index < 10; index += 1) {
          yield* Effect.yieldNow;
        }
        expect(yield* Ref.get(harness.listCalls)).toBe(0);

        yield* discovery.refresh;
        expect(yield* SubscriptionRef.get(discovery.state)).toMatchObject({
          offline: true,
          refreshing: false,
        });
        expect(yield* Ref.get(harness.listCalls)).toBe(0);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect(
    "preserves discovered rows while offline and refreshes after connectivity returns",
    () =>
      Effect.gen(function* () {
        const harness = yield* makeHarness();
        yield* Effect.gen(function* () {
          const discovery = yield* RelayEnvironmentDiscovery.RelayEnvironmentDiscovery;
          const requests = yield* Ref.get(harness.statusRequests);
          for (const environment of environments) {
            yield* Deferred.succeed(
              requests.get(environment.environmentId)!,
              status(environment, "online"),
            );
          }
          yield* discovery.refresh;

          const offlineFiber = yield* SubscriptionRef.changes(discovery.state).pipe(
            Stream.filter((state) => state.offline),
            Stream.runHead,
            Effect.forkChild,
          );
          yield* SubscriptionRef.set(harness.networkStatus, "offline");
          yield* Fiber.join(offlineFiber);
          expect((yield* SubscriptionRef.get(discovery.state)).environments.size).toBe(2);

          yield* SubscriptionRef.set(harness.networkStatus, "online");
          yield* Deferred.await(harness.secondListCall);
          expect(yield* Ref.get(harness.listCalls)).toBe(2);
        }).pipe(Effect.provide(harness.layer));
      }),
  );

  it.effect("publishes listing failures without rejecting the refresh command", () =>
    Effect.gen(function* () {
      const networkStatus = yield* SubscriptionRef.make<NetworkStatus>("online");
      const client = ManagedRelay.ManagedRelayClient.of({
        relayUrl: "https://relay.example.test",
        listEnvironments: () =>
          Effect.fail(
            new ManagedRelay.ManagedRelayRequestTimeoutError({
              activity: "Relay environment listing",
              timeoutMs: ManagedRelay.MANAGED_RELAY_REQUEST_TIMEOUT_MS,
            }),
          ),
        getEnvironmentStatus: () => Effect.die("unused"),
        createEnvironmentLinkChallenge: () => Effect.die("unused"),
        linkEnvironment: () => Effect.die("unused"),
        unlinkEnvironment: () => Effect.die("unused"),
        connectEnvironment: () => Effect.die("unused"),
        resetTokenCache: Effect.void,
      } satisfies ManagedRelay.ManagedRelayClient["Service"]);
      const layer = RelayEnvironmentDiscovery.layer.pipe(
        Layer.provide(
          Layer.mergeAll(
            Layer.succeed(ManagedRelay.ManagedRelayClient, client),
            Layer.succeed(ClientCapabilities.CloudSession, {
              clerkToken: Effect.succeed("clerk-token"),
            }),
            Layer.succeed(Connectivity.Connectivity, {
              status: SubscriptionRef.get(networkStatus),
              changes: SubscriptionRef.changes(networkStatus),
            }),
            Layer.succeed(
              ConnectionWakeups.ConnectionWakeups,
              ConnectionWakeups.ConnectionWakeups.of({
                focusVisibility: Stream.never,
                changes: Stream.never,
              }),
            ),
          ),
        ),
      );

      yield* Effect.gen(function* () {
        const discovery = yield* RelayEnvironmentDiscovery.RelayEnvironmentDiscovery;
        yield* discovery.refresh;

        const state = yield* SubscriptionRef.get(discovery.state);
        expect(state.refreshing).toBe(false);
        expect(Option.getOrThrow(state.error)).toMatchObject({
          _tag: "ConnectionTransientError",
          reason: "timeout",
          message: "Relay environment listing timed out.",
        });
      }).pipe(Effect.provide(layer));
    }),
  );

  it.effect("clears previously discovered rows when a refresh fails", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      yield* Effect.gen(function* () {
        const discovery = yield* RelayEnvironmentDiscovery.RelayEnvironmentDiscovery;
        const requests = yield* Ref.get(harness.statusRequests);
        for (const environment of environments) {
          yield* Deferred.succeed(
            requests.get(environment.environmentId)!,
            status(environment, "online"),
          );
        }
        yield* discovery.refresh;
        expect((yield* SubscriptionRef.get(discovery.state)).environments.size).toBe(2);

        yield* Ref.set(
          harness.listFailure,
          new ManagedRelay.ManagedRelayRequestFailedError({
            action: "list relay-managed environments",
            cause: new Error("Relay request failed."),
          }),
        );
        yield* discovery.refresh;

        const failed = yield* SubscriptionRef.get(discovery.state);
        expect(failed.environments.size).toBe(0);
        expect(Option.isSome(failed.error)).toBe(true);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("does not republish stale rows after sign-out invalidates an in-flight refresh", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness();
      yield* Effect.gen(function* () {
        const discovery = yield* RelayEnvironmentDiscovery.RelayEnvironmentDiscovery;
        const refreshFiber = yield* Effect.forkChild(discovery.refresh);
        yield* SubscriptionRef.changes(discovery.state).pipe(
          Stream.filter((state) => state.environments.size === environments.length),
          Stream.runHead,
        );

        yield* Ref.set(harness.clerkToken, null);
        yield* harness.wake("credentials-changed");
        yield* SubscriptionRef.changes(discovery.state).pipe(
          Stream.filter((state) => state.environments.size === 0),
          Stream.runHead,
        );

        const requests = yield* Ref.get(harness.statusRequests);
        for (const environment of environments) {
          yield* Deferred.succeed(
            requests.get(environment.environmentId)!,
            status(environment, "online"),
          );
        }
        yield* Fiber.join(refreshFiber);
        yield* Effect.yieldNow;

        expect((yield* SubscriptionRef.get(discovery.state)).environments.size).toBe(0);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );
});
