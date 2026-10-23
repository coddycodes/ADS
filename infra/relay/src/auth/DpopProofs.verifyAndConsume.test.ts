// @effect-diagnostics-next-line nodeBuiltinImport:off - Test fixtures generate key pairs synchronously.
import * as NodeCrypto from "node:crypto";
import * as NodeSqlite from "node:sqlite";

import { describe, expect, it } from "@effect/vitest";
import {
  computeDpopAccessTokenHash,
  computeDpopJwkThumbprint,
  type DpopPublicJwk,
} from "@bibcode/shared/dpop";
import { drizzle } from "drizzle-orm/node-sqlite";
import * as DateTime from "effect/DateTime";
import * as Cause from "effect/Cause";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Logger from "effect/Logger";
import * as Ref from "effect/Ref";
import * as Schema from "effect/Schema";
import * as Tracer from "effect/Tracer";
import * as HttpApiError from "effect/http-api/HttpApiError";

import * as RelayDb from "../db.ts";
import { relayDpopProofs } from "../persistence/schema.ts";
import * as DpopProofs from "./DpopProofs.ts";

interface DpopProofInsertValues {
  readonly thumbprint: string;
  readonly jti: string;
  readonly iat: number;
  readonly expiresAt: string;
  readonly createdAt: string;
}

interface StoredDpopProof extends DpopProofInsertValues {}

class ControlledSqlError extends Schema.TaggedError<ControlledSqlError>()("ControlledSqlError", {
  cause: Schema.Defect(),
}) {}

const encodePersistenceError = Schema.encodeSync(DpopProofs.DpopProofReplayPersistenceError);
const isPersistenceError = Schema.is(DpopProofs.DpopProofReplayPersistenceError);

function containsSecret(
  value: unknown,
  secret: string,
  seen: WeakSet<object> = new WeakSet(),
): boolean {
  if (typeof value === "string") {
    return value.includes(secret);
  }
  if (
    value === null ||
    (typeof value !== "object" && typeof value !== "function") ||
    seen.has(value)
  ) {
    return false;
  }
  seen.add(value);
  if (value instanceof Map) {
    return [...value].some(
      ([key, entry]) => containsSecret(key, secret, seen) || containsSecret(entry, secret, seen),
    );
  }
  return Reflect.ownKeys(value).some(
    (key) =>
      (typeof key === "string" && key.includes(secret)) ||
      containsSecret(Reflect.get(value, key), secret, seen),
  );
}

function controlledSqlDatabase(options?: { readonly beforeInsert?: Effect.Effect<void> }) {
  return Effect.acquireRelease(
    Effect.sync(() => {
      const client = new NodeSqlite.DatabaseSync(":memory:");
      client.exec(`
        create table relay_dpop_proofs (
          thumbprint text not null,
          jti text not null,
          iat integer not null,
          expires_at text not null,
          created_at text not null,
          primary key (thumbprint, jti)
        )
      `);
      const db = drizzle({ client });
      const relayDb = {
        insert: (table: typeof relayDpopProofs) => ({
          values: (values: DpopProofInsertValues) => ({
            onConflictDoNothing: () => ({
              returning: (selection: unknown) => {
                const execute = Effect.try({
                  try: () => {
                    const query = db
                      .insert(table as never)
                      .values(values as never)
                      .onConflictDoNothing()
                      .returning(selection as never) as unknown as {
                      readonly all: () => ReadonlyArray<{ readonly jti: string }>;
                    };
                    return query.all();
                  },
                  catch: (cause) => new ControlledSqlError({ cause }),
                });
                return options?.beforeInsert === undefined
                  ? execute
                  : options.beforeInsert.pipe(Effect.andThen(execute));
              },
            }),
          }),
        }),
        delete: (table: typeof relayDpopProofs) => ({
          where: (condition: unknown) =>
            Effect.try({
              try: () => {
                db.delete(table as never)
                  .where(condition as never)
                  .run();
              },
              catch: (cause) => new ControlledSqlError({ cause }),
            }),
        }),
      } as unknown as RelayDb.RelayDb["Service"];

      return {
        close: () => client.close(),
        layer: DpopProofs.layer.pipe(Layer.provide(Layer.succeed(RelayDb.RelayDb, relayDb))),
        rows: () =>
          client
            .prepare(
              "select thumbprint, jti, iat, expires_at as expiresAt, created_at as createdAt from relay_dpop_proofs order by jti",
            )
            .all() as unknown as ReadonlyArray<StoredDpopProof>,
        seed: (values: DpopProofInsertValues) =>
          client
            .prepare(
              "insert into relay_dpop_proofs (thumbprint, jti, iat, expires_at, created_at) values (?, ?, ?, ?, ?)",
            )
            .run(values.thumbprint, values.jti, values.iat, values.expiresAt, values.createdAt),
      };
    }),
    (database) => Effect.sync(database.close),
  );
}

