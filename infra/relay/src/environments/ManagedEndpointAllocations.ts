import type { RelayManagedEndpoint } from "@coddy/contracts/relay";
import { and, eq, exists, gt, isNotNull, sql } from "drizzle-orm";
import * as Context from "effect/Context";
import * as Crypto from "effect/Crypto";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schedule from "effect/Schedule";
import * as Schema from "effect/Schema";
import { QueryBuilder } from "drizzle-orm/pg-core";

import * as RelayDb from "../db.ts";
import { isManagedEndpointHostname, managedEndpointForHostname } from "../deploymentConfig.ts";
import {
  relayEnvironmentOperations,
  relayManagedEndpointAllocations,
} from "../persistence/schema.ts";

export interface ManagedEndpointAllocation {
  readonly userId: string;
  readonly environmentId: string;
  readonly hostname: string;
  readonly tunnelId: string | null;
  readonly tunnelName: string;
  readonly dnsRecordId: string | null;
  readonly readyAt: string | null;
}

export function resolveReadyManagedEndpoint(input: {
  readonly allocation: ManagedEndpointAllocation;
  readonly baseDomain: string | undefined;
}): RelayManagedEndpoint | null {
  if (
    !input.baseDomain ||
    input.allocation.readyAt === null ||
    input.allocation.tunnelId === null ||
    input.allocation.dnsRecordId === null ||
    !isManagedEndpointHostname(input.allocation.hostname, input.baseDomain)
  ) {
    return null;
  }
  return managedEndpointForHostname(input.allocation.hostname);
}

export class ManagedEndpointAllocationPersistenceError extends Schema.TaggedError<ManagedEndpointAllocationPersistenceError>()(
  "ManagedEndpointAllocationPersistenceError",
  {
    operation: Schema.Literals([
      "get",
      "reserve",
      "record-tunnel",
      "record-dns",
      "mark-ready",
      "remove",
      "acquire-operation",
      "release-operation",
      "renew-operation",
      "claim-allocation",
    ]),
    stage: Schema.Literals([
      "database-request",
      "resolve-reservation",
      "operation-busy",
      "ownership-lost",
    ]),
    userId: Schema.String,
    environmentId: Schema.String,
    hostname: Schema.optionalKey(Schema.String),
    tunnelName: Schema.optionalKey(Schema.String),
    tunnelId: Schema.optionalKey(Schema.String),
    dnsRecordId: Schema.optionalKey(Schema.String),
    cause: Schema.optional(Schema.Defect()),
  },
) {
  override get message(): string {
    return `Managed endpoint allocation '${this.operation}' failed during '${this.stage}' for user '${this.userId}', environment '${this.environmentId}'`;
  }
}

const isManagedEndpointAllocationPersistenceError = Schema.is(
  ManagedEndpointAllocationPersistenceError,
);

export type ManagedEndpointOperationKind =
  | "link"
  | "unlink"
  | "provision"
  | "deprovision"
  | "compensate";

export interface ManagedEndpointOperation {
  readonly userId: string;
  readonly environmentId: string;
  readonly kind: ManagedEndpointOperationKind;
  readonly generation: number;
  readonly ownerToken: string;
}

export const MANAGED_ENDPOINT_OPERATION_LEASE_MILLIS = 5 * 60 * 1_000;

const operationLeaseExpiresAt = (now: DateTime.DateTime) =>
  DateTime.formatIso(DateTime.addDuration(now, MANAGED_ENDPOINT_OPERATION_LEASE_MILLIS));

interface ManagedEndpointAllocationKey {
  readonly userId: string;
  readonly environmentId: string;
}

interface ReserveManagedEndpointAllocationInput extends ManagedEndpointAllocationKey {
  readonly hostname: string;
  readonly tunnelName: string;
  readonly ownership: ManagedEndpointOperation;
}

interface RecordManagedEndpointTunnelInput extends ManagedEndpointAllocationKey {
  readonly tunnelId: string;
  readonly ownership: ManagedEndpointOperation;
}

interface RecordManagedEndpointDnsInput extends ManagedEndpointAllocationKey {
  readonly dnsRecordId: string;
  readonly ownership: ManagedEndpointOperation;
}

interface OwnedManagedEndpointAllocationKey extends ManagedEndpointAllocationKey {
  readonly ownership: ManagedEndpointOperation;
}

