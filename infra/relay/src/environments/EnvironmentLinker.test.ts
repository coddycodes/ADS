// @effect-diagnostics-next-line nodeBuiltinImport:off - Test fixtures generate key pairs synchronously.
import * as NodeCrypto from "node:crypto";
import type {
  RelayEnvironmentLinkProofPayload,
  RelayEnvironmentLinkRequest,
} from "@coddy/contracts/relay";
import { RELAY_LINK_PROOF_TYP } from "@coddy/shared/relayJwt";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import { describe, expect, it } from "@effect/vitest";
import * as DateTime from "effect/DateTime";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Redacted from "effect/Redacted";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";

import * as DpopProofs from "../auth/DpopProofs.ts";
import * as RelayTokens from "../auth/RelayTokens.ts";
import * as EnvironmentCredentials from "./EnvironmentCredentials.ts";
import * as EnvironmentLinks from "./EnvironmentLinks.ts";
import * as RelayConfiguration from "../Config.ts";
import * as EnvironmentLinker from "./EnvironmentLinker.ts";
import * as ManagedEndpointAllocations from "./ManagedEndpointAllocations.ts";
import * as ManagedEndpointProvider from "./ManagedEndpointProvider.ts";

const relayKeyPair = NodeCrypto.generateKeyPairSync("ed25519", {
  privateKeyEncoding: { format: "pem", type: "pkcs8" },
  publicKeyEncoding: { format: "pem", type: "spki" },
});
const environmentKeyPair = NodeCrypto.generateKeyPairSync("ed25519", {
  privateKeyEncoding: { format: "pem", type: "pkcs8" },
  publicKeyEncoding: { format: "pem", type: "spki" },
});
const config = RelayConfiguration.RelayConfiguration.of({
  relayIssuer: "https://relay.example.test",
  clerkSecretKey: Redacted.make("clerk-secret"),
  clerkPublishableKey: "pk_test_test",
  clerkJwtAudience: "coddy-studio-relay",
  cloudMintPrivateKey: Redacted.make(relayKeyPair.privateKey),
  cloudMintPublicKey: relayKeyPair.publicKey,
  managedEndpointBaseDomain: undefined,
  managedEndpointNamespace: undefined,
});
const isEnvironmentLinkProofInvalid = Schema.is(EnvironmentLinker.EnvironmentLinkProofInvalid);

function signTestJwt(payload: object, typ: string, privateKey: string): string {
  const header = Buffer.from(JSON.stringify({ alg: "EdDSA", typ })).toString("base64url");
  const encodedPayload = Buffer.from(JSON.stringify(payload)).toString("base64url");
  const signingInput = `${header}.${encodedPayload}`;
  return `${signingInput}.${Buffer.from(NodeCrypto.sign(null, Buffer.from(signingInput), privateKey)).toString("base64url")}`;
}

const makeRequestFor = (managedTunnelsEnabled: boolean) =>
  Effect.gen(function* () {
    const now = yield* DateTime.now;
    const expiresAt = DateTime.add(now, { minutes: 5 });
    const relayTokens = yield* RelayTokens.RelayTokens;
    const challenge = yield* relayTokens.issueLinkChallenge({
      userId: "user_123",
      request: {
        managedTunnelsEnabled,
      },
      jti: "challenge-jti",
      issuedAtEpochSeconds: Math.floor(now.epochMilliseconds / 1_000),
      expiresAtEpochSeconds: Math.floor(expiresAt.epochMilliseconds / 1_000),
    });
    const payload = {
      iss: "coddy-studio-env:env-link-test",
      aud: "https://relay.example.test",
      sub: "env-link-test",
      jti: "link-proof-jti",
      iat: Math.floor(now.epochMilliseconds / 1_000),
      exp: Math.floor(expiresAt.epochMilliseconds / 1_000),
      challenge,
      environmentId: "env-link-test" as RelayEnvironmentLinkProofPayload["environmentId"],
      descriptor: {
        environmentId: "env-link-test" as RelayEnvironmentLinkProofPayload["environmentId"],
        label: "Link Test Environment",
        platform: { os: "darwin", arch: "arm64" },
        serverVersion: "0.0.0-test",
        storageInstanceId: "019c18d0-26b2-7a35-9e06-8568e640f44f",
        bootId: null,
        remoteUpdateSupport: null,
        remoteProtocolVersion: 1,
        minCompatibleRemoteProtocol: 1,
        capabilities: makeTestExecutionEnvironmentCapabilities({ repositoryIdentity: true }),
      },
      environmentPublicKey: environmentKeyPair.publicKey.trim(),
      endpoint: {
        httpBaseUrl: "https://env.example.test/",
        wsBaseUrl: "wss://env.example.test/",
        providerKind: "manual",
      },
      origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      scopes: ["managed_tunnels"],
    } satisfies RelayEnvironmentLinkProofPayload;
    return {
      request: {
        proof: signTestJwt(payload, RELAY_LINK_PROOF_TYP, environmentKeyPair.privateKey),
        managedTunnelsEnabled,
      } satisfies RelayEnvironmentLinkRequest,
      payload,
    };
  });