function makeDpopProof(input: {
  readonly method: string;
  readonly url: string;
  readonly iat: number;
  readonly jti: string;
  readonly accessToken?: string;
}) {
  const { privateKey, publicKey } = NodeCrypto.generateKeyPairSync("ec", {
    namedCurve: "P-256",
  });
  const publicJwk = publicKey.export({ format: "jwk" }) as DpopPublicJwk;
  const header = Buffer.from(
    JSON.stringify({
      typ: "dpop+jwt",
      alg: "ES256",
      jwk: publicJwk,
    }),
  ).toString("base64url");
  const payload = Buffer.from(
    JSON.stringify({
      htm: input.method,
      htu: input.url,
      jti: input.jti,
      iat: input.iat,
      ...(input.accessToken ? { ath: computeDpopAccessTokenHash(input.accessToken) } : {}),
    }),
  ).toString("base64url");
  const signature = Buffer.from(
    NodeCrypto.sign("sha256", Buffer.from(`${header}.${payload}`), {
      key: privateKey,
      dsaEncoding: "ieee-p1363",
    }),
  ).toString("base64url");
  return {
    proof: `${header}.${payload}.${signature}`,
    thumbprint: computeDpopJwkThumbprint(publicJwk),
  };
}

function layer(
  insert: (
    values: DpopProofInsertValues,
  ) => Effect.Effect<ReadonlyArray<{ readonly jti: string }>, { _tag: string }>,
) {
  const fakeDb = {
    insert: (table: unknown) => {
      expect(table).toBe(relayDpopProofs);
      return {
        values: (values: DpopProofInsertValues) => ({
          onConflictDoNothing: () => ({
            returning: (selection: unknown) => {
              expect(selection).toBeDefined();
              return insert(values);
            },
          }),
        }),
      };
    },
  } as unknown as RelayDb.RelayDb["Service"];
  return DpopProofs.layer.pipe(Layer.provide(Layer.succeed(RelayDb.RelayDb, fakeDb)));
}

