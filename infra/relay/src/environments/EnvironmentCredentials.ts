import * as Context from "effect/Context";
import * as Crypto from "effect/Crypto";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Base64Url from "effect/encoding/Base64Url";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import { and, eq, isNotNull, isNull, notExists, sql } from "drizzle-orm";

import * as RelayDb from "../db.ts";
import { relayEnvironmentCredentials, relayEnvironmentLinks } from "../persistence/schema.ts";

export class EnvironmentCredentialCreatePersistenceError extends Schema.TaggedError<EnvironmentCredentialCreatePersistenceError>()(
  "EnvironmentCredentialCreatePersistenceError",
  {
    stage: Schema.Literals([
      "generate-credential",
      "hash-token",
      "insert-credential",
      "revoke-previous-credentials",
      "rollback-credential",
    ]),
    environmentId: Schema.String,
    credentialId: Schema.optionalKey(Schema.String),
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Environment credential creation failed during '${this.stage}' for environment '${this.environmentId}'${this.credentialId === undefined ? "" : `, credential '${this.credentialId}'`}`;
  }
}

const isEnvironmentCredentialCreatePersistenceError = Schema.is(
  EnvironmentCredentialCreatePersistenceError,
);

export class EnvironmentCredentialAuthenticatePersistenceError extends Schema.TaggedError<EnvironmentCredentialAuthenticatePersistenceError>()(
  "EnvironmentCredentialAuthenticatePersistenceError",
  {
    stage: Schema.Literals(["hash-token", "lookup-credential"]),
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Environment credential authentication failed during '${this.stage}'`;
  }
}

