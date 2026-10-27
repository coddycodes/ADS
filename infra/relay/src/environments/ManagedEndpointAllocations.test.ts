import { describe, expect, it } from "@effect/vitest";
import * as NodeCryptoLayer from "@effect/platform-node/NodeCrypto";
import * as Effect from "effect/Effect";
import * as Deferred from "effect/Deferred";
import * as Crypto from "effect/Crypto";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as PlatformError from "effect/PlatformError";
import * as TestClock from "effect/testing/TestClock";
import { PgDialect } from "drizzle-orm/pg-core";

import * as RelayDb from "../db.ts";
import { relayManagedEndpointAllocations } from "../persistence/schema.ts";
import * as ManagedEndpointAllocations from "./ManagedEndpointAllocations.ts";

const ownership: ManagedEndpointAllocations.ManagedEndpointOperation = {
  userId: "user-1",
  environmentId: "env-1",
  kind: "provision",
  generation: 1,
  ownerToken: "test-owner",
};

const layerWithDb = (db: RelayDb.RelayDb["Service"]) => {
  const candidate = db as RelayDb.RelayDb["Service"] & {
    readonly transaction?: RelayDb.RelayDb["Service"]["transaction"];
  };
  const service =
    typeof candidate.transaction === "function"
      ? db
      : ({
          ...db,
          transaction: <A, E, R>(
            work: (transaction: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>,
          ) =>
            work({
              ...db,
              execute: () => Effect.void,
              select: () => ({
                from: () => ({
                  where: () => ({
                    limit: () => Effect.succeed([{ environmentId: ownership.environmentId }]),
                  }),
                }),
              }),
            } as unknown as RelayDb.RelayDb["Service"]),
        } as unknown as RelayDb.RelayDb["Service"]);
  return ManagedEndpointAllocations.layer.pipe(
    Layer.provide(NodeCryptoLayer.layer),
    Layer.provide(Layer.succeed(RelayDb.RelayDb, service)),
  );
};

describe("ManagedEndpointAllocations", () => {
  it("formats allocation errors without embedding provider secrets", () => {
    const error = new ManagedEndpointAllocations.ManagedEndpointAllocationPersistenceError({
      operation: "record-dns",
      stage: "database-request",
      userId: "user-1",
      environmentId: "env-1",
      dnsRecordId: "dns-1",
      cause: new Error("provider-secret"),
    });
    expect(error.message).toBe(
      "Managed endpoint allocation 'record-dns' failed during 'database-request' for user 'user-1', environment 'env-1'",
    );
    expect(JSON.stringify(error)).not.toContain("provider-secret");
  });

  it("resolves only complete allocations under the configured base domain", () => {
    const allocation: ManagedEndpointAllocations.ManagedEndpointAllocation = {
      userId: "user-1",
      environmentId: "env-1",
      hostname: "environment.coddy.test",
      tunnelId: "tunnel-1",
      tunnelName: "tunnel-name",
      dnsRecordId: "dns-1",
      readyAt: "2026-01-01T00:00:00.000Z",
    };
    const resolve = (
      overrides: Partial<ManagedEndpointAllocations.ManagedEndpointAllocation>,
      baseDomain: string | undefined,
    ) =>
      ManagedEndpointAllocations.resolveReadyManagedEndpoint({
        allocation: { ...allocation, ...overrides },
        baseDomain,
      });

    expect(resolve({}, undefined)).toBeNull();
    expect(resolve({ readyAt: null }, "coddy.test")).toBeNull();
    expect(resolve({ tunnelId: null }, "coddy.test")).toBeNull();
    expect(resolve({ dnsRecordId: null }, "coddy.test")).toBeNull();
    expect(resolve({ hostname: "outside.example.test" }, "coddy.test")).toBeNull();
    expect(resolve({}, "coddy.test")).toEqual({
      httpBaseUrl: "https://environment.coddy.test/",
      wsBaseUrl: "wss://environment.coddy.test/ws",
      providerKind: "cloudflare_tunnel",
    });
  });

  it.effect("retains database failures with allocation operation and identity", () => {
    const cause = new Error("database unavailable");
    const fakeDb = {
      select: () => ({
        from: (table: unknown) => {
          expect(table).toBe(relayManagedEndpointAllocations);
          return {
            where: () => ({
              limit: () => Effect.fail(cause),
            }),
          };
        },
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      const error = yield* Effect.flip(
        allocations.get({ userId: "user-1", environmentId: "environment-1" }),
      );

      expect(error).toMatchObject({
        _tag: "ManagedEndpointAllocationPersistenceError",
        operation: "get",
        stage: "database-request",
        userId: "user-1",
        environmentId: "environment-1",
      });
      expect(error.cause).toBe(cause);
    }).pipe(Effect.provide(layerWithDb(fakeDb)));
  });

  it.effect("reports an unresolved reservation without manufacturing a cause", () => {
    const fakeDb = {
      insert: (table: unknown) => {
        expect(table).toBe(relayManagedEndpointAllocations);
        return {
          values: () => ({
            onConflictDoNothing: () => ({
              returning: () => Effect.succeed([]),
            }),
          }),
        };
      },
      update: (table: unknown) => ({
        set: () => ({
          where: () => ({
            returning: () => {
              expect(table).toBe(relayManagedEndpointAllocations);
              return Effect.succeed([]);
            },
          }),
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      const error = yield* Effect.flip(
        allocations.reserve({
          userId: "user-1",
          environmentId: "environment-1",
          hostname: "environment-1.example.test",
          tunnelName: "environment-1-tunnel",
          ownership: { ...ownership, environmentId: "environment-1" },
        }),
      );

      expect(error).toMatchObject({
        _tag: "ManagedEndpointAllocationPersistenceError",
        operation: "reserve",
        stage: "resolve-reservation",
        userId: "user-1",
        environmentId: "environment-1",
        hostname: "environment-1.example.test",
        tunnelName: "environment-1-tunnel",
      });
      expect(error.cause).toBeUndefined();
      expect(error.message).toContain("'resolve-reservation'");
    }).pipe(Effect.provide(layerWithDb(fakeDb)));
  });

  it.effect("loads existing allocations and returns null when absent", () => {
    const allocation: ManagedEndpointAllocations.ManagedEndpointAllocation = {
      userId: "user-1",
      environmentId: "env-1",
      hostname: "env-1.coddy.test",
      tunnelId: null,
      tunnelName: "tunnel-1",
      dnsRecordId: null,
      readyAt: null,
    };
    let call = 0;
    const fakeDb = {
      select: () => ({
        from: () => ({
          where: () => ({ limit: () => Effect.succeed(call++ === 0 ? [allocation] : []) }),
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      expect(yield* allocations.get({ userId: "user-1", environmentId: "env-1" })).toEqual(
        allocation,
      );
      expect(yield* allocations.get({ userId: "user-1", environmentId: "missing" })).toBeNull();
    }).pipe(Effect.provide(layerWithDb(fakeDb)));
  });

  it.effect("returns a newly inserted reservation without a follow-up lookup", () => {
    const allocation: ManagedEndpointAllocations.ManagedEndpointAllocation = {
      userId: "user-1",
      environmentId: "env-1",
      hostname: "env-1.coddy.test",
      tunnelId: null,
      tunnelName: "tunnel-1",
      dnsRecordId: null,
      readyAt: null,
    };
    const fakeDb = {
      insert: () => ({
        values: () => ({
          onConflictDoNothing: () => ({ returning: () => Effect.succeed([allocation]) }),
        }),
      }),
      select: () => {
        throw new Error("lookup must not run");
      },
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      expect(
        yield* allocations.reserve({
          userId: "user-1",
          environmentId: "env-1",
          hostname: allocation.hostname,
          tunnelName: allocation.tunnelName,
          ownership,
        }),
      ).toEqual(allocation);
    }).pipe(Effect.provide(layerWithDb(fakeDb)));
  });

  it.effect("resolves a concurrent reservation collision from the stored winner", () => {
    const winner: ManagedEndpointAllocations.ManagedEndpointAllocation = {
      userId: "user-1",
      environmentId: "env-1",
      hostname: "winner.coddy.test",
      tunnelId: "tunnel-winner",
      tunnelName: "winner",
      dnsRecordId: null,
      readyAt: null,
    };
    const fakeDb = {
      insert: () => ({
        values: () => ({
          onConflictDoNothing: () => ({ returning: () => Effect.succeed([]) }),
        }),
      }),
      update: () => ({
        set: () => ({ where: () => ({ returning: () => Effect.succeed([winner]) }) }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      expect(
        yield* allocations.reserve({
          userId: "user-1",
          environmentId: "env-1",
          hostname: "candidate.coddy.test",
          tunnelName: "candidate",
          ownership,
        }),
      ).toEqual(winner);
    }).pipe(Effect.provide(layerWithDb(fakeDb)));
  });

  it.effect("maps reservation insert and collision-lookup failures", () => {
    const insertCause = new Error("insert failed");
    const lookupCause = new Error("lookup failed");
    const reserve = (db: RelayDb.RelayDb["Service"]) =>
      Effect.gen(function* () {
        const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
        return yield* Effect.flip(
          allocations.reserve({
            userId: "user-1",
            environmentId: "env-1",
            hostname: "env-1.coddy.test",
            tunnelName: "tunnel-1",
            ownership,
          }),
        );
      }).pipe(Effect.provide(layerWithDb(db)));
    const insertDb = {
      insert: () => ({
        values: () => ({
          onConflictDoNothing: () => ({ returning: () => Effect.fail(insertCause) }),
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];
    const lookupDb = {
      insert: () => ({
        values: () => ({
          onConflictDoNothing: () => ({ returning: () => Effect.succeed([]) }),
        }),
      }),
      update: () => ({
        set: () => ({ where: () => ({ returning: () => Effect.fail(lookupCause) }) }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      expect(yield* reserve(insertDb)).toMatchObject({
        operation: "reserve",
        stage: "database-request",
        cause: insertCause,
      });
      expect(yield* reserve(lookupDb)).toMatchObject({
        operation: "reserve",
        stage: "database-request",
        cause: lookupCause,
      });
    });
  });

  it.effect("checkpoints tunnel, DNS, readiness, and removal", () => {
    const sets: Array<Record<string, unknown>> = [];
    let deletes = 0;
    const fakeDb = {
      update: () => ({
        set: (values: Record<string, unknown>) => {
          sets.push(values);
          return {
            where: () => ({
              returning: () => Effect.succeed([{ environmentId: "env-1" }]),
            }),
          };
        },
      }),
      delete: () => ({
        where: () => ({
          returning: () =>
            Effect.sync(() => {
              deletes++;
              return [{ environmentId: "env-1" }];
            }),
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      const key = { userId: "user-1", environmentId: "env-1", ownership };
      yield* allocations.recordTunnel({ ...key, tunnelId: "tunnel-1" });
      yield* allocations.recordDns({ ...key, dnsRecordId: "dns-1" });
      yield* allocations.markReady(key);
      yield* allocations.remove(key);

      expect(sets[0]).toMatchObject({ tunnelId: "tunnel-1" });
      expect(sets[1]).toMatchObject({ dnsRecordId: "dns-1" });
      expect(sets[2]?.readyAt).toBe(sets[2]?.updatedAt);
      expect(deletes).toBe(1);
    }).pipe(Effect.provide(layerWithDb(fakeDb)));
  });

  it.effect("maps every checkpoint persistence failure with operation context", () => {
    const failures = [
      new Error("record tunnel failed"),
      new Error("record dns failed"),
      new Error("mark ready failed"),
      new Error("remove failed"),
    ];
    let update = 0;
    const fakeDb = {
      update: () => ({
        set: () => ({
          where: () => ({ returning: () => Effect.fail(failures[update++]!) }),
        }),
      }),
      delete: () => ({
        where: () => ({ returning: () => Effect.fail(failures[3]!) }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      const key = { userId: "user-1", environmentId: "env-1", ownership };
      const tunnel = yield* Effect.flip(allocations.recordTunnel({ ...key, tunnelId: "tunnel-1" }));
      const dns = yield* Effect.flip(allocations.recordDns({ ...key, dnsRecordId: "dns-1" }));
      const ready = yield* Effect.flip(allocations.markReady(key));
      const remove = yield* Effect.flip(allocations.remove(key));

      expect(tunnel).toMatchObject({ operation: "record-tunnel", cause: failures[0] });
      expect(dns).toMatchObject({ operation: "record-dns", cause: failures[1] });
      expect(ready).toMatchObject({ operation: "mark-ready", cause: failures[2] });
      expect(remove).toMatchObject({ operation: "remove", cause: failures[3] });
    }).pipe(Effect.provide(layerWithDb(fakeDb)));
  });

  it.effect("acquires the next database-backed environment operation generation", () => {
    const events: Array<string> = [];
    const tx = {
      execute: () => Effect.sync(() => events.push("lock")),
      insert: () => ({
        values: () => ({ onConflictDoNothing: () => Effect.sync(() => events.push("ensure-row")) }),
      }),
      select: () => ({
        from: () => ({
          where: () => ({
            limit: () =>
              Effect.succeed([
                {
                  environmentId: "env-1",
                  generation: 7,
                  ownerToken: null,
                  leaseExpiresAt: null,
                },
              ]),
          }),
        }),
      }),
      update: () => ({
        set: () => ({
          where: () => ({
            returning: () =>
              Effect.sync(() => {
                events.push("claim");
                return [{ generation: 8 }];
              }),
          }),
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];
    const db = {
      transaction: <A, E, R>(
        work: (transaction: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>,
      ) => work(tx),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      const operation = yield* allocations.acquireOperation({
        userId: "user-1",
        environmentId: "env-1",
        kind: "provision",
      });

      expect(operation).toMatchObject({
        userId: "user-1",
        environmentId: "env-1",
        kind: "provision",
        generation: 8,
      });
      expect(operation.ownerToken).toMatch(/^[0-9a-f-]{36}$/u);
      expect(events).toEqual(["lock", "ensure-row", "claim"]);
    }).pipe(Effect.provide(layerWithDb(db)));
  });

  it.effect("rejects a checkpoint from a stale operation owner", () => {
    const fakeDb = {
      update: () => ({
        set: () => ({
          where: () => ({ returning: () => Effect.succeed([]) }),
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      const error = yield* Effect.flip(
        allocations.recordTunnel({
          userId: "user-1",
          environmentId: "env-1",
          tunnelId: "stale-tunnel",
          ownership: {
            userId: "user-1",
            environmentId: "env-1",
            kind: "provision",
            generation: 4,
            ownerToken: "stale-owner",
          },
        }),
      );

      expect(error).toMatchObject({
        operation: "record-tunnel",
        stage: "ownership-lost",
      });
    }).pipe(Effect.provide(layerWithDb(fakeDb)));
  });

  it.effect("renews only a current unexpired operation lease", () => {
    const whereConditions: Array<unknown> = [];
    const leaseUpdates: Array<Record<string, unknown>> = [];
    let calls = 0;
    const fakeDb = {
      update: () => ({
        set: (values: Record<string, unknown>) => {
          leaseUpdates.push(values);
          return {
            where: (condition: unknown) => {
              whereConditions.push(condition);
              return {
                returning: () => Effect.succeed(calls++ === 0 ? [{ environmentId: "env-1" }] : []),
              };
            },
          };
        },
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const service = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      yield* service.renewOperation(ownership);
      const stale = yield* Effect.flip(service.renewOperation(ownership));

      expect(stale).toMatchObject({ operation: "renew-operation", stage: "ownership-lost" });
      expect(leaseUpdates).toHaveLength(2);
      expect(Date.parse(String(leaseUpdates[0]?.leaseExpiresAt))).toBeGreaterThan(
        Date.parse(String(leaseUpdates[0]?.updatedAt)),
      );
      const query = new PgDialect().sqlToQuery(whereConditions[0] as never);
      expect(query.sql).toContain('"relay_environment_operations"."owner_token" =');
      expect(query.sql).toContain('"relay_environment_operations"."owner_user_id" =');
      expect(query.sql).toContain('"relay_environment_operations"."lease_expires_at" >');
      expect(query.params).toContain(ownership.ownerToken);
      expect(query.params).toContain(ownership.userId);
    }).pipe(Effect.provide(layerWithDb(fakeDb)));
  });

  it.effect("releases only the matching operation generation and owner", () => {
    const updatedTables: Array<unknown> = [];
    const released: Array<Record<string, unknown>> = [];
    const tx = {
      execute: () => Effect.void,
      update: (table: unknown) => ({
        set: (values: Record<string, unknown>) => {
          updatedTables.push(table);
          released.push(values);
          return {
            where: () => ({
              returning: () => Effect.succeed([{ environmentId: "env-1" }]),
            }),
          };
        },
      }),
    } as unknown as RelayDb.RelayDb["Service"];
    const fakeDb = {
      ...tx,
      transaction: <A, E, R>(
        work: (transaction: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>,
      ) => work(tx),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      yield* allocations.releaseOperation({
        userId: "user-1",
        environmentId: "env-1",
        kind: "unlink",
        generation: 5,
        ownerToken: "current-owner",
      });
      expect(updatedTables).toEqual([expect.anything(), relayManagedEndpointAllocations]);
      expect(released).toEqual([
        expect.objectContaining({
          ownerToken: null,
          ownerUserId: null,
          operationKind: null,
          leaseExpiresAt: null,
        }),
        expect.objectContaining({ operationOwnerToken: null }),
      ]);
    }).pipe(Effect.provide(layerWithDb(fakeDb)));
  });

  it.effect("rejects stale reservation and write ownership after operation release", () => {
    const whereConditions: Array<unknown> = [];
    let insertAttempted = false;
    const tx = {
      execute: () => Effect.void,
      select: () => ({
        from: () => ({ where: () => ({ limit: () => Effect.succeed([]) }) }),
      }),
      insert: () => {
        insertAttempted = true;
        throw new Error("a released operation must not reserve an allocation");
      },
    } as unknown as RelayDb.RelayDb["Service"];
    const fakeDb = {
      transaction: <A, E, R>(
        work: (transaction: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>,
      ) => work(tx),
      update: () => ({
        set: () => ({
          where: (condition: unknown) => {
            whereConditions.push(condition);
            return { returning: () => Effect.succeed([]) };
          },
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const service = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      const reservationError = yield* Effect.flip(
        service.reserve({
          userId: ownership.userId,
          environmentId: ownership.environmentId,
          hostname: "env-1.coddy.test",
          tunnelName: "env-1",
          ownership,
        }),
      );
      const checkpointError = yield* Effect.flip(
        service.recordTunnel({
          userId: ownership.userId,
          environmentId: ownership.environmentId,
          tunnelId: "stale-tunnel",
          ownership,
        }),
      );
      expect(yield* service.claimForOperation(ownership)).toBeNull();

      expect(reservationError).toMatchObject({ operation: "reserve", stage: "ownership-lost" });
      expect(checkpointError).toMatchObject({
        operation: "record-tunnel",
        stage: "ownership-lost",
      });
      expect(insertAttempted).toBe(false);
      expect(whereConditions).toHaveLength(2);
      const dialect = new PgDialect();
      for (const condition of whereConditions) {
        const query = dialect.sqlToQuery(condition as never);
        expect(query.sql).toContain('exists (select "environment_id"');
        expect(query.sql).toContain('from "relay_environment_operations"');
        expect(query.sql).toContain('"owner_token" =');
        expect(query.sql).toContain('"owner_user_id" =');
        expect(query.params).toContain(ownership.ownerToken);
      }
    }).pipe(Effect.provide(layerWithDb(fakeDb)));
  });

  it.effect("brackets an environment operation around the owned workflow", () => {
    const events: Array<string> = [];
    const tx = {
      execute: () => Effect.sync(() => events.push("acquire-lock")),
      insert: () => ({
        values: () => ({ onConflictDoNothing: () => Effect.void }),
      }),
      select: () => ({
        from: () => ({
          where: () => ({
            limit: () =>
              Effect.succeed([{ generation: 0, ownerToken: null, leaseExpiresAt: null }]),
          }),
        }),
      }),
      update: () => ({
        set: (values: { readonly ownerToken?: string | null }) => ({
          where: () => ({
            returning: () =>
              Effect.sync(() => {
                if (values.ownerToken !== undefined) {
                  events.push(values.ownerToken === null ? "release" : "acquire");
                }
                return values.ownerToken === null
                  ? [{ environmentId: "env-1" }]
                  : [{ generation: 1 }];
              }),
          }),
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];
    const db = {
      ...tx,
      transaction: <A, E, R>(
        work: (transaction: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>,
      ) => work(tx),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      const generation = yield* allocations.withOperation(
        { userId: "user-1", environmentId: "env-1", kind: "link" },
        (operation) =>
          Effect.sync(() => {
            events.push("use");
            return operation.generation;
          }),
      );

      expect(generation).toBe(1);
      expect(events).toEqual(["acquire-lock", "acquire", "use", "acquire-lock", "release"]);
    }).pipe(Effect.provide(layerWithDb(db)));
  });

  it.effect("waits for an active environment operation lease before reacquiring", () =>
    Effect.gen(function* () {
      const firstAttempt = yield* Deferred.make<void>();
      let attempts = 0;
      const tx = {
        execute: () => Effect.void,
        insert: () => ({ values: () => ({ onConflictDoNothing: () => Effect.void }) }),
        select: () => ({
          from: () => ({
            where: () => ({
              limit: () =>
                Effect.gen(function* () {
                  const attempt = attempts++;
                  if (attempt < 2) {
                    if (attempt === 0) yield* Deferred.succeed(firstAttempt, undefined);
                    return [
                      {
                        generation: 1,
                        ownerToken: "other-owner",
                        leaseExpiresAt: attempt === 0 ? null : "9999-01-01T00:00:00.000Z",
                      },
                    ];
                  }
                  return [{ generation: 1, ownerToken: null, leaseExpiresAt: null }];
                }),
            }),
          }),
        }),
        update: () => ({
          set: () => ({
            where: () => ({ returning: () => Effect.succeed([{ generation: 2 }]) }),
          }),
        }),
      } as unknown as RelayDb.RelayDb["Service"];
      const db = {
        ...tx,
        transaction: <A, E, R>(
          work: (transaction: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>,
        ) => work(tx),
      } as unknown as RelayDb.RelayDb["Service"];
      const run = Effect.gen(function* () {
        const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
        return yield* allocations.withOperation(
          { userId: "user-1", environmentId: "env-1", kind: "unlink" },
          (operation) => Effect.succeed(operation.generation),
        );
      }).pipe(Effect.provide(layerWithDb(db)));

      const fiber = yield* Effect.forkChild(run);
      yield* Deferred.await(firstAttempt);
      yield* TestClock.adjust("40 millis");
      expect(yield* Fiber.join(fiber)).toBe(2);
      expect(attempts).toBe(3);
    }),
  );

  it.effect("delivers a committed result even when operation release fails", () => {
    let transaction = 0;
    const releaseCause = new Error("release unavailable");
    const tx = {
      execute: () => Effect.void,
      insert: () => ({ values: () => ({ onConflictDoNothing: () => Effect.void }) }),
      select: () => ({
        from: () => ({
          where: () => ({
            limit: () =>
              Effect.succeed([{ generation: 0, ownerToken: null, leaseExpiresAt: null }]),
          }),
        }),
      }),
      update: () => ({
        set: (values: { readonly ownerToken?: string | null }) => ({
          where: () => ({
            returning: () =>
              values.ownerToken === null
                ? Effect.fail(releaseCause)
                : Effect.succeed([{ generation: 1 }]),
          }),
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];
    const db = {
      transaction: <A, E, R>(
        work: (transaction: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>,
      ) => {
        transaction++;
        return work(tx);
      },
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      const result = yield* allocations.withOperation(
        { userId: "user-1", environmentId: "env-1", kind: "link" },
        () => Effect.succeed("committed"),
      );
      expect(result).toBe("committed");
      expect(transaction).toBe(2);
    }).pipe(Effect.provide(layerWithDb(db)));
  });

  it.effect("caps owner-without-lease polling after six exponential attempts", () =>
    Effect.gen(function* () {
      const attempted = yield* Deferred.make<void>();
      let attempts = 0;
      const tx = {
        execute: () => Effect.void,
        insert: () => ({ values: () => ({ onConflictDoNothing: () => Effect.void }) }),
        select: () => ({
          from: () => ({
            where: () => ({
              limit: () =>
                Effect.sync(() => {
                  attempts++;
                  return [{ generation: 4, ownerToken: "corrupt-owner", leaseExpiresAt: null }];
                }).pipe(Effect.tap(() => Deferred.succeed(attempted, undefined))),
            }),
          }),
        }),
      } as unknown as RelayDb.RelayDb["Service"];
      const db = {
        transaction: <A, E, R>(
          work: (transaction: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>,
        ) => work(tx),
      } as unknown as RelayDb.RelayDb["Service"];
      const run = Effect.gen(function* () {
        const service = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
        return yield* service.withOperation(
          { userId: "user-1", environmentId: "env-1", kind: "link" },
          () => Effect.void,
        );
      }).pipe(Effect.provide(layerWithDb(db)));

      const fiber = yield* Effect.forkChild(Effect.exit(run));
      yield* Deferred.await(attempted);
      yield* TestClock.adjust("310 millis");
      const result = yield* Fiber.join(fiber);

      expect(Exit.isFailure(result)).toBe(true);
      if (Exit.isFailure(result)) {
        expect(result.cause.reasons).toEqual([
          expect.objectContaining({ error: expect.objectContaining({ stage: "operation-busy" }) }),
        ]);
      }
      expect(attempts).toBe(6);
    }),
  );

  it.effect("cancels operation polling without issuing another transaction", () =>
    Effect.gen(function* () {
      const attempted = yield* Deferred.make<void>();
      let attempts = 0;
      const tx = {
        execute: () => Effect.void,
        insert: () => ({ values: () => ({ onConflictDoNothing: () => Effect.void }) }),
        select: () => ({
          from: () => ({
            where: () => ({
              limit: () =>
                Effect.sync(() => {
                  attempts++;
                  return [
                    {
                      generation: 1,
                      ownerToken: "other-owner",
                      leaseExpiresAt: "9999-01-01T00:00:00.000Z",
                    },
                  ];
                }).pipe(Effect.tap(() => Deferred.succeed(attempted, undefined))),
            }),
          }),
        }),
      } as unknown as RelayDb.RelayDb["Service"];
      const db = {
        transaction: <A, E, R>(
          work: (transaction: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>,
        ) => work(tx),
      } as unknown as RelayDb.RelayDb["Service"];
      const run = Effect.gen(function* () {
        const service = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
        return yield* service.withOperation(
          { userId: "user-1", environmentId: "env-1", kind: "unlink" },
          () => Effect.void,
        );
      }).pipe(Effect.provide(layerWithDb(db)));

      const fiber = yield* Effect.forkChild(run);
      yield* Deferred.await(attempted);
      yield* Fiber.interrupt(fiber);
      yield* TestClock.adjust("1 second");

      expect(attempts).toBe(1);
    }),
  );

  it.effect("maps operation acquisition failures and stale compare-and-set results", () =>
    Effect.gen(function* () {
      const input = { userId: "user-1", environmentId: "env-1", kind: "provision" as const };
      const run = (db: RelayDb.RelayDb["Service"]) =>
        Effect.gen(function* () {
          const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
          return yield* Effect.flip(allocations.acquireOperation(input));
        }).pipe(Effect.provide(layerWithDb(db)));
      const transactionDb = (current: unknown, claimed: ReadonlyArray<unknown>) => {
        const tx = {
          execute: () => Effect.void,
          insert: () => ({ values: () => ({ onConflictDoNothing: () => Effect.void }) }),
          select: () => ({
            from: () => ({
              where: () => ({ limit: () => Effect.succeed([current].filter(Boolean)) }),
            }),
          }),
          update: () => ({
            set: () => ({ where: () => ({ returning: () => Effect.succeed(claimed) }) }),
          }),
        } as unknown as RelayDb.RelayDb["Service"];
        return {
          transaction: <A, E, R>(
            work: (transaction: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>,
          ) => work(tx),
        } as unknown as RelayDb.RelayDb["Service"];
      };
      const missing = yield* run(transactionDb(undefined, []));
      const stale = yield* run(
        transactionDb({ generation: 3, ownerToken: null, leaseExpiresAt: null }, []),
      );
      const databaseCause = new Error("transaction unavailable");
      const database = yield* run({
        transaction: () => Effect.fail(databaseCause),
      } as unknown as RelayDb.RelayDb["Service"]);
      const nodeCrypto = yield* Crypto.Crypto;
      const uuidCause = PlatformError.badArgument({
        module: "Crypto",
        method: "randomUUIDv4",
        description: "unavailable",
      });
      const uuid = yield* Effect.gen(function* () {
        const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
        return yield* Effect.flip(allocations.acquireOperation(input));
      }).pipe(
        Effect.provide(
          ManagedEndpointAllocations.layer.pipe(
            Layer.provide(
              Layer.succeed(Crypto.Crypto, {
                ...nodeCrypto,
                randomUUIDv4: Effect.fail(uuidCause),
              }),
            ),
            Layer.provide(Layer.succeed(RelayDb.RelayDb, {} as RelayDb.RelayDb["Service"])),
          ),
        ),
      );

      expect(missing).toMatchObject({ stage: "ownership-lost" });
      expect(stale).toMatchObject({ stage: "ownership-lost" });
      expect(database).toMatchObject({ stage: "database-request", cause: databaseCause });
      expect(uuid).toMatchObject({ stage: "database-request", cause: uuidCause });
    }).pipe(Effect.provide(NodeCryptoLayer.layer)),
  );

  it.effect("maps release and allocation-claim persistence outcomes", () => {
    const operation: ManagedEndpointAllocations.ManagedEndpointOperation = {
      userId: "user-1",
      environmentId: "env-1",
      kind: "unlink",
      generation: 4,
      ownerToken: "owner-4",
    };
    const allocation: ManagedEndpointAllocations.ManagedEndpointAllocation = {
      userId: "user-1",
      environmentId: "env-1",
      hostname: "env.coddy.test",
      tunnelId: null,
      tunnelName: "tunnel",
      dnsRecordId: null,
      readyAt: null,
    };
    const run = <A, E>(
      db: RelayDb.RelayDb["Service"],
      use: (
        service: ManagedEndpointAllocations.ManagedEndpointAllocations["Service"],
      ) => Effect.Effect<A, E>,
    ) =>
      Effect.gen(function* () {
        const service = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
        return yield* use(service);
      }).pipe(Effect.provide(layerWithDb(db)));
    const updateDb = (result: Effect.Effect<ReadonlyArray<unknown>, Error>) =>
      ({
        update: () => ({
          set: () => ({ where: () => ({ returning: () => result }) }),
        }),
      }) as unknown as RelayDb.RelayDb["Service"];
    const releaseCause = new Error("release unavailable");
    const claimCause = new Error("claim unavailable");

    return Effect.gen(function* () {
      const releaseFailure = yield* Effect.flip(
        run(updateDb(Effect.fail(releaseCause)), (service) => service.releaseOperation(operation)),
      );
      const renewFailure = yield* Effect.flip(
        run(updateDb(Effect.fail(releaseCause)), (service) => service.renewOperation(operation)),
      );
      const releaseStale = yield* Effect.flip(
        run(updateDb(Effect.succeed([])), (service) => service.releaseOperation(operation)),
      );
      const claimed = yield* run(updateDb(Effect.succeed([allocation])), (service) =>
        service.claimForOperation(operation),
      );
      const absent = yield* run(updateDb(Effect.succeed([])), (service) =>
        service.claimForOperation(operation),
      );
      const claimFailure = yield* Effect.flip(
        run(updateDb(Effect.fail(claimCause)), (service) => service.claimForOperation(operation)),
      );

      expect(releaseFailure).toMatchObject({
        operation: "release-operation",
        stage: "database-request",
        cause: releaseCause,
      });
      expect(releaseStale).toMatchObject({ stage: "ownership-lost" });
      expect(renewFailure).toMatchObject({
        operation: "renew-operation",
        stage: "database-request",
        cause: releaseCause,
      });
      expect(claimed).toEqual(allocation);
      expect(absent).toBeNull();
      expect(claimFailure).toMatchObject({
        operation: "claim-allocation",
        cause: claimCause,
      });
    });
  });

  it.effect("rejects every stale owned checkpoint and removal", () => {
    const fakeDb = {
      update: () => ({
        set: () => ({ where: () => ({ returning: () => Effect.succeed([]) }) }),
      }),
      delete: () => ({ where: () => ({ returning: () => Effect.succeed([]) }) }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const service = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
      const key = { userId: "user-1", environmentId: "env-1", ownership };
      const errors = yield* Effect.forEach(
        [
          Effect.flip(service.recordDns({ ...key, dnsRecordId: "dns" })),
          Effect.flip(service.markReady(key)),
          Effect.flip(service.remove(key)),
        ],
        (effect) => effect,
      );
      expect(errors.map((error) => error.stage)).toEqual([
        "ownership-lost",
        "ownership-lost",
        "ownership-lost",
      ]);
    }).pipe(Effect.provide(layerWithDb(fakeDb)));
  });
});