const makeRequest = makeRequestFor(false);

function withSignedPayload(
  request: RelayEnvironmentLinkRequest,
  payload: RelayEnvironmentLinkProofPayload,
  overrides: Partial<RelayEnvironmentLinkProofPayload>,
  requestOverrides: Partial<RelayEnvironmentLinkRequest> = {},
): RelayEnvironmentLinkRequest {
  const nextPayload = { ...payload, ...overrides };
  return {
    ...request,
    ...requestOverrides,
    proof: signTestJwt(nextPayload, RELAY_LINK_PROOF_TYP, environmentKeyPair.privateKey),
  };
}

function testLayer(input?: {
  readonly upsert?: EnvironmentLinks.EnvironmentLinks["Service"]["upsert"];
  readonly consume?: DpopProofs.DpopProofReplay["Service"]["consume"];
  readonly provision?: ManagedEndpointProvider.ManagedEndpointProvider["Service"]["provision"];
  readonly deprovision?: ManagedEndpointProvider.ManagedEndpointProvider["Service"]["deprovision"];
  readonly createCredential?: EnvironmentCredentials.EnvironmentCredentials["Service"]["create"];
  readonly rotateCredential?: EnvironmentCredentials.EnvironmentCredentials["Service"]["rotate"];
  readonly rollbackCredential?: EnvironmentCredentials.EnvironmentCredentials["Service"]["rollbackRotation"];
  readonly revokeLink?: EnvironmentLinks.EnvironmentLinks["Service"]["revokeForUser"];
  readonly revokeOrphaned?: EnvironmentCredentials.EnvironmentCredentials["Service"]["revokeOrphanedForEnvironment"];
  readonly getLink?: EnvironmentLinks.EnvironmentLinks["Service"]["getForUser"];
  readonly restoreLink?: EnvironmentLinks.EnvironmentLinks["Service"]["restoreForUser"];
}) {
  return EnvironmentLinker.layer.pipe(
    Layer.provideMerge(RelayTokens.layer),
    Layer.provide(
      Layer.mergeAll(
        RelayConfiguration.layer(config),
        Layer.succeed(DpopProofs.DpopProofReplay, {
          verifyAndConsume: () => Effect.die("unexpected DPoP proof verification"),
          consume: input?.consume ?? (() => Effect.succeed(true)),
          pruneExpired: Effect.void,
        }),
        Layer.succeed(EnvironmentLinks.EnvironmentLinks, {
          upsert: input?.upsert ?? (() => Effect.void),
          listUsersForEnvironment: () => Effect.succeed([]),
          listPublicKeysForEnvironment: () => Effect.succeed([]),
          listForUser: () => Effect.succeed([]),
          getForUser: input?.getLink ?? (() => Effect.succeed(null)),
          restoreForUser: input?.restoreLink ?? (() => Effect.void),
          revokeForUser: input?.revokeLink ?? (() => Effect.succeed(false)),
        }),
        Layer.succeed(EnvironmentCredentials.EnvironmentCredentials, {
          create: input?.createCredential ?? (() => Effect.succeed("coddystudioenv_credential_secret")),
          rotate:
            input?.rotateCredential ??
            ((rotationInput) =>
              (input?.createCredential ?? (() => Effect.succeed("coddystudioenv_credential_secret")))(
                rotationInput,
              ).pipe(
                Effect.map((token) => ({
                  token,
                  credentialId: "test-credential-id",
                  previousCredentialId: null,
                  ...rotationInput,
                })),
              )),
          rollbackRotation: input?.rollbackCredential ?? (() => Effect.succeed(true)),
          authenticate: () => Effect.succeedNone,
          revokeForEnvironmentPublicKey: () => Effect.succeed(false),
          revokeOrphanedForEnvironment: input?.revokeOrphaned ?? (() => Effect.succeed(false)),
        }),
        Layer.succeed(ManagedEndpointProvider.ManagedEndpointProvider, {
          deprovision: input?.deprovision ?? (() => Effect.void),
          provision:
            input?.provision ??
            (() =>
              Effect.succeed({
                endpoint: {
                  httpBaseUrl: "https://managed.example.test/",
                  wsBaseUrl: "wss://managed.example.test/ws",
                  providerKind: "cloudflare_tunnel",
                },
                runtime: { providerKind: "cloudflare_tunnel", connectorToken: "connector-token" },
                endpointDisposition: "created",
              })),
        }),
        Layer.succeed(ManagedEndpointAllocations.ManagedEndpointAllocations, {
          withOperation: (operationInput, use) =>
            use({ ...operationInput, generation: 1, ownerToken: "test-operation-owner" }),
          acquireOperation: () => Effect.die("unused"),
          releaseOperation: () => Effect.die("unused"),
          renewOperation: () => Effect.die("unused"),
          claimForOperation: () => Effect.die("unused"),
          get: () => Effect.die("unused"),
          reserve: () => Effect.die("unused"),
          recordTunnel: () => Effect.die("unused"),
          recordDns: () => Effect.die("unused"),
          markReady: () => Effect.die("unused"),
          remove: () => Effect.die("unused"),
        }),
      ),
    ),
  );
}