export class ManagedEndpointAllocations extends Context.Service<
  ManagedEndpointAllocations,
  {
    readonly withOperation: <A, E, R>(
      input: {
        readonly userId: string;
        readonly environmentId: string;
        readonly kind: ManagedEndpointOperationKind;
      },
      use: (operation: ManagedEndpointOperation) => Effect.Effect<A, E, R>,
    ) => Effect.Effect<A, E | ManagedEndpointAllocationPersistenceError, R>;
    readonly acquireOperation: (input: {
      readonly userId: string;
      readonly environmentId: string;
      readonly kind: ManagedEndpointOperationKind;
    }) => Effect.Effect<ManagedEndpointOperation, ManagedEndpointAllocationPersistenceError>;
    readonly releaseOperation: (
      operation: ManagedEndpointOperation,
    ) => Effect.Effect<void, ManagedEndpointAllocationPersistenceError>;
    readonly renewOperation: (
      operation: ManagedEndpointOperation,
    ) => Effect.Effect<void, ManagedEndpointAllocationPersistenceError>;
    readonly claimForOperation: (
      operation: ManagedEndpointOperation,
    ) => Effect.Effect<ManagedEndpointAllocation | null, ManagedEndpointAllocationPersistenceError>;
    readonly get: (
      input: ManagedEndpointAllocationKey,
    ) => Effect.Effect<ManagedEndpointAllocation | null, ManagedEndpointAllocationPersistenceError>;
    readonly reserve: (
      input: ReserveManagedEndpointAllocationInput,
    ) => Effect.Effect<ManagedEndpointAllocation, ManagedEndpointAllocationPersistenceError>;
    readonly recordTunnel: (
      input: RecordManagedEndpointTunnelInput,
    ) => Effect.Effect<void, ManagedEndpointAllocationPersistenceError>;
    readonly recordDns: (
      input: RecordManagedEndpointDnsInput,
    ) => Effect.Effect<void, ManagedEndpointAllocationPersistenceError>;
    readonly markReady: (
      input: OwnedManagedEndpointAllocationKey,
    ) => Effect.Effect<void, ManagedEndpointAllocationPersistenceError>;
    readonly remove: (
      input: OwnedManagedEndpointAllocationKey,
    ) => Effect.Effect<void, ManagedEndpointAllocationPersistenceError>;
  }
>()("coddy-studio-relay/environments/ManagedEndpointAllocations") {}

const allocationSelection = {
  userId: relayManagedEndpointAllocations.userId,
  environmentId: relayManagedEndpointAllocations.environmentId,
  hostname: relayManagedEndpointAllocations.hostname,
  tunnelId: relayManagedEndpointAllocations.tunnelId,
  tunnelName: relayManagedEndpointAllocations.tunnelName,
  dnsRecordId: relayManagedEndpointAllocations.dnsRecordId,
  readyAt: relayManagedEndpointAllocations.readyAt,
};

const whereAllocation = (input: ManagedEndpointAllocationKey) =>
  and(
    eq(relayManagedEndpointAllocations.userId, input.userId),
    eq(relayManagedEndpointAllocations.environmentId, input.environmentId),
  );

const whereAllocationOwnership = (input: OwnedManagedEndpointAllocationKey) =>
  and(
    whereAllocation(input),
    eq(relayManagedEndpointAllocations.operationGeneration, input.ownership.generation),
    eq(relayManagedEndpointAllocations.operationOwnerToken, input.ownership.ownerToken),
  );