export class EnvironmentCredentialRevokePersistenceError extends Schema.TaggedError<EnvironmentCredentialRevokePersistenceError>()(
  "EnvironmentCredentialRevokePersistenceError",
  {
    environmentId: Schema.String,
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Failed to revoke credentials for environment '${this.environmentId}'`;
  }
}

export interface EnvironmentCredentialPrincipal {
  readonly credentialId: string;
  readonly environmentId: string;
  readonly environmentPublicKey: string;
}

export interface EnvironmentCredentialRotation {
  readonly token: string;
  readonly credentialId: string;
  readonly previousCredentialId: string | null;
  readonly environmentId: string;
  readonly environmentPublicKey: string;
}

export class EnvironmentCredentials extends Context.Service<
  EnvironmentCredentials,
  {
    readonly create: (input: {
      readonly environmentId: string;
      readonly environmentPublicKey: string;
    }) => Effect.Effect<string, EnvironmentCredentialCreatePersistenceError>;
    readonly rotate: (input: {
      readonly environmentId: string;
      readonly environmentPublicKey: string;
    }) => Effect.Effect<EnvironmentCredentialRotation, EnvironmentCredentialCreatePersistenceError>;
    readonly rollbackRotation: (
      rotation: EnvironmentCredentialRotation,
    ) => Effect.Effect<boolean, EnvironmentCredentialCreatePersistenceError>;
    readonly authenticate: (
      token: string,
    ) => Effect.Effect<
      Option.Option<EnvironmentCredentialPrincipal>,
      EnvironmentCredentialAuthenticatePersistenceError
    >;
    readonly revokeForEnvironmentPublicKey: (input: {
      readonly environmentId: string;
      readonly environmentPublicKey: string;
    }) => Effect.Effect<boolean, EnvironmentCredentialRevokePersistenceError>;
    readonly revokeOrphanedForEnvironment: (input: {
      readonly environmentId: string;
    }) => Effect.Effect<boolean, EnvironmentCredentialRevokePersistenceError>;
  }
>()("bibcode-relay/environments/EnvironmentCredentials") {}

const make = Effect.gen(function* () {
  const db = yield* RelayDb.RelayDb;
  const crypto = yield* Crypto.Crypto;
  const hashToken = (token: string) =>
    crypto.digest("SHA-256", new TextEncoder().encode(token)).pipe(Effect.map(Base64Url.encode));
  const randomTokenPart = (segments: number) =>
    Effect.map(Effect.all(Array.from({ length: segments }, () => crypto.randomUUIDv4)), (values) =>
      values.join("").replaceAll("-", ""),
    );
  const makeCredential = Effect.fnUntraced(function* () {
    const credentialId = yield* randomTokenPart(2);
    const secret = yield* randomTokenPart(3);
    return {
      credentialId,
      token: `bibcodeenv_${credentialId}_${secret}`,
    };
  });

  const rotate = Effect.fn("relay.environment_credentials.rotate")(function* (input: {
    readonly environmentId: string;
    readonly environmentPublicKey: string;
  }) {
    yield* Effect.annotateCurrentSpan({ "relay.environment_id": input.environmentId });
    const credential = yield* makeCredential().pipe(
      Effect.mapError(
        (cause) =>
          new EnvironmentCredentialCreatePersistenceError({
            stage: "generate-credential",
            environmentId: input.environmentId,
            cause,
          }),
      ),
    );
    const credentialHash = yield* hashToken(credential.token).pipe(
      Effect.mapError(
        (cause) =>
          new EnvironmentCredentialCreatePersistenceError({
            stage: "hash-token",
            environmentId: input.environmentId,
            credentialId: credential.credentialId,
            cause,
          }),
      ),
    );
    const now = DateTime.formatIso(yield* DateTime.now);
    const previousCredentialId = yield* db
      .transaction((tx) =>
        Effect.gen(function* () {
          yield* tx
            .execute(
              sql`select pg_advisory_xact_lock(hashtextextended(${`credential:${input.environmentId}:${input.environmentPublicKey}`}, 0))`,
            )
            .pipe(
              Effect.mapError(
                (cause) =>
                  new EnvironmentCredentialCreatePersistenceError({
                    stage: "revoke-previous-credentials",
                    environmentId: input.environmentId,
                    credentialId: credential.credentialId,
                    cause,
                  }),
              ),
            );
          const revokedPrevious = yield* tx
            .update(relayEnvironmentCredentials)
            .set({
              revokedAt: now,
              updatedAt: now,
            })
            .where(
              and(
                eq(relayEnvironmentCredentials.environmentId, input.environmentId),
                eq(relayEnvironmentCredentials.environmentPublicKey, input.environmentPublicKey),
                isNull(relayEnvironmentCredentials.revokedAt),
              ),
            )
            .returning({ credentialId: relayEnvironmentCredentials.credentialId })
            .pipe(
              Effect.mapError(
                (cause) =>
                  new EnvironmentCredentialCreatePersistenceError({
                    stage: "revoke-previous-credentials",
                    environmentId: input.environmentId,
                    credentialId: credential.credentialId,
                    cause,
                  }),
              ),
            );
          const previousCredentialId = revokedPrevious[0]?.credentialId ?? null;
          yield* tx
            .insert(relayEnvironmentCredentials)
            .values({
              credentialId: credential.credentialId,
              environmentId: input.environmentId,
              environmentPublicKey: input.environmentPublicKey,
              credentialHash,
              revokedAt: null,
              createdAt: now,
              updatedAt: now,
            })
            .pipe(
              Effect.mapError(
                (cause) =>
                  new EnvironmentCredentialCreatePersistenceError({
                    stage: "insert-credential",
                    environmentId: input.environmentId,
                    credentialId: credential.credentialId,
                    cause,
                  }),
              ),
            );
          return previousCredentialId;
        }),
      )
      .pipe(
        Effect.mapError((cause) =>
          isEnvironmentCredentialCreatePersistenceError(cause)
            ? cause
            : new EnvironmentCredentialCreatePersistenceError({
                stage: "insert-credential",
                environmentId: input.environmentId,
                credentialId: credential.credentialId,
                cause,
              }),
        ),
      );
    return {
      ...input,
      ...credential,
      previousCredentialId,
    } satisfies EnvironmentCredentialRotation;
  });

  return EnvironmentCredentials.of({
    create: (input) => rotate(input).pipe(Effect.map((rotation) => rotation.token)),
    rotate,
    rollbackRotation: Effect.fn("relay.environment_credentials.rollback_rotation")(
      function* (rotation) {
        const now = DateTime.formatIso(yield* DateTime.now);
        return yield* db
          .transaction((tx) =>
            Effect.gen(function* () {
              yield* tx.execute(
                sql`select pg_advisory_xact_lock(hashtextextended(${`credential:${rotation.environmentId}:${rotation.environmentPublicKey}`}, 0))`,
              );
              const revokedReplacement = yield* tx
                .update(relayEnvironmentCredentials)
                .set({ revokedAt: now, updatedAt: now })
                .where(
                  and(
                    eq(relayEnvironmentCredentials.credentialId, rotation.credentialId),
                    eq(relayEnvironmentCredentials.environmentId, rotation.environmentId),
                    eq(
                      relayEnvironmentCredentials.environmentPublicKey,
                      rotation.environmentPublicKey,
                    ),
                    isNull(relayEnvironmentCredentials.revokedAt),
                  ),
                )
                .returning({ credentialId: relayEnvironmentCredentials.credentialId });
              if (revokedReplacement.length === 0) return false;
              if (rotation.previousCredentialId !== null) {
                const restoredPrevious = yield* tx
                  .update(relayEnvironmentCredentials)
                  .set({ revokedAt: null, updatedAt: now })
                  .where(
                    and(
                      eq(relayEnvironmentCredentials.credentialId, rotation.previousCredentialId),
                      eq(relayEnvironmentCredentials.environmentId, rotation.environmentId),
                      eq(
                        relayEnvironmentCredentials.environmentPublicKey,
                        rotation.environmentPublicKey,
                      ),
                      isNotNull(relayEnvironmentCredentials.revokedAt),
                    ),
                  )
                  .returning({ credentialId: relayEnvironmentCredentials.credentialId });
                if (restoredPrevious.length === 0) {
                  return yield* new EnvironmentCredentialCreatePersistenceError({
                    stage: "rollback-credential",
                    environmentId: rotation.environmentId,
                    credentialId: rotation.credentialId,
                    cause: new Error("The exact prior credential could not be restored"),
                  });
                }
              }
              return true;
            }),
          )
          .pipe(
            Effect.mapError((cause) =>
              isEnvironmentCredentialCreatePersistenceError(cause)
                ? cause
                : new EnvironmentCredentialCreatePersistenceError({
                    stage: "rollback-credential",
                    environmentId: rotation.environmentId,
                    credentialId: rotation.credentialId,
                    cause,
                  }),
            ),
          );
      },
    ),

    authenticate: Effect.fn("relay.environment_credentials.authenticate")(function* (token) {
      const credentialHash = yield* hashToken(token).pipe(
        Effect.mapError(
          (cause) =>
            new EnvironmentCredentialAuthenticatePersistenceError({
              stage: "hash-token",
              cause,
            }),
        ),
      );
      const rows = yield* db
        .select({
          credentialId: relayEnvironmentCredentials.credentialId,
          environmentId: relayEnvironmentCredentials.environmentId,
          environmentPublicKey: relayEnvironmentCredentials.environmentPublicKey,
        })
        .from(relayEnvironmentCredentials)
        .where(
          and(
            eq(relayEnvironmentCredentials.credentialHash, credentialHash),
            isNull(relayEnvironmentCredentials.revokedAt),
          ),
        )
        .limit(1)
        .pipe(
          Effect.mapError(
            (cause) =>
              new EnvironmentCredentialAuthenticatePersistenceError({
                stage: "lookup-credential",
                cause,
              }),
          ),
        );
      const row = rows[0];
      if (row) {
        yield* Effect.annotateCurrentSpan({ "relay.environment_id": row.environmentId });
      }
      return row
        ? Option.some({
            credentialId: row.credentialId,
            environmentId: row.environmentId,
            environmentPublicKey: row.environmentPublicKey,
          })
        : Option.none();
    }),

    revokeForEnvironmentPublicKey: Effect.fn(
      "relay.environment_credentials.revoke_for_environment_public_key",
    )(function* (input) {
      yield* Effect.annotateCurrentSpan({ "relay.environment_id": input.environmentId });
      const revokedAt = DateTime.formatIso(yield* DateTime.now);
      const rows = yield* db
        .update(relayEnvironmentCredentials)
        .set({
          revokedAt,
          updatedAt: revokedAt,
        })
        .where(
          and(
            eq(relayEnvironmentCredentials.environmentId, input.environmentId),
            eq(relayEnvironmentCredentials.environmentPublicKey, input.environmentPublicKey),
            isNull(relayEnvironmentCredentials.revokedAt),
            notExists(
              db
                .select({ userId: relayEnvironmentLinks.userId })
                .from(relayEnvironmentLinks)
                .where(
                  and(
                    eq(relayEnvironmentLinks.environmentId, input.environmentId),
                    eq(relayEnvironmentLinks.environmentPublicKey, input.environmentPublicKey),
                    isNull(relayEnvironmentLinks.revokedAt),
                  ),
                ),
            ),
          ),
        )
        .returning({
          credentialId: relayEnvironmentCredentials.credentialId,
        })
        .pipe(
          Effect.mapError(
            (cause) =>
              new EnvironmentCredentialRevokePersistenceError({
                environmentId: input.environmentId,
                cause,
              }),
          ),
        );
      return rows.length > 0;
    }),

    revokeOrphanedForEnvironment: Effect.fn(
      "relay.environment_credentials.revoke_orphaned_for_environment",
    )(function* (input) {
      yield* Effect.annotateCurrentSpan({ "relay.environment_id": input.environmentId });
      const revokedAt = DateTime.formatIso(yield* DateTime.now);
      const rows = yield* db
        .update(relayEnvironmentCredentials)
        .set({
          revokedAt,
          updatedAt: revokedAt,
        })
        .where(
          and(
            eq(relayEnvironmentCredentials.environmentId, input.environmentId),
            isNull(relayEnvironmentCredentials.revokedAt),
            notExists(
              db
                .select({ userId: relayEnvironmentLinks.userId })
                .from(relayEnvironmentLinks)
                .where(
                  and(
                    eq(relayEnvironmentLinks.environmentId, input.environmentId),
                    eq(
                      relayEnvironmentLinks.environmentPublicKey,
                      relayEnvironmentCredentials.environmentPublicKey,
                    ),
                    isNull(relayEnvironmentLinks.revokedAt),
                  ),
                ),
            ),
          ),
        )
        .returning({ credentialId: relayEnvironmentCredentials.credentialId })
        .pipe(
          Effect.mapError(
            (cause) =>
              new EnvironmentCredentialRevokePersistenceError({
                environmentId: input.environmentId,
                cause,
              }),
          ),
        );
      return rows.length > 0;
    }),
  });
});

export const layer = Layer.effect(EnvironmentCredentials, make);