describe("EnvironmentLinker", () => {
  it("formats expired and invalid proof errors without serializing causes", () => {
    const expired = new EnvironmentLinker.EnvironmentLinkProofExpired({
      userId: "user-1",
      environmentId: "env-1",
      expiresAt: "2026-01-01T00:00:00.000Z",
    });
    const invalid = new EnvironmentLinker.EnvironmentLinkProofInvalid({
      userId: "user-1",
      environmentId: "env-1",
      reason: "invalid_signature_or_scope",
      stage: "decode_token",
      cause: new Error("secret-proof"),
    });
    expect(expired.message).toBe(
      "Environment 'env-1' link proof expired at 2026-01-01T00:00:00.000Z",
    );
    expect(invalid.message).toBe(
      "Environment 'env-1' link proof is invalid during decode_token: invalid_signature_or_scope",
    );
    expect(JSON.stringify(invalid)).not.toContain("secret-proof");
  });

  it.effect("classifies compact-token and payload decoding failures before side effects", () =>
    Effect.gen(function* () {
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const malformed = yield* Effect.flip(
        linker.link({
          userId: "user_123",
          request: { proof: "not-a-jwt", managedTunnelsEnabled: false },
        }),
      );
      expect(malformed).toMatchObject({
        environmentId: "unknown",
        stage: "decode_token",
        reason: "invalid_signature_or_scope",
      });

      const invalidPayload = yield* Effect.flip(
        linker.link({
          userId: "user_123",
          request: {
            proof: signTestJwt({}, RELAY_LINK_PROOF_TYP, environmentKeyPair.privateKey),
            managedTunnelsEnabled: false,
          },
        }),
      );
      expect(invalidPayload).toMatchObject({
        environmentId: "unknown",
        stage: "decode_payload",
        reason: "invalid_signature_or_scope",
      });
    }).pipe(Effect.provide(testLayer())),
  );

  it.effect("rejects expired proofs before signature verification", () =>
    Effect.gen(function* () {
      const { request, payload } = yield* makeRequestFor(true);
      const now = yield* DateTime.now;
      const expiredRequest = withSignedPayload(request, payload, {
        exp: Math.floor(now.epochMilliseconds / 1_000) - 1,
      });
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const error = yield* Effect.flip(
        linker.link({ userId: "user_123", request: expiredRequest }),
      );
      expect(error).toMatchObject({
        _tag: "EnvironmentLinkProofExpired",
        environmentId: "env-link-test",
      });
      expect(error.message).toContain("link proof expired at");
    }).pipe(Effect.provide(testLayer())),
  );

  it.effect("enforces subject, capability, and descriptor authorization", () =>
    Effect.gen(function* () {
      const { request, payload } = yield* makeRequestFor(true);
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const cases = [
        withSignedPayload(request, payload, { sub: "different-environment" }),
        withSignedPayload(request, payload, { scopes: [] }, { managedTunnelsEnabled: true }),
        withSignedPayload(request, payload, {
          descriptor: { ...payload.descriptor, environmentId: "different" as never },
        }),
      ];
      const errors = yield* Effect.forEach(cases, (candidate) =>
        Effect.flip(linker.link({ userId: "user_123", request: candidate })),
      );
      expect(errors.every(isEnvironmentLinkProofInvalid)).toBe(true);
      const proofErrors = errors.filter(isEnvironmentLinkProofInvalid);
      expect(proofErrors.map((error) => error.stage)).toEqual([
        "authorize_capabilities",
        "authorize_capabilities",
        "validate_descriptor",
      ]);
    }).pipe(Effect.provide(testLayer())),
  );

  it.effect("rejects invalid challenges and separately consumed challenge replays", () => {
    let consumeCalls = 0;
    return Effect.gen(function* () {
      const { request, payload } = yield* makeRequest;
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const invalidChallenge = withSignedPayload(request, payload, { challenge: "invalid" });
      const challengeError = yield* Effect.flip(
        linker.link({ userId: "user_123", request: invalidChallenge }),
      );
      expect(challengeError).toMatchObject({
        stage: "verify_challenge",
        reason: "challenge_invalid",
      });

      const replay = yield* Effect.flip(linker.link({ userId: "user_123", request }));
      expect(replay).toMatchObject({
        stage: "consume_challenge_nonce",
        reason: "challenge_invalid",
      });
    }).pipe(
      Effect.provide(
        testLayer({
          consume: () => Effect.succeed(++consumeCalls === 1),
        }),
      ),
    );
  });

  it.effect("rejects proof expirations outside the DateTime range", () =>
    Effect.gen(function* () {
      const { request, payload } = yield* makeRequest;
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const invalidExpiration = withSignedPayload(request, payload, {
        exp: Number.MAX_SAFE_INTEGER,
      });
      const error = yield* Effect.flip(
        linker.link({ userId: "user_123", request: invalidExpiration }),
      );
      expect(error).toMatchObject({
        stage: "validate_expiration",
        reason: "invalid_signature_or_scope",
      });
    }).pipe(Effect.provide(testLayer())),
  );

  it.effect("validates managed origins and returns provisioned runtime for managed links", () =>
    Effect.gen(function* () {
      const { request, payload } = yield* makeRequestFor(true);
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const invalidOrigin = withSignedPayload(
        request,
        payload,
        { origin: { localHttpHost: "example.test", localHttpPort: 3773 } },
        { managedTunnelsEnabled: true },
      );
      const error = yield* Effect.flip(linker.link({ userId: "user_123", request: invalidOrigin }));
      expect(error).toMatchObject({ stage: "validate_origin", reason: "origin_not_allowed" });

      const managed = withSignedPayload(
        request,
        payload,
        { origin: { localHttpHost: "[::1]", localHttpPort: 3773 } },
        { managedTunnelsEnabled: true },
      );
      const result = yield* linker.link({ userId: "user_123", request: managed });
      expect(result.endpoint.providerKind).toBe("cloudflare_tunnel");
      expect(result.endpointRuntime).toMatchObject({ connectorToken: "connector-token" });
    }).pipe(Effect.provide(testLayer())),
  );

  it.effect("rejects insecure and malformed manual endpoints before persistence", () =>
    Effect.gen(function* () {
      const { request, payload } = yield* makeRequest;
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const cases = [
        withSignedPayload(request, payload, {
          endpoint: { ...payload.endpoint, httpBaseUrl: "http://env.example.test/" },
        }),
        withSignedPayload(request, payload, {
          endpoint: { ...payload.endpoint, wsBaseUrl: "not-a-url" },
        }),
      ];
      const errors = yield* Effect.forEach(cases, (candidate) =>
        Effect.flip(linker.link({ userId: "user_123", request: candidate })),
      );
      expect(errors).toEqual([
        expect.objectContaining({ stage: "validate_endpoint", reason: "endpoint_not_secure" }),
        expect.objectContaining({ stage: "validate_endpoint", reason: "endpoint_not_secure" }),
      ]);
    }).pipe(Effect.provide(testLayer())),
  );
  it.effect("uses verified JWT claims when linking an environment", () => {
    let persistedEnvironmentId: string | null = null;
    return Effect.gen(function* () {
      const { request, payload } = yield* makeRequest;
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const result = yield* linker.link({ userId: "user_123", request });
      expect(result.environmentId).toBe(payload.environmentId);
      expect(result.environmentCredential).toBe("coddystudioenv_credential_secret");
      expect(persistedEnvironmentId).toBe(payload.environmentId);
    }).pipe(
      Effect.provide(
        testLayer({
          upsert: (input) =>
            Effect.sync(() => {
              persistedEnvironmentId = input.proof.environmentId;
            }),
        }),
      ),
    );
  });

  it.effect("compensates a persisted managed link when credential creation fails", () => {
    const events: Array<string> = [];
    const primary = new EnvironmentCredentials.EnvironmentCredentialCreatePersistenceError({
      stage: "insert-credential",
      environmentId: "env-link-test",
      cause: new Error("database unavailable"),
    });

    return Effect.gen(function* () {
      const { request, payload } = yield* makeRequestFor(true);
      const managed = withSignedPayload(
        request,
        payload,
        { origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 } },
        { managedTunnelsEnabled: true },
      );
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const error = yield* Effect.flip(linker.link({ userId: "user_123", request: managed }));

      expect(error).toBe(primary);
      expect(events).toEqual([
        "provision:1",
        "upsert",
        "credential",
        "revoke-link",
        "revoke-credentials",
        "deprovision:1",
      ]);
    }).pipe(
      Effect.provide(
        testLayer({
          provision: (input) =>
            Effect.sync(() => {
              events.push(`provision:${input.ownership?.generation ?? "missing"}`);
              return {
                endpoint: {
                  httpBaseUrl: "https://managed.example.test/",
                  wsBaseUrl: "wss://managed.example.test/ws",
                  providerKind: "cloudflare_tunnel" as const,
                },
                runtime: {
                  providerKind: "cloudflare_tunnel" as const,
                  connectorToken: "connector-token",
                },
                endpointDisposition: "created" as const,
              };
            }),
          upsert: () => Effect.sync(() => events.push("upsert")),
          createCredential: () =>
            Effect.gen(function* () {
              events.push("credential");
              return yield* primary;
            }),
          revokeLink: () =>
            Effect.gen(function* () {
              events.push("revoke-link");
              return yield* new EnvironmentLinks.EnvironmentLinkRevokePersistenceError({
                userId: "user_123",
                environmentId: "env-link-test",
                cause: new Error("link cleanup unavailable"),
              });
            }),
          revokeOrphaned: () =>
            Effect.gen(function* () {
              events.push("revoke-credentials");
              return yield* new EnvironmentCredentials.EnvironmentCredentialRevokePersistenceError({
                environmentId: "env-link-test",
                cause: new Error("credential cleanup unavailable"),
              });
            }),
          deprovision: (input) =>
            Effect.gen(function* () {
              events.push(`deprovision:${input.ownership?.generation ?? "missing"}`);
              return yield* new ManagedEndpointProvider.ManagedEndpointDeprovisioningFailed({
                stage: "remove-allocation",
                userId: "user_123",
                environmentId: "env-link-test",
                cause: new Error("endpoint cleanup unavailable"),
              });
            }),
        }),
      ),
    );
  });

  it.effect("restores a pre-existing relink without deprovisioning its managed endpoint", () => {
    const previous: EnvironmentLinks.RelayLinkedEnvironmentRecord = {
      environmentId: "env-link-test" as RelayEnvironmentLinkProofPayload["environmentId"],
      label: "Previous Environment",
      environmentPublicKey: environmentKeyPair.publicKey.trim(),
      endpoint: {
        httpBaseUrl: "https://previous-managed.example.test/",
        wsBaseUrl: "wss://previous-managed.example.test/ws",
        providerKind: "cloudflare_tunnel",
      },
      managedTunnelsEnabled: true,
      linkedAt: "2026-01-01T00:00:00.000Z",
    };
    let persisted: EnvironmentLinks.RelayLinkedEnvironmentRecord | null = previous;
    let deprovisioned = 0;
    const primary = new EnvironmentCredentials.EnvironmentCredentialCreatePersistenceError({
      stage: "insert-credential",
      environmentId: "env-link-test",
      cause: new Error("database unavailable"),
    });

    return Effect.gen(function* () {
      const { request } = yield* makeRequestFor(true);
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const error = yield* Effect.flip(linker.link({ userId: "user_123", request }));

      expect(error).toBe(primary);
      expect(persisted).toEqual(previous);
      expect(deprovisioned).toBe(0);
    }).pipe(
      Effect.provide(
        testLayer({
          getLink: () => Effect.succeed(previous),
          provision: () =>
            Effect.succeed({
              endpoint: {
                httpBaseUrl: "https://previous-managed.example.test/",
                wsBaseUrl: "wss://previous-managed.example.test/ws",
                providerKind: "cloudflare_tunnel",
              },
              runtime: {
                providerKind: "cloudflare_tunnel",
                connectorToken: "connector-token",
              },
              endpointDisposition: "reused",
            } as ManagedEndpointProvider.ManagedEndpointProvisioningResult),
          upsert: (input) =>
            Effect.sync(() => {
              persisted = {
                environmentId: input.proof.environmentId,
                label: input.proof.descriptor.label,
                environmentPublicKey: input.proof.environmentPublicKey,
                endpoint: input.endpoint,
                managedTunnelsEnabled: true,
                linkedAt: "2026-01-02T00:00:00.000Z",
              };
            }),
          restoreLink: ({ record }) =>
            Effect.sync(() => {
              persisted = record;
            }).pipe(
              Effect.andThen(
                Effect.fail(
                  new EnvironmentLinks.EnvironmentLinkUpsertPersistenceError({
                    userId: "user_123",
                    environmentId: "env-link-test",
                    cause: new Error("restore acknowledgement unavailable"),
                  }),
                ),
              ),
            ),
          createCredential: () => Effect.fail(primary),
          revokeLink: () =>
            Effect.sync(() => {
              persisted = null;
              return true;
            }),
          deprovision: () =>
            Effect.sync(() => {
              deprovisioned++;
            }),
        }),
      ),
    );
  });

  it.effect("deprovisions an endpoint created by a failed relink", () => {
    const previous: EnvironmentLinks.RelayLinkedEnvironmentRecord = {
      environmentId: "env-link-test" as RelayEnvironmentLinkProofPayload["environmentId"],
      label: "Previous Unmanaged Environment",
      environmentPublicKey: environmentKeyPair.publicKey.trim(),
      endpoint: {
        httpBaseUrl: "https://previous.example.test/",
        wsBaseUrl: "wss://previous.example.test/ws",
        providerKind: "manual",
      },
      managedTunnelsEnabled: false,
      linkedAt: "2026-01-01T00:00:00.000Z",
    };
    let restored = 0;
    let deprovisioned = 0;
    const primary = new EnvironmentCredentials.EnvironmentCredentialCreatePersistenceError({
      stage: "insert-credential",
      environmentId: "env-link-test",
      cause: new Error("database unavailable"),
    });

    return Effect.gen(function* () {
      const { request } = yield* makeRequestFor(true);
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const error = yield* Effect.flip(linker.link({ userId: "user_123", request }));

      expect(error).toBe(primary);
      expect(restored).toBe(1);
      expect(deprovisioned).toBe(1);
    }).pipe(
      Effect.provide(
        testLayer({
          getLink: () => Effect.succeed(previous),
          provision: () =>
            Effect.succeed({
              endpoint: {
                httpBaseUrl: "https://new-managed.example.test/",
                wsBaseUrl: "wss://new-managed.example.test/ws",
                providerKind: "cloudflare_tunnel",
              },
              runtime: { providerKind: "cloudflare_tunnel", connectorToken: "connector-token" },
              endpointDisposition: "created",
            }),
          upsert: () => Effect.void,
          restoreLink: () => Effect.sync(() => restored++),
          createCredential: () => Effect.fail(primary),
          deprovision: () => Effect.sync(() => deprovisioned++),
        }),
      ),
    );
  });

  it.effect("compensates interruption after link upsert before response bookkeeping", () =>
    Effect.gen(function* () {
      const upserted = yield* Deferred.make<void>();
      const continueUpsert = yield* Deferred.make<void>();
      let linked = false;
      let credentialsActive = false;
      let endpointActive = true;
      const layer = testLayer({
        provision: () =>
          Effect.succeed({
            endpoint: {
              httpBaseUrl: "https://managed.example.test/",
              wsBaseUrl: "wss://managed.example.test/ws",
              providerKind: "cloudflare_tunnel",
            },
            runtime: { providerKind: "cloudflare_tunnel", connectorToken: "connector-token" },
            endpointDisposition: "created",
          }),
        upsert: () =>
          Effect.gen(function* () {
            linked = true;
            yield* Deferred.succeed(upserted, undefined);
            yield* Deferred.await(continueUpsert);
          }),
        createCredential: () =>
          Effect.sync(() => {
            credentialsActive = true;
            return "coddystudioenv_credential_secret";
          }),
        revokeLink: () =>
          Effect.sync(() => {
            linked = false;
            return true;
          }),
        revokeOrphaned: () =>
          Effect.sync(() => {
            credentialsActive = false;
            return true;
          }),
        deprovision: () =>
          Effect.sync(() => {
            endpointActive = false;
          }),
      });
      const { request } = yield* makeRequestFor(true).pipe(Effect.provide(layer));
      const run = Effect.gen(function* () {
        const linker = yield* EnvironmentLinker.EnvironmentLinker;
        return yield* linker.link({ userId: "user_123", request });
      }).pipe(Effect.provide(layer));

      const fiber = yield* Effect.forkChild(run);
      yield* Deferred.await(upserted);
      const interrupted = yield* Effect.forkChild(Fiber.interrupt(fiber));
      yield* Deferred.succeed(continueUpsert, undefined);
      yield* Fiber.join(interrupted);

      expect(linked).toBe(false);
      expect(credentialsActive).toBe(false);
      expect(endpointActive).toBe(false);
    }),
  );

  it.effect("restores the prior same-key credential when relink is interrupted", () =>
    Effect.gen(function* () {
      const credentialCreated = yield* Deferred.make<void>();
      let priorCredentialActive = true;
      let replacementCredentialActive = false;
      const previous: EnvironmentLinks.RelayLinkedEnvironmentRecord = {
        environmentId: "env-link-test" as RelayEnvironmentLinkProofPayload["environmentId"],
        label: "Previous Environment",
        environmentPublicKey: environmentKeyPair.publicKey.trim(),
        endpoint: {
          httpBaseUrl: "https://managed.example.test/",
          wsBaseUrl: "wss://managed.example.test/ws",
          providerKind: "cloudflare_tunnel",
        },
        managedTunnelsEnabled: true,
        linkedAt: "2026-01-01T00:00:00.000Z",
      };
      const rotation = {
        token: "coddystudioenv_undisclosed_replacement",
        credentialId: "replacement-credential",
        previousCredentialId: "prior-credential",
        environmentId: "env-link-test",
        environmentPublicKey: previous.environmentPublicKey,
      } as const;
      const layer = testLayer({
        getLink: () => Effect.succeed(previous),
        provision: () =>
          Effect.succeed({
            endpoint: previous.endpoint,
            runtime: { providerKind: "cloudflare_tunnel", connectorToken: "connector-token" },
            endpointDisposition: "reused",
          }),
        createCredential: () =>
          Effect.sync(() => {
            priorCredentialActive = false;
            replacementCredentialActive = true;
          }).pipe(
            Effect.andThen(Deferred.succeed(credentialCreated, undefined)),
            Effect.as(rotation.token),
          ),
        rotateCredential: () =>
          Effect.sync(() => {
            priorCredentialActive = false;
            replacementCredentialActive = true;
          }).pipe(
            Effect.andThen(Deferred.succeed(credentialCreated, undefined)),
            Effect.as(rotation),
          ),
        rollbackCredential: (received) =>
          Effect.sync(() => {
            expect(received).toBe(rotation);
            replacementCredentialActive = false;
            priorCredentialActive = true;
          }).pipe(
            Effect.andThen(
              Effect.fail(
                new EnvironmentCredentials.EnvironmentCredentialCreatePersistenceError({
                  stage: "rollback-credential",
                  environmentId: rotation.environmentId,
                  credentialId: rotation.credentialId,
                  cause: new Error("rollback acknowledgement unavailable"),
                }),
              ),
            ),
          ),
      });
      const { request } = yield* makeRequestFor(true).pipe(Effect.provide(layer));
      const run = Effect.gen(function* () {
        const linker = yield* EnvironmentLinker.EnvironmentLinker;
        return yield* linker.link({ userId: "user_123", request });
      }).pipe(Effect.provide(layer));

      const fiber = yield* Effect.forkChild(run);
      yield* Deferred.await(credentialCreated);
      yield* Fiber.interrupt(fiber);

      expect(priorCredentialActive).toBe(true);
      expect(replacementCredentialActive).toBe(false);
    }),
  );

  it.effect("retains a coherent relink when credential rollback fails before mutation", () =>
    Effect.gen(function* () {
      const credentialCreated = yield* Deferred.make<void>();
      let priorCredentialActive = true;
      let replacementCredentialActive = false;
      let restoredLinks = 0;
      let orphanCleanup = 0;
      let endpointActive = true;
      const previous: EnvironmentLinks.RelayLinkedEnvironmentRecord = {
        environmentId: "env-link-test" as RelayEnvironmentLinkProofPayload["environmentId"],
        label: "Previous Environment",
        environmentPublicKey: environmentKeyPair.publicKey.trim(),
        endpoint: {
          httpBaseUrl: "https://previous.example.test/",
          wsBaseUrl: "wss://previous.example.test/ws",
          providerKind: "manual",
        },
        managedTunnelsEnabled: false,
        linkedAt: "2026-01-01T00:00:00.000Z",
      };
      let persisted = previous;
      const replacementEndpoint = {
        httpBaseUrl: "https://managed.example.test/",
        wsBaseUrl: "wss://managed.example.test/ws",
        providerKind: "cloudflare_tunnel" as const,
      };
      const rotation = {
        token: "coddystudioenv_undisclosed_replacement",
        credentialId: "replacement-credential",
        previousCredentialId: "prior-credential",
        environmentId: "env-link-test",
        environmentPublicKey: previous.environmentPublicKey,
      } as const;
      const rollbackError = new EnvironmentCredentials.EnvironmentCredentialCreatePersistenceError({
        stage: "rollback-credential",
        environmentId: rotation.environmentId,
        credentialId: rotation.credentialId,
        cause: new Error("rollback unavailable before mutation"),
      });
      const layer = testLayer({
        getLink: () => Effect.succeed(previous),
        provision: () =>
          Effect.succeed({
            endpoint: replacementEndpoint,
            runtime: { providerKind: "cloudflare_tunnel", connectorToken: "connector-token" },
            endpointDisposition: "created",
          }),
        upsert: (input) =>
          Effect.sync(() => {
            persisted = {
              environmentId: input.proof.environmentId,
              label: input.proof.descriptor.label,
              environmentPublicKey: input.proof.environmentPublicKey,
              endpoint: input.endpoint,
              managedTunnelsEnabled: true,
              linkedAt: "2026-01-02T00:00:00.000Z",
            };
          }),
        rotateCredential: () =>
          Effect.sync(() => {
            priorCredentialActive = false;
            replacementCredentialActive = true;
          }).pipe(
            Effect.andThen(Deferred.succeed(credentialCreated, undefined)),
            Effect.as(rotation),
          ),
        rollbackCredential: () => Effect.fail(rollbackError),
        restoreLink: ({ record }) =>
          Effect.sync(() => {
            restoredLinks++;
            persisted = record;
          }),
        revokeOrphaned: () =>
          Effect.sync(() => {
            orphanCleanup++;
            replacementCredentialActive = false;
            return true;
          }),
        deprovision: () =>
          Effect.sync(() => {
            endpointActive = false;
          }),
      });
      const { request } = yield* makeRequestFor(true).pipe(Effect.provide(layer));
      const run = Effect.gen(function* () {
        const linker = yield* EnvironmentLinker.EnvironmentLinker;
        return yield* linker.link({ userId: "user_123", request });
      }).pipe(Effect.provide(layer));

      const fiber = yield* Effect.forkChild(run);
      yield* Deferred.await(credentialCreated);
      yield* Fiber.interrupt(fiber);

      expect(persisted.endpoint).toEqual(replacementEndpoint);
      expect(priorCredentialActive).toBe(false);
      expect(replacementCredentialActive).toBe(true);
      expect(restoredLinks).toBe(0);
      expect(orphanCleanup).toBe(0);
      expect(endpointActive).toBe(true);
    }),
  );

  it.effect("stops compensation when the replacement credential is no longer current", () =>
    Effect.gen(function* () {
      const credentialCreated = yield* Deferred.make<void>();
      let destructiveCleanup = 0;
      const rotation = {
        token: "coddystudioenv_undisclosed_replacement",
        credentialId: "replacement-credential",
        previousCredentialId: null,
        environmentId: "env-link-test",
        environmentPublicKey: environmentKeyPair.publicKey.trim(),
      } as const;
      const layer = testLayer({
        rotateCredential: () =>
          Deferred.succeed(credentialCreated, undefined).pipe(Effect.as(rotation)),
        rollbackCredential: () => Effect.succeed(false),
        revokeLink: () =>
          Effect.sync(() => {
            destructiveCleanup++;
            return true;
          }),
        revokeOrphaned: () =>
          Effect.sync(() => {
            destructiveCleanup++;
            return true;
          }),
        deprovision: () => Effect.sync(() => destructiveCleanup++),
      });
      const { request } = yield* makeRequestFor(true).pipe(Effect.provide(layer));
      const run = Effect.gen(function* () {
        const linker = yield* EnvironmentLinker.EnvironmentLinker;
        return yield* linker.link({ userId: "user_123", request });
      }).pipe(Effect.provide(layer));

      const fiber = yield* Effect.forkChild(run);
      yield* Deferred.await(credentialCreated);
      yield* Fiber.interrupt(fiber);

      expect(destructiveCleanup).toBe(0);
    }),
  );

  it.effect("compensates a committed link when the upsert acknowledgement fails", () => {
    let linked = false;
    let endpointActive = true;
    let credentialAttempts = 0;
    const primary = new EnvironmentLinks.EnvironmentLinkUpsertPersistenceError({
      userId: "user_123",
      environmentId: "env-link-test",
      cause: new Error("upsert acknowledgement unavailable"),
    });

    return Effect.gen(function* () {
      const { request } = yield* makeRequestFor(true);
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const error = yield* Effect.flip(linker.link({ userId: "user_123", request }));
      expect(error).toBe(primary);
      expect(linked).toBe(false);
      expect(endpointActive).toBe(false);
      expect(credentialAttempts).toBe(0);
    }).pipe(
      Effect.provide(
        testLayer({
          provision: () =>
            Effect.succeed({
              endpoint: {
                httpBaseUrl: "https://managed.example.test/",
                wsBaseUrl: "wss://managed.example.test/ws",
                providerKind: "cloudflare_tunnel",
              },
              runtime: { providerKind: "cloudflare_tunnel", connectorToken: "connector-token" },
              endpointDisposition: "created",
            }),
          upsert: () =>
            Effect.sync(() => {
              linked = true;
            }).pipe(Effect.andThen(Effect.fail(primary))),
          revokeLink: () =>
            Effect.sync(() => {
              linked = false;
              return true;
            }),
          createCredential: () =>
            Effect.sync(() => {
              credentialAttempts++;
              return "unexpected";
            }),
          deprovision: () =>
            Effect.sync(() => {
              endpointActive = false;
            }),
        }),
      ),
    );
  });

  it.effect("rejects a tampered compact proof before persistence", () => {
    let persisted = false;
    return Effect.gen(function* () {
      const { request } = yield* makeRequest;
      const segments = request.proof.split(".");
      const signature = segments[2]!;
      segments[2] = `${signature.startsWith("A") ? "B" : "A"}${signature.slice(1)}`;
      const tampered = { ...request, proof: segments.join(".") };
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const result = yield* Effect.result(linker.link({ userId: "user_123", request: tampered }));
      expect(Result.isFailure(result)).toBe(true);
      if (Result.isFailure(result)) {
        expect(isEnvironmentLinkProofInvalid(result.failure)).toBe(true);
        if (isEnvironmentLinkProofInvalid(result.failure)) {
          expect(result.failure).toMatchObject({
            userId: "user_123",
            environmentId: "env-link-test",
            reason: "invalid_signature_or_scope",
            stage: "verify_proof",
            cause: { _tag: "RelayJwtError" },
          });
        }
      }
      expect(persisted).toBe(false);
    }).pipe(
      Effect.provide(
        testLayer({
          upsert: () =>
            Effect.sync(() => {
              persisted = true;
            }),
        }),
      ),
    );
  });

  it.effect("rejects replayed JWT ids", () =>
    Effect.gen(function* () {
      const { request } = yield* makeRequest;
      const linker = yield* EnvironmentLinker.EnvironmentLinker;
      const result = yield* Effect.result(linker.link({ userId: "user_123", request }));
      expect(Result.isFailure(result)).toBe(true);
      if (Result.isFailure(result)) {
        expect(isEnvironmentLinkProofInvalid(result.failure)).toBe(true);
        if (isEnvironmentLinkProofInvalid(result.failure)) {
          expect(result.failure).toMatchObject({
            userId: "user_123",
            environmentId: "env-link-test",
            reason: "replayed_nonce",
            stage: "consume_proof_nonce",
          });
        }
      }
    }).pipe(Effect.provide(testLayer({ consume: () => Effect.succeed(false) }))),
  );
});