export const make = Effect.gen(function* () {
  const db = yield* RelayDb.RelayDb;
  const crypto = yield* Crypto.Crypto;
  const whereCurrentOperation = (operation: ManagedEndpointOperation) =>
    and(
      eq(relayEnvironmentOperations.environmentId, operation.environmentId),
      eq(relayEnvironmentOperations.generation, operation.generation),
      eq(relayEnvironmentOperations.ownerToken, operation.ownerToken),
      eq(relayEnvironmentOperations.ownerUserId, operation.userId),
    );
  const currentOperationExists = (operation: ManagedEndpointOperation) =>
    exists(
      new QueryBuilder()
        .select({ environmentId: relayEnvironmentOperations.environmentId })
        .from(relayEnvironmentOperations)
        .where(whereCurrentOperation(operation)),
    );
  const whereCurrentAllocationOwnership = (input: OwnedManagedEndpointAllocationKey) =>
    and(whereAllocationOwnership(input), currentOperationExists(input.ownership));

  let service: ManagedEndpointAllocations["Service"];
  const waitForOperation = Schedule.max([
    Schedule.exponential("10 millis"),
    Schedule.recurs(5),
  ]).pipe(
    Schedule.while(
      ({ input }) =>
        isManagedEndpointAllocationPersistenceError(input) && input.stage === "operation-busy",
    ),
  );
  service = ManagedEndpointAllocations.of({
    withOperation: (input, use) =>
      Effect.acquireUseRelease(
        service.acquireOperation(input).pipe(Effect.retry(waitForOperation)),
        use,
        (operation) =>
          service
            .releaseOperation(operation)
            .pipe(
              Effect.catch(() =>
                Effect.logWarning(
                  "Managed endpoint operation lease release failed; the fenced lease will expire",
                ),
              ),
            ),
      ),
    acquireOperation: Effect.fn("relay.managed_endpoint_allocations.acquire_operation")(
      function* (input) {
        const ownerToken = yield* crypto.randomUUIDv4.pipe(
          Effect.mapError(
            (cause) =>
              new ManagedEndpointAllocationPersistenceError({
                operation: "acquire-operation",
                stage: "database-request",
                ...input,
                cause,
              }),
          ),
        );
        const now = yield* DateTime.now;
        const nowIso = DateTime.formatIso(now);
        const leaseExpiresAt = operationLeaseExpiresAt(now);
        return yield* db
          .transaction((tx) =>
            Effect.gen(function* () {
              yield* tx.execute(
                sql`select pg_advisory_xact_lock(hashtextextended(${`environment:${input.environmentId}`}, 0))`,
              );
              yield* tx
                .insert(relayEnvironmentOperations)
                .values({
                  environmentId: input.environmentId,
                  generation: 0,
                  ownerToken: null,
                  ownerUserId: null,
                  operationKind: null,
                  leaseExpiresAt: null,
                  createdAt: nowIso,
                  updatedAt: nowIso,
                })
                .onConflictDoNothing();
              const rows = yield* tx
                .select({
                  generation: relayEnvironmentOperations.generation,
                  ownerToken: relayEnvironmentOperations.ownerToken,
                  leaseExpiresAt: relayEnvironmentOperations.leaseExpiresAt,
                })
                .from(relayEnvironmentOperations)
                .where(eq(relayEnvironmentOperations.environmentId, input.environmentId))
                .limit(1);
              const current = rows[0];
              if (
                current === undefined ||
                (current.ownerToken !== null &&
                  (current.leaseExpiresAt === null || current.leaseExpiresAt > nowIso))
              ) {
                return yield* new ManagedEndpointAllocationPersistenceError({
                  operation: "acquire-operation",
                  stage: current === undefined ? "ownership-lost" : "operation-busy",
                  ...input,
                });
              }
              const generation = current.generation + 1;
              const claimed = yield* tx
                .update(relayEnvironmentOperations)
                .set({
                  generation,
                  ownerToken,
                  ownerUserId: input.userId,
                  operationKind: input.kind,
                  leaseExpiresAt,
                  updatedAt: nowIso,
                })
                .where(
                  and(
                    eq(relayEnvironmentOperations.environmentId, input.environmentId),
                    eq(relayEnvironmentOperations.generation, current.generation),
                  ),
                )
                .returning({ generation: relayEnvironmentOperations.generation });
              if (claimed.length === 0) {
                return yield* new ManagedEndpointAllocationPersistenceError({
                  operation: "acquire-operation",
                  stage: "ownership-lost",
                  ...input,
                });
              }
              return { ...input, generation, ownerToken };
            }),
          )
          .pipe(
            Effect.mapError((cause) =>
              isManagedEndpointAllocationPersistenceError(cause)
                ? cause
                : new ManagedEndpointAllocationPersistenceError({
                    operation: "acquire-operation",
                    stage: "database-request",
                    ...input,
                    cause,
                  }),
            ),
          );
      },
    ),
    releaseOperation: Effect.fn("relay.managed_endpoint_allocations.release_operation")(
      function* (operation) {
        const now = DateTime.formatIso(yield* DateTime.now);
        yield* db
          .transaction((tx) =>
            Effect.gen(function* () {
              yield* tx.execute(
                sql`select pg_advisory_xact_lock(hashtextextended(${`environment:${operation.environmentId}`}, 0))`,
              );
              const released = yield* tx
                .update(relayEnvironmentOperations)
                .set({
                  ownerToken: null,
                  ownerUserId: null,
                  operationKind: null,
                  leaseExpiresAt: null,
                  updatedAt: now,
                })
                .where(whereCurrentOperation(operation))
                .returning({ environmentId: relayEnvironmentOperations.environmentId });
              if (released.length === 0) {
                return yield* new ManagedEndpointAllocationPersistenceError({
                  operation: "release-operation",
                  stage: "ownership-lost",
                  ...operation,
                });
              }
              yield* tx
                .update(relayManagedEndpointAllocations)
                .set({ operationOwnerToken: null, updatedAt: now })
                .where(
                  and(
                    whereAllocation(operation),
                    eq(relayManagedEndpointAllocations.operationGeneration, operation.generation),
                    eq(relayManagedEndpointAllocations.operationOwnerToken, operation.ownerToken),
                  ),
                )
                .returning({ environmentId: relayManagedEndpointAllocations.environmentId });
            }),
          )
          .pipe(
            Effect.mapError((cause) =>
              isManagedEndpointAllocationPersistenceError(cause)
                ? cause
                : new ManagedEndpointAllocationPersistenceError({
                    operation: "release-operation",
                    stage: "database-request",
                    ...operation,
                    cause,
                  }),
            ),
          );
      },
    ),
    renewOperation: Effect.fn("relay.managed_endpoint_allocations.renew_operation")(
      function* (operation) {
        const now = yield* DateTime.now;
        const nowIso = DateTime.formatIso(now);
        const renewed = yield* db
          .update(relayEnvironmentOperations)
          .set({
            leaseExpiresAt: operationLeaseExpiresAt(now),
            updatedAt: nowIso,
          })
          .where(
            and(
              whereCurrentOperation(operation),
              isNotNull(relayEnvironmentOperations.leaseExpiresAt),
              gt(relayEnvironmentOperations.leaseExpiresAt, nowIso),
            ),
          )
          .returning({ environmentId: relayEnvironmentOperations.environmentId })
          .pipe(
            Effect.mapError(
              (cause) =>
                new ManagedEndpointAllocationPersistenceError({
                  operation: "renew-operation",
                  stage: "database-request",
                  ...operation,
                  cause,
                }),
            ),
          );
        if (renewed.length === 0) {
          return yield* new ManagedEndpointAllocationPersistenceError({
            operation: "renew-operation",
            stage: "ownership-lost",
            ...operation,
          });
        }
      },
    ),
    claimForOperation: Effect.fn("relay.managed_endpoint_allocations.claim_for_operation")(
      function* (operation) {
        const rows = yield* db
          .update(relayManagedEndpointAllocations)
          .set({
            operationGeneration: operation.generation,
            operationOwnerToken: operation.ownerToken,
            updatedAt: DateTime.formatIso(yield* DateTime.now),
          })
          .where(and(whereAllocation(operation), currentOperationExists(operation)))
          .returning(allocationSelection)
          .pipe(
            Effect.mapError(
              (cause) =>
                new ManagedEndpointAllocationPersistenceError({
                  operation: "claim-allocation",
                  stage: "database-request",
                  ...operation,
                  cause,
                }),
            ),
          );
        return rows[0] ?? null;
      },
    ),
    get: Effect.fn("relay.managed_endpoint_allocations.get")(function* (
      input: ManagedEndpointAllocationKey,
    ) {
      return yield* db
        .select(allocationSelection)
        .from(relayManagedEndpointAllocations)
        .where(whereAllocation(input))
        .limit(1)
        .pipe(
          Effect.map((rows) => rows[0] ?? null),
          Effect.mapError(
            (cause) =>
              new ManagedEndpointAllocationPersistenceError({
                operation: "get",
                stage: "database-request",
                ...input,
                cause,
              }),
          ),
        );
    }),
    reserve: Effect.fn("relay.managed_endpoint_allocations.reserve")(function* (
      input: ReserveManagedEndpointAllocationInput,
    ) {
      const { ownership, ...reservation } = input;
      const now = DateTime.formatIso(yield* DateTime.now);
      const allocation = yield* db
        .transaction((tx) =>
          Effect.gen(function* () {
            yield* tx.execute(
              sql`select pg_advisory_xact_lock(hashtextextended(${`environment:${ownership.environmentId}`}, 0))`,
            );
            const current = yield* tx
              .select({ environmentId: relayEnvironmentOperations.environmentId })
              .from(relayEnvironmentOperations)
              .where(whereCurrentOperation(ownership))
              .limit(1);
            if (current.length === 0) {
              return yield* new ManagedEndpointAllocationPersistenceError({
                operation: "reserve",
                stage: "ownership-lost",
                ...reservation,
              });
            }
            const inserted = yield* tx
              .insert(relayManagedEndpointAllocations)
              .values({
                ...reservation,
                operationGeneration: ownership.generation,
                operationOwnerToken: ownership.ownerToken,
                createdAt: now,
                updatedAt: now,
              })
              .onConflictDoNothing()
              .returning(allocationSelection);
            if (inserted[0] !== undefined) return inserted[0];
            const rows = yield* tx
              .update(relayManagedEndpointAllocations)
              .set({
                operationGeneration: ownership.generation,
                operationOwnerToken: ownership.ownerToken,
                updatedAt: now,
              })
              .where(and(whereAllocation(reservation), currentOperationExists(ownership)))
              .returning(allocationSelection);
            return rows[0];
          }),
        )
        .pipe(
          Effect.mapError((cause) =>
            isManagedEndpointAllocationPersistenceError(cause)
              ? cause
              : new ManagedEndpointAllocationPersistenceError({
                  operation: "reserve",
                  stage: "database-request",
                  ...reservation,
                  cause,
                }),
          ),
        );

      if (allocation === undefined) {
        return yield* new ManagedEndpointAllocationPersistenceError({
          operation: "reserve",
          stage: "resolve-reservation",
          ...reservation,
        });
      }

      return allocation;
    }),
    recordTunnel: Effect.fn("relay.managed_endpoint_allocations.record_tunnel")(function* (
      input: RecordManagedEndpointTunnelInput,
    ) {
      const updated = yield* db
        .update(relayManagedEndpointAllocations)
        .set({
          tunnelId: input.tunnelId,
          updatedAt: DateTime.formatIso(yield* DateTime.now),
        })
        .where(whereCurrentAllocationOwnership(input))
        .returning({ environmentId: relayManagedEndpointAllocations.environmentId })
        .pipe(
          Effect.mapError(
            (cause) =>
              new ManagedEndpointAllocationPersistenceError({
                operation: "record-tunnel",
                stage: "database-request",
                ...input,
                cause,
              }),
          ),
        );
      if (updated.length === 0) {
        return yield* new ManagedEndpointAllocationPersistenceError({
          operation: "record-tunnel",
          stage: "ownership-lost",
          ...input,
        });
      }
    }),
    recordDns: Effect.fn("relay.managed_endpoint_allocations.record_dns")(function* (
      input: RecordManagedEndpointDnsInput,
    ) {
      const updated = yield* db
        .update(relayManagedEndpointAllocations)
        .set({
          dnsRecordId: input.dnsRecordId,
          updatedAt: DateTime.formatIso(yield* DateTime.now),
        })
        .where(whereCurrentAllocationOwnership(input))
        .returning({ environmentId: relayManagedEndpointAllocations.environmentId })
        .pipe(
          Effect.mapError(
            (cause) =>
              new ManagedEndpointAllocationPersistenceError({
                operation: "record-dns",
                stage: "database-request",
                ...input,
                cause,
              }),
          ),
        );
      if (updated.length === 0) {
        return yield* new ManagedEndpointAllocationPersistenceError({
          operation: "record-dns",
          stage: "ownership-lost",
          ...input,
        });
      }
    }),
    markReady: Effect.fn("relay.managed_endpoint_allocations.mark_ready")(function* (
      input: OwnedManagedEndpointAllocationKey,
    ) {
      const now = DateTime.formatIso(yield* DateTime.now);
      const updated = yield* db
        .update(relayManagedEndpointAllocations)
        .set({
          readyAt: now,
          updatedAt: now,
        })
        .where(whereCurrentAllocationOwnership(input))
        .returning({ environmentId: relayManagedEndpointAllocations.environmentId })
        .pipe(
          Effect.mapError(
            (cause) =>
              new ManagedEndpointAllocationPersistenceError({
                operation: "mark-ready",
                stage: "database-request",
                ...input,
                cause,
              }),
          ),
        );
      if (updated.length === 0) {
        return yield* new ManagedEndpointAllocationPersistenceError({
          operation: "mark-ready",
          stage: "ownership-lost",
          ...input,
        });
      }
    }),
    remove: Effect.fn("relay.managed_endpoint_allocations.remove")(function* (
      input: OwnedManagedEndpointAllocationKey,
    ) {
      const removed = yield* db
        .delete(relayManagedEndpointAllocations)
        .where(whereCurrentAllocationOwnership(input))
        .returning({ environmentId: relayManagedEndpointAllocations.environmentId })
        .pipe(
          Effect.mapError(
            (cause) =>
              new ManagedEndpointAllocationPersistenceError({
                operation: "remove",
                stage: "database-request",
                ...input,
                cause,
              }),
          ),
        );
      if (removed.length === 0) {
        return yield* new ManagedEndpointAllocationPersistenceError({
          operation: "remove",
          stage: "ownership-lost",
          ...input,
        });
      }
    }),
  });
  return service;
});

export const layer = Layer.effect(ManagedEndpointAllocations, make);