describe("DpopProofReplay.verifyAndConsume", () => {
  it.effect("rejects replayed proofs after persistence consumes the jti once", () => {
    const now = DateTime.makeUnsafe("2026-05-25T12:00:00.000Z");
    const proof = makeDpopProof({
      method: "POST",
      url: "https://relay.example.com/v1/environments/env/connect",
      iat: Math.floor(now.epochMilliseconds / 1_000),
      jti: "proof-1",
    });

    return Effect.scoped(
      Effect.gen(function* () {
        const database = yield* controlledSqlDatabase();
        return yield* Effect.gen(function* () {
          const replay = yield* DpopProofs.DpopProofReplay;
          const first = yield* replay.verifyAndConsume({
            proof: proof.proof,
            method: "POST",
            url: "https://relay.example.com/v1/environments/env/connect",
            expectedThumbprint: proof.thumbprint,
            now,
          });
          const second = yield* Effect.exit(
            replay.verifyAndConsume({
              proof: proof.proof,
              method: "POST",
              url: "https://relay.example.com/v1/environments/env/connect",
              expectedThumbprint: proof.thumbprint,
              now,
            }),
          );

          expect(first).toBe(proof.thumbprint);
          expect(second._tag).toBe("Failure");
          expect(database.rows()).toHaveLength(1);
        }).pipe(Effect.provide(database.layer));
      }),
    );
  });

  it.effect("lets exactly one concurrent consume win through the database primary key", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const arrivals = yield* Ref.make(0);
        const ready = yield* Deferred.make<void>();
        const beforeInsert = Ref.updateAndGet(arrivals, (count) => count + 1).pipe(
          Effect.tap((count) => (count === 2 ? Deferred.succeed(ready, undefined) : Effect.void)),
          Effect.andThen(Deferred.await(ready)),
        );
        const database = yield* controlledSqlDatabase({ beforeInsert });

        const results = yield* Effect.gen(function* () {
          const replay = yield* DpopProofs.DpopProofReplay;
          const input = {
            thumbprint: "concurrent-thumbprint",
            jti: "concurrent-jti",
            iat: 1_771_000_000,
            expiresAt: DateTime.makeUnsafe("2026-05-25T12:05:00.000Z"),
          } as const;
          return yield* Effect.all([replay.consume(input), replay.consume(input)], {
            concurrency: "unbounded",
          });
        }).pipe(Effect.provide(database.layer));

        expect(results.filter(Boolean)).toHaveLength(1);
        expect(results.filter((result) => !result)).toHaveLength(1);
        expect(yield* Ref.get(arrivals)).toBe(2);
        expect(database.rows()).toHaveLength(1);
      }),
    ),
  );

  it.effect("prunes only rows strictly before now through the database predicate", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const database = yield* controlledSqlDatabase();
        const now = yield* DateTime.now;
        const nowIso = DateTime.formatIso(now);
        database.seed({
          thumbprint: "thumbprint",
          jti: "just-before-now",
          iat: 1,
          expiresAt: DateTime.formatIso(DateTime.makeUnsafe(now.epochMilliseconds - 1)),
          createdAt: nowIso,
        });
        database.seed({
          thumbprint: "thumbprint",
          jti: "exactly-now",
          iat: 2,
          expiresAt: nowIso,
          createdAt: nowIso,
        });

        yield* Effect.gen(function* () {
          const replay = yield* DpopProofs.DpopProofReplay;
          yield* replay.pruneExpired;
        }).pipe(Effect.provide(database.layer));

        expect(database.rows().map((row) => row.jti)).toEqual(["exactly-now"]);
      }),
    ),
  );

  it.effect("rejects proofs missing the expected access token hash", () => {
    const now = DateTime.makeUnsafe("2026-05-25T12:00:00.000Z");
    const proof = makeDpopProof({
      method: "POST",
      url: "https://relay.example.com/v1/environments/env/connect",
      iat: Math.floor(now.epochMilliseconds / 1_000),
      jti: "proof-1",
    });
    let persistenceAttempts = 0;

    return Effect.gen(function* () {
      const replay = yield* DpopProofs.DpopProofReplay;
      const error = yield* Effect.flip(
        replay.verifyAndConsume({
          proof: proof.proof,
          method: "POST",
          url: "https://relay.example.com/v1/environments/env/connect",
          expectedThumbprint: proof.thumbprint,
          expectedAccessToken: "clerk-access-token",
          now,
        }),
      );

      expect(error).toBeInstanceOf(HttpApiError.Unauthorized);
      expect(persistenceAttempts).toBe(0);
    }).pipe(
      Effect.provide(
        layer(() => {
          persistenceAttempts += 1;
          return Effect.die("unexpected DPoP replay persistence");
        }),
      ),
    );
  });

  it.effect("fails closed for present empty thumbprint and access-token bindings", () => {
    const now = DateTime.makeUnsafe("2026-05-25T12:00:00.000Z");
    const emptyThumbprintProof = makeDpopProof({
      method: "POST",
      url: "https://relay.example.com/v1/environments/env/connect",
      iat: Math.floor(now.epochMilliseconds / 1_000),
      jti: "empty-thumbprint",
    });
    const emptyAccessTokenProof = makeDpopProof({
      method: "POST",
      url: "https://relay.example.com/v1/environments/env/connect",
      iat: Math.floor(now.epochMilliseconds / 1_000),
      jti: "empty-access-token",
    });

    return Effect.gen(function* () {
      const replay = yield* DpopProofs.DpopProofReplay;
      const emptyThumbprint = yield* Effect.exit(
        replay.verifyAndConsume({
          proof: emptyThumbprintProof.proof,
          method: "POST",
          url: "https://relay.example.com/v1/environments/env/connect",
          expectedThumbprint: "",
          now,
        }),
      );
      const emptyAccessToken = yield* Effect.exit(
        replay.verifyAndConsume({
          proof: emptyAccessTokenProof.proof,
          method: "POST",
          url: "https://relay.example.com/v1/environments/env/connect",
          expectedAccessToken: "",
          now,
        }),
      );

      expect(emptyThumbprint._tag).toBe("Failure");
      expect(emptyAccessToken._tag).toBe("Failure");
    }).pipe(Effect.provide(layer(() => Effect.die("invalid binding must not reach persistence"))));
  });

  it.effect("keeps omitted thumbprint and access-token bindings optional", () => {
    const now = DateTime.makeUnsafe("2026-05-25T12:00:00.000Z");
    const proof = makeDpopProof({
      method: "POST",
      url: "https://relay.example.com/v1/environments/env/connect",
      iat: Math.floor(now.epochMilliseconds / 1_000),
      jti: "bindings-omitted",
    });

    return Effect.scoped(
      Effect.gen(function* () {
        const database = yield* controlledSqlDatabase();
        return yield* Effect.gen(function* () {
          const replay = yield* DpopProofs.DpopProofReplay;
          expect(
            yield* replay.verifyAndConsume({
              proof: proof.proof,
              method: "POST",
              url: "https://relay.example.com/v1/environments/env/connect",
              now,
            }),
          ).toBe(proof.thumbprint);
          expect(database.rows()).toHaveLength(1);
        }).pipe(Effect.provide(database.layer));
      }),
    );
  });

  it.effect(
    "accepts the time-window boundary and rejects an expired proof without consuming it",
    () => {
      const now = DateTime.makeUnsafe("2026-05-25T12:00:00.000Z");
      const nowSeconds = Math.floor(now.epochMilliseconds / 1_000);
      const boundaryProof = makeDpopProof({
        method: "POST",
        url: "https://relay.example.com/v1/environments/env/connect",
        iat: nowSeconds - 300,
        jti: "time-window-boundary",
      });
      const expiredProof = makeDpopProof({
        method: "POST",
        url: "https://relay.example.com/v1/environments/env/connect",
        iat: nowSeconds - 301,
        jti: "time-window-expired",
      });
      return Effect.scoped(
        Effect.gen(function* () {
          const database = yield* controlledSqlDatabase();
          return yield* Effect.gen(function* () {
            const replay = yield* DpopProofs.DpopProofReplay;
            expect(
              yield* replay.verifyAndConsume({
                proof: boundaryProof.proof,
                method: "POST",
                url: "https://relay.example.com/v1/environments/env/connect",
                now,
              }),
            ).toBe(boundaryProof.thumbprint);
            const expired = yield* Effect.exit(
              replay.verifyAndConsume({
                proof: expiredProof.proof,
                method: "POST",
                url: "https://relay.example.com/v1/environments/env/connect",
                now,
              }),
            );

            expect(expired._tag).toBe("Failure");
            expect(database.rows()).toEqual([
              expect.objectContaining({
                jti: "time-window-boundary",
                iat: nowSeconds - 300,
                expiresAt: "2026-05-25T12:05:00.000Z",
              }),
            ]);
          }).pipe(Effect.provide(database.layer));
        }),
      );
    },
  );

  it.effect("records rejection observability without exposing proof or token secrets", () => {
    const now = DateTime.makeUnsafe("2026-05-25T12:00:00.000Z");
    const proofSecret = "proof-secret";
    const accessTokenSecret = "access-token-secret";
    const querySecret = "query-credential-secret";
    const requestUrl = `https://relay.example.com/v1/environments/env/connect?credential=${querySecret}`;
    const loggedMessages: Array<unknown> = [];
    const spans: Array<Tracer.Span> = [];
    const logger = Logger.make((options) => {
      loggedMessages.push(options.message);
    });
    const tracer = Tracer.make({
      span: (options) => {
        const span = new Tracer.NativeSpan(options);
        spans.push(span);
        return span;
      },
    });

    return Effect.gen(function* () {
      const replay = yield* DpopProofs.DpopProofReplay;
      const error = yield* Effect.flip(
        replay.verifyAndConsume({
          proof: proofSecret,
          method: "POST",
          url: requestUrl,
          expectedThumbprint: "expected-thumbprint",
          expectedAccessToken: accessTokenSecret,
          now,
        }),
      );

      expect(error).toBeInstanceOf(HttpApiError.Unauthorized);
      expect(String(error)).not.toContain(querySecret);
      expect(loggedMessages).toEqual([
        [
          "relay dpop proof rejected",
          {
            reason: "Invalid DPoP compact JWT.",
            method: "POST",
            expectedThumbprintPresent: true,
            expectedAccessTokenPresent: true,
          },
        ],
      ]);
      const verificationSpan = spans.find(
        (span) => span.name === "relay.dpop_proofs.verify_and_consume",
      );
      expect(verificationSpan?.attributes.get("relay.dpop.method")).toBe("POST");
      expect(verificationSpan?.attributes.get("relay.dpop.expected_thumbprint_present")).toBe(true);
      expect(verificationSpan?.attributes.get("relay.dpop.expected_access_token_present")).toBe(
        true,
      );
      expect([...(verificationSpan?.attributes.values() ?? [])]).not.toContain(proofSecret);
      expect([...(verificationSpan?.attributes.values() ?? [])]).not.toContain(accessTokenSecret);
      expect(spans.flatMap((span) => [...span.attributes.values()])).not.toContain(querySecret);
      for (const secret of [proofSecret, accessTokenSecret, querySecret]) {
        expect(containsSecret(loggedMessages, secret)).toBe(false);
        expect(containsSecret(spans, secret)).toBe(false);
        expect(containsSecret(error, secret)).toBe(false);
      }
    }).pipe(
      Effect.withLogger(logger),
      Effect.provideService(Tracer.Tracer, tracer),
      Effect.provide(layer(() => Effect.die("rejected proof must not reach persistence"))),
    );
  });

  it.effect("preserves replay persistence failures", () => {
    const now = DateTime.makeUnsafe("2026-05-25T12:00:00.000Z");
    const proof = makeDpopProof({
      method: "POST",
      url: "https://relay.example.com/v1/environments/env/connect",
      iat: Math.floor(now.epochMilliseconds / 1_000),
      jti: "proof-persistence-failure",
    });
    const urlSecret = "https://database.example.test/?credential=url-secret";
    const tokenSecret = "database-token-secret";
    const credentialSecret = "database-credential-secret";
    const cause = {
      _tag: "DatabaseUnavailable",
      diagnostic: `${urlSecret} ${tokenSecret} ${credentialSecret}`,
    } as const;
    const loggedMessages: Array<unknown> = [];
    const spans: Array<Tracer.Span> = [];
    const logger = Logger.make((options) => {
      loggedMessages.push(options.message);
    });
    const tracer = Tracer.make({
      span: (options) => {
        const span = new Tracer.NativeSpan(options);
        spans.push(span);
        return span;
      },
    });

    return Effect.gen(function* () {
      const replay = yield* DpopProofs.DpopProofReplay;
      const error = yield* Effect.flip(
        replay.verifyAndConsume({
          proof: proof.proof,
          method: "POST",
          url: "https://relay.example.com/v1/environments/env/connect",
          expectedThumbprint: proof.thumbprint,
          now,
        }),
      );

      if (!isPersistenceError(error)) {
        return yield* Effect.die("expected DPoP replay persistence error");
      }

      expect(error).toMatchObject({
        _tag: "DpopProofReplayPersistenceError",
        operation: "consume",
        thumbprint: proof.thumbprint,
        jti: "proof-persistence-failure",
        iat: Math.floor(now.epochMilliseconds / 1_000),
      });
      expect(error.cause).toBe("database-error");
      expect(error).not.toHaveProperty("proof");
      expect(encodePersistenceError(error)).toEqual({
        _tag: "DpopProofReplayPersistenceError",
        operation: "consume",
        thumbprint: proof.thumbprint,
        jti: "proof-persistence-failure",
        iat: Math.floor(now.epochMilliseconds / 1_000),
        cause: "database-error",
      });
      expect(error.message).not.toContain(urlSecret);
      expect(error.message).not.toContain(tokenSecret);
      expect(error.message).not.toContain(credentialSecret);
      const diagnostics = Cause.pretty(Cause.fail(error));
      expect(diagnostics).not.toContain(urlSecret);
      expect(diagnostics).not.toContain(tokenSecret);
      expect(diagnostics).not.toContain(credentialSecret);
      expect(loggedMessages).toEqual([]);
      const spanAttributes = spans.flatMap((span) => [...span.attributes.values()]);
      expect(spanAttributes).not.toContain(urlSecret);
      expect(spanAttributes).not.toContain(tokenSecret);
      expect(spanAttributes).not.toContain(credentialSecret);
    }).pipe(
      Effect.withLogger(logger),
      Effect.provideService(Tracer.Tracer, tracer),
      Effect.provide(layer(() => Effect.fail(cause))),
    );
  });

  it.effect("accepts proofs bound to the access token hash", () => {
    const now = DateTime.makeUnsafe("2026-05-25T12:00:00.000Z");
    const proof = makeDpopProof({
      method: "POST",
      url: "https://relay.example.com/v1/environments/env/status",
      iat: Math.floor(now.epochMilliseconds / 1_000),
      jti: "proof-status-1",
      accessToken: "clerk-access-token",
    });

    return Effect.scoped(
      Effect.gen(function* () {
        const database = yield* controlledSqlDatabase();
        return yield* Effect.gen(function* () {
          const replay = yield* DpopProofs.DpopProofReplay;
          const thumbprint = yield* replay.verifyAndConsume({
            proof: proof.proof,
            method: "POST",
            url: "https://relay.example.com/v1/environments/env/status",
            expectedAccessToken: "clerk-access-token",
            now,
          });
          const second = yield* Effect.exit(
            replay.verifyAndConsume({
              proof: proof.proof,
              method: "POST",
              url: "https://relay.example.com/v1/environments/env/status",
              expectedAccessToken: "clerk-access-token",
              now,
            }),
          );

          expect(thumbprint).toBe(proof.thumbprint);
          expect(second._tag).toBe("Failure");
          expect(database.rows()).toHaveLength(1);
        }).pipe(Effect.provide(database.layer));
      }),
    );
  });
});
