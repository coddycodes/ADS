import * as NodeCryptoLayer from "@effect/platform-node/NodeCrypto";
import { describe, expect, it } from "@effect/vitest";
import { PgDialect, QueryBuilder } from "drizzle-orm/pg-core";
import * as Crypto from "effect/Crypto";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as PlatformError from "effect/PlatformError";
import * as Semaphore from "effect/Semaphore";

import * as RelayDb from "../db.ts";
import { relayEnvironmentCredentials } from "../persistence/schema.ts";
import * as EnvironmentCredentials from "./EnvironmentCredentials.ts";

function withPassthroughTransaction<T extends object>(service: T): RelayDb.RelayDb["Service"] {
  const transactionService = {
    execute: () => Effect.void,
    select: () => ({
      from: () => ({
        where: () => ({ limit: () => Effect.succeed([]) }),
      }),
    }),
    update: () => ({
      set: () => ({
        where: () => ({ returning: () => Effect.succeed([]) }),
      }),
    }),
    ...service,
  } as unknown as RelayDb.RelayDb["Service"];
  return {
    ...service,
    transaction: <A, E, R>(
      work: (tx: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>,
    ): Effect.Effect<A, E, R> => work(transactionService),
  } as unknown as RelayDb.RelayDb["Service"];
}

describe("EnvironmentCredentials", () => {
  it("formats structured errors without retaining credential material", () => {
    const cause = new Error("secret-token-must-not-appear");
    const withoutCredential =
      new EnvironmentCredentials.EnvironmentCredentialCreatePersistenceError({
        stage: "generate-credential",
        environmentId: "env-1",
        cause,
      });
    const withCredential = new EnvironmentCredentials.EnvironmentCredentialCreatePersistenceError({
      stage: "hash-token",
      environmentId: "env-1",
      credentialId: "credential-1",
      cause,
    });
    const authenticate =
      new EnvironmentCredentials.EnvironmentCredentialAuthenticatePersistenceError({
        stage: "hash-token",
        cause,
      });
    const revoke = new EnvironmentCredentials.EnvironmentCredentialRevokePersistenceError({
      environmentId: "env-1",
      cause,
    });

    expect(withoutCredential.message).toBe(
      "Environment credential creation failed during 'generate-credential' for environment 'env-1'",
    );
    expect(withCredential.message).toBe(
      "Environment credential creation failed during 'hash-token' for environment 'env-1', credential 'credential-1'",
    );
    expect(authenticate.message).toBe(
      "Environment credential authentication failed during 'hash-token'",
    );
    expect(revoke.message).toBe("Failed to revoke credentials for environment 'env-1'");
    for (const error of [withoutCredential, withCredential, authenticate, revoke]) {
      expect(JSON.stringify(error)).not.toContain("secret-token-must-not-appear");
    }
  });

  it.effect("classifies credential generation, hashing, and insertion failures", () => {
    const generationCause = PlatformError.badArgument({
      module: "Crypto",
      method: "randomUUIDv4",
      description: "uuid unavailable",
    });
    const hashCause = PlatformError.badArgument({
      module: "Crypto",
      method: "digest",
      description: "digest unavailable",
    });
    const insertCause = new Error("insert unavailable");
    const insertDb = withPassthroughTransaction({
      insert: () => ({ values: () => Effect.fail(insertCause) }),
    });
    const noDb = {} as RelayDb.RelayDb["Service"];

    const create = (crypto: Crypto.Crypto, db: RelayDb.RelayDb["Service"]) =>
      Effect.gen(function* () {
        const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
        return yield* Effect.flip(
          credentials.create({
            environmentId: "env-1",
            environmentPublicKey: "public-key",
          }),
        );
      }).pipe(
        Effect.provide(
          EnvironmentCredentials.layer.pipe(
            Layer.provide(Layer.succeed(Crypto.Crypto, crypto)),
            Layer.provide(Layer.succeed(RelayDb.RelayDb, db)),
          ),
        ),
      );

    return Effect.gen(function* () {
      const nodeCrypto = yield* Crypto.Crypto;
      const deterministicCrypto = Crypto.Crypto.of({
        ...nodeCrypto,
        randomUUIDv4: Effect.succeed("00000000-0000-4000-8000-000000000000"),
        digest: () => Effect.fail(hashCause),
      });
      const generationCrypto = Crypto.Crypto.of({
        ...nodeCrypto,
        randomUUIDv4: Effect.fail(generationCause),
        digest: () => Effect.die("digest must not run"),
      });
      const generation = yield* create(generationCrypto, noDb);
      expect(generation).toMatchObject({
        stage: "generate-credential",
        environmentId: "env-1",
        cause: generationCause,
      });

      const hashing = yield* create(deterministicCrypto, noDb);
      expect(hashing).toMatchObject({
        stage: "hash-token",
        environmentId: "env-1",
        cause: hashCause,
      });

      const insertion = yield* create(
        {
          ...deterministicCrypto,
          digest: () => Effect.succeed(new Uint8Array([1, 2, 3])),
        },
        insertDb,
      );
      expect(insertion).toMatchObject({
        stage: "insert-credential",
        environmentId: "env-1",
        cause: insertCause,
      });
    }).pipe(Effect.provide(NodeCryptoLayer.layer));
  });

  it.effect("reports the credential creation persistence stage and preserves its cause", () => {
    const cause = new Error("database unavailable");
    const fakeDb = withPassthroughTransaction({
      insert: (table: unknown) => {
        expect(table).toBe(relayEnvironmentCredentials);
        return {
          values: () => Effect.void,
        };
      },
      update: (table: unknown) => {
        expect(table).toBe(relayEnvironmentCredentials);
        return {
          set: () => ({
            where: () => ({ returning: () => Effect.fail(cause) }),
          }),
        };
      },
    });

    return Effect.gen(function* () {
      const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
      const error = yield* Effect.flip(
        credentials.create({
          environmentId: "env_test",
          environmentPublicKey: "sensitive-public-key-material",
        }),
      );

      expect(error).toMatchObject({
        _tag: "EnvironmentCredentialCreatePersistenceError",
        stage: "revoke-previous-credentials",
        environmentId: "env_test",
      });
      expect(error.credentialId).toMatch(/^[0-9a-f]{64}$/);
      expect(error.cause).toBe(cause);
      expect(error).not.toHaveProperty("environmentPublicKey");
    }).pipe(
      Effect.provide(
        EnvironmentCredentials.layer.pipe(
          Layer.provide(NodeCryptoLayer.layer),
          Layer.provide(Layer.succeed(RelayDb.RelayDb, fakeDb)),
        ),
      ),
    );
  });

  it.effect("does not retain credential tokens when lookup persistence fails", () => {
    const cause = new Error("database unavailable");
    const token = "bibcodeenv_sensitive-credential-token";
    const fakeDb = {
      select: () => ({
        from: (table: unknown) => {
          expect(table).toBe(relayEnvironmentCredentials);
          return {
            where: () => ({
              limit: () => Effect.fail(cause),
            }),
          };
        },
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
      const error = yield* Effect.flip(credentials.authenticate(token));

      expect(error).toMatchObject({
        _tag: "EnvironmentCredentialAuthenticatePersistenceError",
        stage: "lookup-credential",
      });
      expect(error.cause).toBe(cause);
      expect(error).not.toHaveProperty("token");
    }).pipe(
      Effect.provide(
        EnvironmentCredentials.layer.pipe(
          Layer.provide(NodeCryptoLayer.layer),
          Layer.provide(Layer.succeed(RelayDb.RelayDb, fakeDb)),
        ),
      ),
    );
  });

  it.effect("authenticates active credentials and returns none for an unknown token", () => {
    const rows = [
      {
        credentialId: "credential-1",
        environmentId: "env-1",
        environmentPublicKey: "public-key-1",
      },
    ];
    const fakeDb = {
      select: () => ({
        from: () => ({
          where: () => ({
            limit: () => Effect.sync(() => rows.splice(0)),
          }),
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
      const authenticated = yield* credentials.authenticate("token-1");
      const missing = yield* credentials.authenticate("token-2");

      expect(Option.getOrNull(authenticated)).toEqual({
        credentialId: "credential-1",
        environmentId: "env-1",
        environmentPublicKey: "public-key-1",
      });
      expect(Option.isNone(missing)).toBe(true);
    }).pipe(
      Effect.provide(
        EnvironmentCredentials.layer.pipe(
          Layer.provide(NodeCryptoLayer.layer),
          Layer.provide(Layer.succeed(RelayDb.RelayDb, fakeDb)),
        ),
      ),
    );
  });

  it.effect("classifies authentication hashing failures without retaining the token", () => {
    const cause = PlatformError.badArgument({
      module: "Crypto",
      method: "digest",
      description: "digest unavailable",
    });
    const token = "bibcodeenv_sensitive-token";

    return Effect.gen(function* () {
      const nodeCrypto = yield* Crypto.Crypto;
      const crypto = Crypto.Crypto.of({ ...nodeCrypto, digest: () => Effect.fail(cause) });
      const error = yield* Effect.gen(function* () {
        const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
        return yield* Effect.flip(credentials.authenticate(token));
      }).pipe(
        Effect.provide(
          EnvironmentCredentials.layer.pipe(
            Layer.provide(Layer.succeed(Crypto.Crypto, crypto)),
            Layer.provide(Layer.succeed(RelayDb.RelayDb, {} as RelayDb.RelayDb["Service"])),
          ),
        ),
      );
      expect(error).toMatchObject({ stage: "hash-token", cause });
      expect(error).not.toHaveProperty("token");
      expect(error.message).not.toContain(token);
    }).pipe(Effect.provide(NodeCryptoLayer.layer));
  });

  it.effect("rolls back the inserted credential when rotation revocation fails", () => {
    const cause = new Error("revoke unavailable");
    const persistedCredentialIds: Array<string> = [];
    let stagedCredentialId: string | undefined;
    const insert = (commitImmediately: boolean) => () => ({
      values: (values: { readonly credentialId: string }) =>
        Effect.sync(() => {
          if (commitImmediately) {
            persistedCredentialIds.push(values.credentialId);
          } else {
            stagedCredentialId = values.credentialId;
          }
        }),
    });
    const update = () => ({
      set: () => ({ where: () => ({ returning: () => Effect.fail(cause) }) }),
    });
    const transactionDb = {
      execute: () => Effect.void,
      insert: insert(false),
      update,
    } as unknown as RelayDb.RelayDb["Service"];
    const fakeDb = {
      insert: insert(true),
      update,
      transaction: <A, E, R>(
        work: (tx: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>,
      ): Effect.Effect<A, E, R> =>
        work(transactionDb).pipe(
          Effect.tap(() =>
            Effect.sync(() => {
              if (stagedCredentialId !== undefined) {
                persistedCredentialIds.push(stagedCredentialId);
              }
            }),
          ),
        ),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
      const error = yield* Effect.flip(
        credentials.create({
          environmentId: "env-rollback",
          environmentPublicKey: "environment-public-key",
        }),
      );
      expect(error).toMatchObject({ stage: "revoke-previous-credentials", cause });
      expect(persistedCredentialIds).toEqual([]);
    }).pipe(
      Effect.provide(
        EnvironmentCredentials.layer.pipe(
          Layer.provide(NodeCryptoLayer.layer),
          Layer.provide(Layer.succeed(RelayDb.RelayDb, fakeDb)),
        ),
      ),
    );
  });

  it.effect("maps credential transaction boundary failures to the insert stage", () => {
    const cause = new Error("transaction unavailable");
    const fakeDb = {
      insert: () => ({ values: () => Effect.void }),
      update: () => ({ set: () => ({ where: () => Effect.void }) }),
      transaction: () => Effect.fail(cause),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
      const error = yield* Effect.flip(
        credentials.create({
          environmentId: "env-transaction",
          environmentPublicKey: "environment-public-key",
        }),
      );
      expect(error).toMatchObject({
        stage: "insert-credential",
        environmentId: "env-transaction",
        cause,
      });
    }).pipe(
      Effect.provide(
        EnvironmentCredentials.layer.pipe(
          Layer.provide(NodeCryptoLayer.layer),
          Layer.provide(Layer.succeed(RelayDb.RelayDb, fakeDb)),
        ),
      ),
    );
  });

  it.effect("locks and revokes the active credential before inserting its replacement", () => {
    const events: Array<string> = [];
    let hasActiveCredential = true;
    const uniqueViolation = { code: "23505", constraint: "active_environment_key" } as const;
    const transactionDb = {
      execute: () => Effect.sync(() => events.push("lock")),
      update: () => ({
        set: () => ({
          where: () => ({
            returning: () =>
              Effect.sync(() => {
                events.push("revoke");
                if (!hasActiveCredential) return [];
                hasActiveCredential = false;
                return [{ credentialId: "prior" }];
              }),
          }),
        }),
      }),
      insert: () => ({
        values: () =>
          Effect.suspend(() => {
            events.push("insert");
            if (hasActiveCredential) {
              return Effect.fail(uniqueViolation);
            }
            hasActiveCredential = true;
            return Effect.void;
          }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];
    const fakeDb = {
      transaction: <A, E, R>(work: (tx: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>) =>
        work(transactionDb),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
      const token = yield* credentials.create({
        environmentId: "env-serialized",
        environmentPublicKey: "environment-public-key",
      });

      expect(token).toMatch(/^bibcodeenv_/u);
      expect(events).toEqual(["lock", "revoke", "insert"]);
      expect(hasActiveCredential).toBe(true);
    }).pipe(
      Effect.provide(
        EnvironmentCredentials.layer.pipe(
          Layer.provide(NodeCryptoLayer.layer),
          Layer.provide(Layer.succeed(RelayDb.RelayDb, fakeDb)),
        ),
      ),
    );
  });

  it.effect("maps advisory-lock and orphan-cleanup persistence failures", () => {
    const lockCause = new Error("lock unavailable");
    const orphanCause = new Error("cleanup unavailable");
    const createDb = {
      transaction: <A, E, R>(work: (tx: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>) =>
        work({ execute: () => Effect.fail(lockCause) } as unknown as RelayDb.RelayDb["Service"]),
    } as unknown as RelayDb.RelayDb["Service"];
    const revokeDb = {
      select: (fields: Parameters<QueryBuilder["select"]>[0]) => new QueryBuilder().select(fields),
      update: () => ({
        set: () => ({ where: () => ({ returning: () => Effect.fail(orphanCause) }) }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];
    const run = (db: RelayDb.RelayDb["Service"]) =>
      EnvironmentCredentials.layer.pipe(
        Layer.provide(NodeCryptoLayer.layer),
        Layer.provide(Layer.succeed(RelayDb.RelayDb, db)),
      );

    return Effect.gen(function* () {
      const lock = yield* Effect.gen(function* () {
        const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
        return yield* Effect.flip(
          credentials.create({
            environmentId: "env-failure",
            environmentPublicKey: "public-key",
          }),
        );
      }).pipe(Effect.provide(run(createDb)));
      const orphan = yield* Effect.gen(function* () {
        const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
        return yield* Effect.flip(
          credentials.revokeOrphanedForEnvironment({ environmentId: "env-failure" }),
        );
      }).pipe(Effect.provide(run(revokeDb)));

      expect(lock).toMatchObject({ stage: "revoke-previous-credentials", cause: lockCause });
      expect(orphan).toMatchObject({ environmentId: "env-failure", cause: orphanCause });
    });
  });

  it.effect("keeps exactly one active credential across concurrent rotations", () =>
    Effect.forEach(Array.from({ length: 8 }), () =>
      Effect.gen(function* () {
        const transactionLock = yield* Semaphore.make(1);
        const rows: Array<{
          credentialId: string;
          environmentId: string;
          environmentPublicKey: string;
          credentialHash: string;
          revokedAt: string | null;
        }> = [];
        const tx = {
          execute: () => Effect.void,
          update: () => ({
            set: (values: { readonly revokedAt: string }) => ({
              where: () => ({
                returning: () =>
                  Effect.sync(() => {
                    const revoked: Array<{ readonly credentialId: string }> = [];
                    for (const row of rows) {
                      if (row.revokedAt === null) {
                        row.revokedAt = values.revokedAt;
                        revoked.push({ credentialId: row.credentialId });
                      }
                    }
                    return revoked;
                  }),
              }),
            }),
          }),
          insert: () => ({
            values: (values: (typeof rows)[number]) =>
              Effect.suspend(() => {
                if (rows.some((row) => row.revokedAt === null)) {
                  return Effect.fail({ code: "23505", constraint: "active_environment_key" });
                }
                rows.push({ ...values });
                return Effect.void;
              }),
          }),
        } as unknown as RelayDb.RelayDb["Service"];
        const db = {
          transaction: <A, E, R>(
            work: (transaction: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>,
          ) => Semaphore.withPermits(transactionLock, 1, work(tx)),
          select: () => ({
            from: () => ({
              where: (condition: unknown) => ({
                limit: () => {
                  const query = new PgDialect().sqlToQuery(condition as never);
                  const credentialHash = query.params[0];
                  return Effect.succeed(
                    rows
                      .filter(
                        (row) => row.revokedAt === null && row.credentialHash === credentialHash,
                      )
                      .map(
                        ({ credentialHash: _credentialHash, revokedAt: _revokedAt, ...row }) => row,
                      ),
                  );
                },
              }),
            }),
          }),
        } as unknown as RelayDb.RelayDb["Service"];
        const layer = EnvironmentCredentials.layer.pipe(
          Layer.provide(NodeCryptoLayer.layer),
          Layer.provide(Layer.succeed(RelayDb.RelayDb, db)),
        );
        const program = Effect.gen(function* () {
          const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
          const oldToken = yield* credentials.create({
            environmentId: "env-race",
            environmentPublicKey: "public-key",
          });
          const replacements = yield* Effect.all(
            [
              credentials.create({
                environmentId: "env-race",
                environmentPublicKey: "public-key",
              }),
              credentials.create({
                environmentId: "env-race",
                environmentPublicKey: "public-key",
              }),
            ],
            { concurrency: "unbounded" },
          );
          const oldPrincipal = yield* credentials.authenticate(oldToken);
          const replacementPrincipals = yield* Effect.forEach(replacements, (token) =>
            credentials.authenticate(token),
          );

          expect(Option.isNone(oldPrincipal)).toBe(true);
          expect(replacementPrincipals.filter(Option.isSome)).toHaveLength(1);
          expect(rows.filter((row) => row.revokedAt === null)).toHaveLength(1);
        }).pipe(Effect.provide(layer));
        yield* program;
      }),
    ),
  );

  it.effect("restores the exact predecessor despite historical timestamp ties", () => {
    let activeCredentialId: string | null = "replacement";
    const events: Array<string> = [];
    const transactionDb = {
      execute: () => Effect.sync(() => events.push("lock")),
      update: () => ({
        set: (values: { readonly revokedAt: string | null }) => ({
          where: (condition: unknown) =>
            values.revokedAt === null
              ? {
                  returning: () =>
                    Effect.sync(() => {
                      const query = new PgDialect().sqlToQuery(condition as never);
                      const restoredCredentialId = String(query.params[0]);
                      events.push(`restore-${restoredCredentialId}`);
                      activeCredentialId = restoredCredentialId;
                      return [{ credentialId: restoredCredentialId }];
                    }),
                }
              : {
                  returning: () =>
                    Effect.sync(() => {
                      events.push("revoke-replacement");
                      if (activeCredentialId !== "replacement") return [];
                      activeCredentialId = null;
                      return [{ credentialId: "replacement" }];
                    }),
                },
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];
    const db = {
      transaction: <A, E, R>(work: (tx: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>) =>
        work(transactionDb),
    } as unknown as RelayDb.RelayDb["Service"];
    const rotation: EnvironmentCredentials.EnvironmentCredentialRotation = {
      token: "bibcodeenv_undisclosed_replacement",
      credentialId: "replacement",
      previousCredentialId: "prior",
      environmentId: "env-1",
      environmentPublicKey: "public-key",
    };

    return Effect.gen(function* () {
      const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
      expect(yield* credentials.rollbackRotation(rotation)).toBe(true);
      expect(activeCredentialId).toBe("prior");
      expect(events).toEqual(["lock", "revoke-replacement", "restore-prior"]);

      activeCredentialId = "newer";
      events.length = 0;
      expect(yield* credentials.rollbackRotation(rotation)).toBe(false);
      expect(activeCredentialId).toBe("newer");
      expect(events).toEqual(["lock", "revoke-replacement"]);
    }).pipe(
      Effect.provide(
        EnvironmentCredentials.layer.pipe(
          Layer.provide(NodeCryptoLayer.layer),
          Layer.provide(Layer.succeed(RelayDb.RelayDb, db)),
        ),
      ),
    );
  });

  it.effect(
    "revokes a replacement without restoring history when rotation had no predecessor",
    () => {
      let activeCredentialId: string | null = "replacement";
      let restoreAttempts = 0;
      const transactionDb = {
        execute: () => Effect.void,
        update: () => ({
          set: (values: { readonly revokedAt: string | null }) => ({
            where: () =>
              values.revokedAt === null
                ? Effect.sync(() => {
                    restoreAttempts++;
                    activeCredentialId = "historical";
                  })
                : {
                    returning: () =>
                      Effect.sync(() => {
                        if (activeCredentialId !== "replacement") return [];
                        activeCredentialId = null;
                        return [{ credentialId: "replacement" }];
                      }),
                  },
          }),
        }),
        select: () => ({
          from: () => ({
            where: () => ({
              orderBy: () => ({ limit: () => Effect.succeed([{ credentialId: "historical" }]) }),
            }),
          }),
        }),
      } as unknown as RelayDb.RelayDb["Service"];
      const db = {
        transaction: <A, E, R>(work: (tx: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>) =>
          work(transactionDb),
      } as unknown as RelayDb.RelayDb["Service"];
      const rotation = {
        token: "bibcodeenv_undisclosed_replacement",
        credentialId: "replacement",
        previousCredentialId: null,
        environmentId: "env-1",
        environmentPublicKey: "public-key",
      } as EnvironmentCredentials.EnvironmentCredentialRotation & {
        readonly previousCredentialId: null;
      };

      return Effect.gen(function* () {
        const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
        expect(yield* credentials.rollbackRotation(rotation)).toBe(true);
        expect(activeCredentialId).toBeNull();
        expect(restoreAttempts).toBe(0);
      }).pipe(
        Effect.provide(
          EnvironmentCredentials.layer.pipe(
            Layer.provide(NodeCryptoLayer.layer),
            Layer.provide(Layer.succeed(RelayDb.RelayDb, db)),
          ),
        ),
      );
    },
  );

  it.effect("maps credential rollback transaction failures", () => {
    const cause = new Error("rollback transaction unavailable");
    const db = {
      transaction: () => Effect.fail(cause),
    } as unknown as RelayDb.RelayDb["Service"];
    const rotation: EnvironmentCredentials.EnvironmentCredentialRotation = {
      token: "bibcodeenv_undisclosed_replacement",
      credentialId: "replacement",
      previousCredentialId: "prior",
      environmentId: "env-1",
      environmentPublicKey: "public-key",
    };

    return Effect.gen(function* () {
      const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
      const error = yield* Effect.flip(credentials.rollbackRotation(rotation));
      expect(error).toMatchObject({
        stage: "rollback-credential",
        environmentId: "env-1",
        credentialId: "replacement",
        cause,
      });
    }).pipe(
      Effect.provide(
        EnvironmentCredentials.layer.pipe(
          Layer.provide(NodeCryptoLayer.layer),
          Layer.provide(Layer.succeed(RelayDb.RelayDb, db)),
        ),
      ),
    );
  });

  it.effect("fails rollback when the exact predecessor cannot be restored", () => {
    let updateAttempt = 0;
    const transactionDb = {
      execute: () => Effect.void,
      update: () => ({
        set: () => ({
          where: () => ({
            returning: () =>
              Effect.sync(() => {
                updateAttempt++;
                return updateAttempt === 1 ? [{ credentialId: "replacement" }] : [];
              }),
          }),
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];
    const db = {
      transaction: <A, E, R>(work: (tx: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>) =>
        work(transactionDb),
    } as unknown as RelayDb.RelayDb["Service"];
    const rotation: EnvironmentCredentials.EnvironmentCredentialRotation = {
      token: "bibcodeenv_undisclosed_replacement",
      credentialId: "replacement",
      previousCredentialId: "missing-prior",
      environmentId: "env-1",
      environmentPublicKey: "public-key",
    };

    return Effect.gen(function* () {
      const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
      const error = yield* Effect.flip(credentials.rollbackRotation(rotation));
      expect(error).toMatchObject({
        stage: "rollback-credential",
        environmentId: "env-1",
        credentialId: "replacement",
      });
      expect(error.message).not.toContain(rotation.token);
      expect(updateAttempt).toBe(2);
    }).pipe(
      Effect.provide(
        EnvironmentCredentials.layer.pipe(
          Layer.provide(NodeCryptoLayer.layer),
          Layer.provide(Layer.succeed(RelayDb.RelayDb, db)),
        ),
      ),
    );
  });

  it.effect(
    "carries the exact active predecessor despite historical credentials with equal timestamps",
    () => {
      const equalTimestamp = "2026-07-14T12:00:00.000Z";
      const rows: Array<{
        credentialId: string;
        revokedAt: string | null;
        updatedAt: string;
      }> = [
        { credentialId: "historical-a", revokedAt: equalTimestamp, updatedAt: equalTimestamp },
        { credentialId: "historical-b", revokedAt: equalTimestamp, updatedAt: equalTimestamp },
        { credentialId: "prior-active", revokedAt: null, updatedAt: equalTimestamp },
      ];
      const events: Array<string> = [];
      const transactionDb = {
        execute: () => Effect.sync(() => events.push("lock")),
        update: () => ({
          set: (values: { readonly revokedAt: string }) => ({
            where: () => ({
              returning: () =>
                Effect.sync(() => {
                  events.push("revoke-active");
                  const active = rows.find((row) => row.revokedAt === null);
                  if (active === undefined) return [];
                  active.revokedAt = values.revokedAt;
                  return [{ credentialId: active.credentialId }];
                }),
            }),
          }),
        }),
        insert: () => ({
          values: (values: { readonly credentialId: string }) =>
            Effect.sync(() => {
              events.push("insert-replacement");
              rows.push({
                credentialId: values.credentialId,
                revokedAt: null,
                updatedAt: equalTimestamp,
              });
            }),
        }),
      } as unknown as RelayDb.RelayDb["Service"];
      const db = {
        transaction: <A, E, R>(work: (tx: RelayDb.RelayDb["Service"]) => Effect.Effect<A, E, R>) =>
          work(transactionDb),
      } as unknown as RelayDb.RelayDb["Service"];

      return Effect.gen(function* () {
        const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
        const rotation = yield* credentials.rotate({
          environmentId: "env_test",
          environmentPublicKey: "environment-public-key",
        });

        expect(rotation).toMatchObject({ previousCredentialId: "prior-active" });
        expect(events).toEqual(["lock", "revoke-active", "insert-replacement"]);
        expect(rows.filter((row) => row.revokedAt === null)).toHaveLength(1);
        expect(rows.slice(0, 2).map((row) => row.credentialId)).toEqual([
          "historical-a",
          "historical-b",
        ]);
      }).pipe(
        Effect.provide(
          EnvironmentCredentials.layer.pipe(
            Layer.provide(NodeCryptoLayer.layer),
            Layer.provide(Layer.succeed(RelayDb.RelayDb, db)),
          ),
        ),
      );
    },
  );

  it.effect(
    "creates opaque credentials and revokes only older credentials for the same key",
    () => {
      const insertedValues: Array<{
        readonly credentialId: string;
        readonly environmentId: string;
        readonly environmentPublicKey: string;
        readonly credentialHash: string;
        readonly revokedAt: null;
        readonly createdAt: string;
        readonly updatedAt: string;
      }> = [];
      const staleCredentialRevocations: Array<{
        readonly values: Record<string, unknown>;
        readonly condition: unknown;
      }> = [];

      const fakeDb = withPassthroughTransaction({
        insert: (table: unknown) => {
          expect(table).toBe(relayEnvironmentCredentials);
          return {
            values: (values: (typeof insertedValues)[number]) => {
              insertedValues.push(values);
              return Effect.void;
            },
          };
        },
        update: (table: unknown) => {
          expect(table).toBe(relayEnvironmentCredentials);
          return {
            set: (values: Record<string, unknown>) => ({
              where: (condition: unknown) => {
                staleCredentialRevocations.push({ values, condition });
                return { returning: () => Effect.succeed([]) };
              },
            }),
          };
        },
      });

      return Effect.gen(function* () {
        const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
        const rotation = yield* credentials.rotate({
          environmentId: "env_test",
          environmentPublicKey: "environment-public-key",
        });
        const token = rotation.token;
        const [, credentialId, secret] = token.split("_");

        expect(token).toMatch(/^bibcodeenv_[0-9a-f]{64}_[0-9a-f]{96}$/);
        expect(credentialId).toHaveLength(64);
        expect(secret).toHaveLength(96);
        expect(insertedValues).toHaveLength(1);
        expect(insertedValues[0]).toMatchObject({
          credentialId,
          environmentId: "env_test",
          environmentPublicKey: "environment-public-key",
          revokedAt: null,
        });
        expect(insertedValues[0]?.credentialHash).toMatch(/^[A-Za-z0-9_-]{43}$/);
        expect(insertedValues[0]?.credentialHash).not.toContain(token);
        expect(insertedValues[0]?.createdAt).toBe(insertedValues[0]?.updatedAt);
        expect(staleCredentialRevocations).toHaveLength(1);
        expect(rotation).toMatchObject({ previousCredentialId: null });
        expect(staleCredentialRevocations[0]?.values.revokedAt).toEqual(
          staleCredentialRevocations[0]?.values.updatedAt,
        );

        const query = new PgDialect().sqlToQuery(staleCredentialRevocations[0]?.condition as never);
        expect(query.sql).toContain('"relay_environment_credentials"."environment_id" = $1');
        expect(query.sql).toContain(
          '"relay_environment_credentials"."environment_public_key" = $2',
        );
        expect(query.sql).toContain('"relay_environment_credentials"."revoked_at" is null');
        expect(query.params).toEqual(["env_test", "environment-public-key"]);
      }).pipe(
        Effect.provide(
          EnvironmentCredentials.layer.pipe(
            Layer.provide(NodeCryptoLayer.layer),
            Layer.provide(Layer.succeed(RelayDb.RelayDb, fakeDb)),
          ),
        ),
      );
    },
  );

  it.effect("revokes active credentials for an environment public key", () => {
    const updateValues: Array<Record<string, unknown>> = [];
    const whereConditions: Array<unknown> = [];
    const fakeDb = {
      select: (fields: Parameters<QueryBuilder["select"]>[0]) => new QueryBuilder().select(fields),
      update: (table: unknown) => {
        expect(table).toBe(relayEnvironmentCredentials);
        return {
          set: (values: Record<string, unknown>) => {
            updateValues.push(values);
            return {
              where: (condition: unknown) => {
                whereConditions.push(condition);
                return {
                  returning: (selection: unknown) => {
                    expect(selection).toBeDefined();
                    return Effect.succeed([{ credentialId: "credential-1" }]);
                  },
                };
              },
            };
          },
        };
      },
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
      const revoked = yield* credentials.revokeForEnvironmentPublicKey({
        environmentId: "env_test",
        environmentPublicKey: "environment-public-key",
      });

      expect(revoked).toBe(true);
      expect(updateValues).toHaveLength(1);
      expect(updateValues[0]?.revokedAt).toEqual(updateValues[0]?.updatedAt);
      expect(whereConditions).toHaveLength(1);

      const query = new PgDialect().sqlToQuery(whereConditions[0] as never);
      expect(query.sql).toContain('"relay_environment_credentials"."environment_id" = $1');
      expect(query.sql).toContain('"relay_environment_credentials"."environment_public_key" = $2');
      expect(query.sql).toContain('"relay_environment_credentials"."revoked_at" is null');
      expect(query.sql).toContain("not exists");
      expect(query.sql).toContain('"relay_environment_links"."environment_id" = $3');
      expect(query.sql).toContain('"relay_environment_links"."environment_public_key" = $4');
      expect(query.sql).toContain('"relay_environment_links"."revoked_at" is null');
      expect(query.params).toEqual([
        "env_test",
        "environment-public-key",
        "env_test",
        "environment-public-key",
      ]);
    }).pipe(
      Effect.provide(
        EnvironmentCredentials.layer.pipe(
          Layer.provide(NodeCryptoLayer.layer),
          Layer.provide(Layer.succeed(RelayDb.RelayDb, fakeDb)),
        ),
      ),
    );
  });

  it.effect("returns false when no credentials are eligible for revocation", () => {
    const fakeDb = {
      select: (fields: Parameters<QueryBuilder["select"]>[0]) => new QueryBuilder().select(fields),
      update: () => ({
        set: () => ({
          where: () => ({ returning: () => Effect.succeed([]) }),
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
      expect(
        yield* credentials.revokeForEnvironmentPublicKey({
          environmentId: "env-1",
          environmentPublicKey: "public-key",
        }),
      ).toBe(false);
    }).pipe(
      Effect.provide(
        EnvironmentCredentials.layer.pipe(
          Layer.provide(NodeCryptoLayer.layer),
          Layer.provide(Layer.succeed(RelayDb.RelayDb, fakeDb)),
        ),
      ),
    );
  });

  it.effect("revokes every orphaned active credential for an environment", () => {
    const whereConditions: Array<unknown> = [];
    const fakeDb = {
      select: (fields: Parameters<QueryBuilder["select"]>[0]) => new QueryBuilder().select(fields),
      update: () => ({
        set: () => ({
          where: (condition: unknown) => {
            whereConditions.push(condition);
            return { returning: () => Effect.succeed([{ credentialId: "credential-1" }]) };
          },
        }),
      }),
    } as unknown as RelayDb.RelayDb["Service"];

    return Effect.gen(function* () {
      const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
      expect(yield* credentials.revokeOrphanedForEnvironment({ environmentId: "env-retry" })).toBe(
        true,
      );

      const query = new PgDialect().sqlToQuery(whereConditions[0] as never);
      expect(query.sql).toContain('"relay_environment_credentials"."environment_id" = $1');
      expect(query.sql).toContain('"relay_environment_credentials"."revoked_at" is null');
      expect(query.sql).toContain("not exists");
      expect(query.sql).toContain(
        '"relay_environment_links"."environment_public_key" = "relay_environment_credentials"."environment_public_key"',
      );
      expect(query.params).toEqual(["env-retry", "env-retry"]);
    }).pipe(
      Effect.provide(
        EnvironmentCredentials.layer.pipe(
          Layer.provide(NodeCryptoLayer.layer),
          Layer.provide(Layer.succeed(RelayDb.RelayDb, fakeDb)),
        ),
      ),
    );
  });

  it.effect(
    "maps credential revocation persistence failures without retaining the public key",
    () => {
      const cause = new Error("update unavailable");
      const publicKey = "sensitive-public-key";
      const fakeDb = {
        select: (fields: Parameters<QueryBuilder["select"]>[0]) =>
          new QueryBuilder().select(fields),
        update: () => ({
          set: () => ({
            where: () => ({ returning: () => Effect.fail(cause) }),
          }),
        }),
      } as unknown as RelayDb.RelayDb["Service"];

      return Effect.gen(function* () {
        const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
        const error = yield* Effect.flip(
          credentials.revokeForEnvironmentPublicKey({
            environmentId: "env-1",
            environmentPublicKey: publicKey,
          }),
        );
        expect(error).toMatchObject({ environmentId: "env-1", cause });
        expect(error).not.toHaveProperty("environmentPublicKey");
        expect(error.message).not.toContain(publicKey);
      }).pipe(
        Effect.provide(
          EnvironmentCredentials.layer.pipe(
            Layer.provide(NodeCryptoLayer.layer),
            Layer.provide(Layer.succeed(RelayDb.RelayDb, fakeDb)),
          ),
        ),
      );
    },
  );
});
