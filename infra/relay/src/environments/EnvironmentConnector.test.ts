// @effect-diagnostics-next-line nodeBuiltinImport:off - Test fixtures generate key pairs synchronously.
import * as NodeCrypto from "node:crypto";
import * as NodeCryptoLayer from "@effect/platform-node/NodeCrypto";

import {
  RelayCloudEnvironmentHealthRequest,
  RelayCloudMintCredentialRequest,
  RelayCloudEnvironmentHealthProofPayload,
  RelayCloudMintCredentialProofPayload,
  RelayEnvironmentHealthResponse,
  RelayEnvironmentHealthResponseProofPayload,
  RelayEnvironmentMintResponse,
  RelayEnvironmentMintResponseProofPayload,
} from "@bibcode/contracts/relay";
import { EnvironmentHttpBadRequestError } from "@bibcode/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as DateTime from "effect/DateTime";
import * as Crypto from "effect/Crypto";
import { RELAY_HEALTH_RESPONSE_TYP, RELAY_MINT_RESPONSE_TYP } from "@bibcode/shared/relayJwt";
import { makeTestExecutionEnvironmentCapabilities } from "@bibcode/shared/testSupport";
import * as Deferred from "effect/Deferred";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as PlatformError from "effect/PlatformError";
import * as Redacted from "effect/Redacted";
import * as Ref from "effect/Ref";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";
import * as TestClock from "effect/testing/TestClock";
import * as Tracer from "effect/Tracer";
import { HttpClient, HttpClientRequest, HttpClientResponse } from "effect/http";
import * as HttpClientError from "effect/http/HttpClientError";

import * as EnvironmentLinks from "./EnvironmentLinks.ts";
import * as RelayConfiguration from "../Config.ts";
import * as EnvironmentConnector from "./EnvironmentConnector.ts";
import * as ManagedEndpointAllocations from "./ManagedEndpointAllocations.ts";

const cloudKeyPair = NodeCrypto.generateKeyPairSync("ed25519", {
  privateKeyEncoding: { format: "pem", type: "pkcs8" },
  publicKeyEncoding: { format: "pem", type: "spki" },
});

const environmentKeyPair = NodeCrypto.generateKeyPairSync("ed25519", {
  privateKeyEncoding: { format: "pem", type: "pkcs8" },
  publicKeyEncoding: { format: "pem", type: "spki" },
});

const otherEnvironmentKeyPair = NodeCrypto.generateKeyPairSync("ed25519", {
  privateKeyEncoding: { format: "pem", type: "pkcs8" },
  publicKeyEncoding: { format: "pem", type: "spki" },
});

const decodeHealthRequestBody = Schema.decodeUnknownSync(
  Schema.fromJsonString(RelayCloudEnvironmentHealthRequest),
);
const decodeMintRequestBody = Schema.decodeUnknownSync(
  Schema.fromJsonString(RelayCloudMintCredentialRequest),
);
const isEnvironmentConnectNotAuthorized = Schema.is(
  EnvironmentConnector.EnvironmentConnectNotAuthorized,
);

function requestBodyText(request: HttpClientRequest.HttpClientRequest): string {
  return request.body._tag === "Uint8Array" ? new TextDecoder().decode(request.body.body) : "{}";
}

const settings = RelayConfiguration.RelayConfiguration.of({
  relayIssuer: "https://relay.example.test",
  clerkSecretKey: Redacted.make("clerk-secret"),
  clerkPublishableKey: "pk_test_test",
  clerkJwtAudience: "bibcode-relay",
  cloudMintPrivateKey: Redacted.make(cloudKeyPair.privateKey),
  cloudMintPublicKey: cloudKeyPair.publicKey,
  managedEndpointBaseDomain: "example.test",
  managedEndpointNamespace: undefined,
});

function signTestJwt(payload: object, typ: string, privateKey: string): string {
  const header = Buffer.from(JSON.stringify({ alg: "EdDSA", typ })).toString("base64url");
  const encodedPayload = Buffer.from(JSON.stringify(payload)).toString("base64url");
  const input = `${header}.${encodedPayload}`;
  return `${input}.${Buffer.from(NodeCrypto.sign(null, Buffer.from(input), privateKey)).toString("base64url")}`;
}

function decodeRequestProof<T>(proof: string): T {
  const payload = proof.split(".")[1];
  if (!payload) throw new Error("Missing JWT payload.");
  return JSON.parse(Buffer.from(payload, "base64url").toString("utf8")) as T;
}

function signMintResponse(
  request: RelayCloudMintCredentialRequest,
  overrides: Partial<RelayEnvironmentMintResponseProofPayload> = {},
  privateKey = environmentKeyPair.privateKey,
): RelayEnvironmentMintResponse {
  const requestProof = decodeRequestProof<RelayCloudMintCredentialProofPayload>(request.proof);
  const payload = {
    iss: `bibcode-env:${requestProof.environmentId}`,
    aud: "https://relay.example.test",
    sub: requestProof.environmentId,
    jti: "mint-response-jti",
    iat: requestProof.iat,
    exp: requestProof.exp,
    environmentId: requestProof.environmentId,
    clientProofKeyThumbprint: requestProof.clientProofKeyThumbprint,
    requestNonce: requestProof.nonce,
    credential: "pairing_credential",
    ...overrides,
  } satisfies RelayEnvironmentMintResponseProofPayload;
  return {
    credential: payload.credential,
    expiresAt: DateTime.formatIso(DateTime.makeUnsafe(payload.exp * 1_000)),
    proof: signTestJwt(payload, RELAY_MINT_RESPONSE_TYP, privateKey),
  };
}

function signHealthResponse(
  request: RelayCloudEnvironmentHealthRequest,
  privateKey = environmentKeyPair.privateKey,
  overrides: Partial<RelayEnvironmentHealthResponse> = {},
  payloadOverrides: Partial<RelayEnvironmentHealthResponseProofPayload> = {},
): RelayEnvironmentHealthResponse {
  const requestProof = decodeRequestProof<RelayCloudEnvironmentHealthProofPayload>(request.proof);
  const payload = {
    iss: `bibcode-env:${requestProof.environmentId}`,
    aud: "https://relay.example.test",
    sub: requestProof.environmentId,
    jti: "health-response-jti",
    iat: requestProof.iat,
    exp: requestProof.exp,
    environmentId: requestProof.environmentId,
    requestNonce: requestProof.nonce,
    status: "online",
    descriptor: {
      environmentId: requestProof.environmentId,
      label: "Connector Test Environment",
      platform: { os: "darwin", arch: "arm64" },
      serverVersion: "0.0.0-test",
      storageInstanceId: "019c18d0-26b2-7a35-9e06-8568e640f44f",
      bootId: null,
      remoteUpdateSupport: null,
      remoteProtocolVersion: 1,
      minCompatibleRemoteProtocol: 1,
      capabilities: makeTestExecutionEnvironmentCapabilities({ repositoryIdentity: true }),
    },
    checkedAt: DateTime.formatIso(DateTime.makeUnsafe(requestProof.iat * 1_000)),
    ...payloadOverrides,
  } satisfies RelayEnvironmentHealthResponseProofPayload;
  return {
    environmentId: payload.environmentId,
    status: "online",
    descriptor: payload.descriptor,
    checkedAt: payload.checkedAt,
    proof: signTestJwt(payload, RELAY_HEALTH_RESPONSE_TYP, privateKey),
    ...overrides,
  };
}

function connectorTestLayer(
  execute: (
    request: HttpClientRequest.HttpClientRequest,
  ) => Effect.Effect<HttpClientResponse.HttpClientResponse, HttpClientError.HttpClientError>,
  options?: {
    readonly links?: EnvironmentLinks.EnvironmentLinks["Service"];
    readonly allocations?: ManagedEndpointAllocations.ManagedEndpointAllocations["Service"];
    readonly settings?: RelayConfiguration.RelayConfiguration["Service"];
    readonly crypto?: Crypto.Crypto;
  },
) {
  return EnvironmentConnector.layer.pipe(
    Layer.provide(
      options?.crypto === undefined
        ? NodeCryptoLayer.layer
        : Layer.succeed(Crypto.Crypto, options.crypto),
    ),
    Layer.provide(Layer.succeed(EnvironmentLinks.EnvironmentLinks, options?.links ?? makeLinks())),
    Layer.provide(
      Layer.succeed(
        ManagedEndpointAllocations.ManagedEndpointAllocations,
        options?.allocations ?? makeAllocations(),
      ),
    ),
    Layer.provide(RelayConfiguration.layer(options?.settings ?? settings)),
    Layer.provide(Layer.succeed(HttpClient.HttpClient, HttpClient.make(execute))),
  );
}

function makeAllocations(
  allocation: ManagedEndpointAllocations.ManagedEndpointAllocation | null = {
    userId: "user_123",
    environmentId: "env-connector-test",
    hostname: "env.example.test",
    tunnelId: "tunnel-id",
    tunnelName: "tunnel-name",
    dnsRecordId: "dns-record-id",
    readyAt: "2026-05-25T00:00:00.000Z",
  },
): ManagedEndpointAllocations.ManagedEndpointAllocations["Service"] {
  return {
    withOperation: (input, use) =>
      use({ ...input, generation: 1, ownerToken: "test-operation-owner" }),
    acquireOperation: () => Effect.die("unused"),
    releaseOperation: () => Effect.die("unused"),
    renewOperation: () => Effect.die("unused"),
    claimForOperation: () => Effect.die("unused"),
    get: () => Effect.succeed(allocation),
    reserve: () => Effect.die("unused"),
    recordTunnel: () => Effect.die("unused"),
    recordDns: () => Effect.die("unused"),
    markReady: () => Effect.die("unused"),
    remove: () => Effect.die("unused"),
  };
}

function makeLinks(
  overrides: Partial<EnvironmentLinks.RelayLinkedEnvironmentRecord> = {},
): EnvironmentLinks.EnvironmentLinks["Service"] {
  return {
    upsert: () => Effect.void,
    listUsersForEnvironment: () => Effect.succeed([]),
    listPublicKeysForEnvironment: () => Effect.succeed([environmentKeyPair.publicKey]),
    listForUser: () => Effect.succeed([]),
    getForUser: () =>
      Effect.succeed({
        environmentId: "env-connector-test" as never,
        label: "Connector Test Environment",
        endpoint: {
          httpBaseUrl: "https://env.example.test/",
          wsBaseUrl: "wss://env.example.test/ws",
          providerKind: "cloudflare_tunnel",
        },
        linkedAt: "2026-05-25T00:00:00.000Z",
        environmentPublicKey: environmentKeyPair.publicKey,
        managedTunnelsEnabled: true,
        ...overrides,
      }),
    restoreForUser: () => Effect.void,
    revokeForUser: () => Effect.succeed(false),
  };
}

describe("EnvironmentConnector", () => {
  it("formats all authorization and connector errors without exposing causes", () => {
    const reasonMessages = {
      client_proof_key_thumbprint_missing: "the client proof key thumbprint is missing",
      environment_link_not_found: "no active environment link was found",
      endpoint_provider_not_managed: "the linked endpoint is not relay-managed",
      managed_endpoint_allocation_not_found: "no managed endpoint allocation was found",
      managed_endpoint_base_domain_not_configured:
        "the managed endpoint base domain is not configured",
      managed_endpoint_allocation_not_ready: "the managed endpoint allocation is incomplete",
      managed_endpoint_hostname_invalid: "the managed endpoint hostname is invalid",
      managed_endpoint_mismatch: "the linked endpoint does not match its managed allocation",
    } as const;
    for (const [reason, text] of Object.entries(reasonMessages)) {
      const error = new EnvironmentConnector.EnvironmentConnectNotAuthorized({
        environmentId: "env-1",
        operation: "connect",
        reason: reason as keyof typeof reasonMessages,
      });
      expect(error.message).toBe(`Environment 'env-1' is not authorized for connect: ${text}`);
    }
    const cause = new Error("secret-cause");
    const request = new EnvironmentConnector.EnvironmentMintRequestFailed({
      environmentId: "env-1",
      operation: "status",
      cause,
    });
    const timeout = new EnvironmentConnector.EnvironmentMintRequestTimedOut({
      environmentId: "env-1",
      timeoutMs: 10_000,
    });
    const response = new EnvironmentConnector.EnvironmentMintResponseInvalid({
      environmentId: "env-1",
      operation: "connect",
    });
    expect(request.message).toBe("Environment 'env-1' status request failed");
    expect(timeout.message).toBe("Environment 'env-1' mint request timed out after 10000ms");
    expect(response.message).toBe("Environment 'env-1' returned an invalid connect response");
    expect(JSON.stringify(request)).not.toContain("secret-cause");
  });

  it.effect(
    "fails closed for missing links, allocations, configuration, and endpoint mismatch",
    () => {
      const execute = () => Effect.die("HTTP must not run");
      const key = { userId: "user_123", environmentId: "env-connector-test" };
      const completeAllocation = {
        userId: key.userId,
        environmentId: key.environmentId,
        hostname: "env.example.test",
        tunnelId: "tunnel-id",
        tunnelName: "tunnel-name",
        dnsRecordId: "dns-record-id",
        readyAt: "2026-05-25T00:00:00.000Z",
      } satisfies ManagedEndpointAllocations.ManagedEndpointAllocation;
      const runStatus = (options: Parameters<typeof connectorTestLayer>[1]) =>
        Effect.gen(function* () {
          const connector = yield* EnvironmentConnector.EnvironmentConnector;
          return yield* Effect.flip(connector.status(key));
        }).pipe(Effect.provide(connectorTestLayer(execute, options)));

      return Effect.gen(function* () {
        const noLink = yield* runStatus({
          links: { ...makeLinks(), getForUser: () => Effect.succeed(null) },
        });
        const noAllocation = yield* runStatus({ allocations: makeAllocations(null) });
        const noDomain = yield* runStatus({
          settings: { ...settings, managedEndpointBaseDomain: undefined },
        });
        const invalidHostname = yield* runStatus({
          allocations: makeAllocations({ ...completeAllocation, hostname: "outside.test" }),
        });
        const mismatch = yield* runStatus({
          links: makeLinks({
            endpoint: {
              httpBaseUrl: "https://different.example.test/",
              wsBaseUrl: "wss://different.example.test/ws",
              providerKind: "cloudflare_tunnel",
            },
          }),
        });

        const reasons = [noLink, noAllocation, noDomain, invalidHostname, mismatch].map((error) => {
          expect(isEnvironmentConnectNotAuthorized(error)).toBe(true);
          return isEnvironmentConnectNotAuthorized(error) ? error.reason : "unexpected";
        });
        expect(reasons).toEqual([
          "environment_link_not_found",
          "managed_endpoint_allocation_not_found",
          "managed_endpoint_base_domain_not_configured",
          "managed_endpoint_hostname_invalid",
          "managed_endpoint_mismatch",
        ]);
      });
    },
  );

  it.effect("rejects blank proof thumbprints and missing connect links before HTTP", () => {
    const execute = () => Effect.die("HTTP must not run");
    const connect = (clientProofKeyThumbprint: string, links = makeLinks()) =>
      Effect.gen(function* () {
        const connector = yield* EnvironmentConnector.EnvironmentConnector;
        return yield* Effect.flip(
          connector.connect({
            userId: "user_123",
            environmentId: "env-connector-test",
            clientProofKeyThumbprint,
          }),
        );
      }).pipe(Effect.provide(connectorTestLayer(execute, { links })));

    return Effect.gen(function* () {
      const blank = yield* connect("   ");
      const missing = yield* connect("proof-thumbprint", {
        ...makeLinks(),
        getForUser: () => Effect.succeed(null),
      });
      expect(isEnvironmentConnectNotAuthorized(blank)).toBe(true);
      expect(isEnvironmentConnectNotAuthorized(missing)).toBe(true);
      if (isEnvironmentConnectNotAuthorized(blank) && isEnvironmentConnectNotAuthorized(missing)) {
        expect(blank.reason).toBe("client_proof_key_thumbprint_missing");
        expect(missing.reason).toBe("environment_link_not_found");
      }
    });
  });

  it.effect("classifies typed HTTP, transport, and schema health failures", () => {
    const transport = new HttpClientError.HttpClientError({
      reason: new HttpClientError.TransportError({
        request: HttpClientRequest.get("https://env.example.test/health"),
        description: "transport down",
      }),
    });
    const executions = [
      (request: HttpClientRequest.HttpClientRequest) =>
        Effect.succeed(
          HttpClientResponse.fromWeb(
            request,
            Response.json(
              new EnvironmentHttpBadRequestError({ message: "bad environment request" }),
              {
                status: 400,
              },
            ),
          ),
        ),
      () => Effect.fail(transport),
      (request: HttpClientRequest.HttpClientRequest) =>
        Effect.succeed(HttpClientResponse.fromWeb(request, Response.json({ malformed: true }))),
    ] as const;
    const run = (execute: (typeof executions)[number]) => {
      return Effect.gen(function* () {
        const connector = yield* EnvironmentConnector.EnvironmentConnector;
        return yield* connector.status({
          userId: "user_123",
          environmentId: "env-connector-test",
        });
      }).pipe(Effect.provide(connectorTestLayer(execute)), Effect.withTracerEnabled(false));
    };

    return Effect.gen(function* () {
      const results = yield* Effect.forEach(executions, run);
      expect(results.map((result) => result.status)).toEqual(["offline", "offline", "offline"]);
      expect(results[0]?.error).toContain("bad environment request");
      expect(results.slice(1).every((result) => result.error !== undefined)).toBe(true);
      expect(results.every((result) => typeof result.traceId === "string")).toBe(true);
    });
  });
  it.effect("loads the environment link and managed allocation concurrently", () =>
    Effect.gen(function* () {
      const started = yield* Ref.make(0);
      const bothStarted = yield* Deferred.make<void>();
      const release = yield* Deferred.make<void>();
      const waitForPeer = Effect.gen(function* () {
        const count = yield* Ref.updateAndGet(started, (value) => value + 1);
        if (count === 2) {
          yield* Deferred.succeed(bothStarted, undefined);
        }
        yield* Deferred.await(release);
      });
      const links = makeLinks();
      const allocations = makeAllocations();
      const execute = (request: HttpClientRequest.HttpClientRequest) =>
        Effect.sync(() => {
          const healthRequest = decodeHealthRequestBody(requestBodyText(request));
          return HttpClientResponse.fromWeb(
            request,
            Response.json(signHealthResponse(healthRequest), { status: 200 }),
          );
        });
      const status = Effect.gen(function* () {
        const connector = yield* EnvironmentConnector.EnvironmentConnector;
        return yield* connector.status({
          userId: "user_123",
          environmentId: "env-connector-test" as never,
        });
      }).pipe(
        Effect.provide(
          connectorTestLayer(execute, {
            links: {
              ...links,
              getForUser: (input) => waitForPeer.pipe(Effect.andThen(links.getForUser(input))),
            },
            allocations: {
              ...allocations,
              get: (input) => waitForPeer.pipe(Effect.andThen(allocations.get(input))),
            },
          }),
        ),
      );

      const fiber = yield* Effect.forkChild(status);
      yield* Deferred.await(bothStarted);
      yield* Deferred.succeed(release, undefined);
      yield* Fiber.join(fiber);

      expect(yield* Ref.get(started)).toBe(2);
    }),
  );

  it.effect("checks linked environment health through the managed endpoint", () => {
    const seenUrls: Array<string> = [];
    const seenProofs: Array<RelayCloudEnvironmentHealthProofPayload> = [];
    const execute = (request: HttpClientRequest.HttpClientRequest) =>
      Effect.sync(() => {
        const healthRequest = decodeHealthRequestBody(requestBodyText(request));
        seenUrls.push(request.url);
        seenProofs.push(decodeRequestProof(healthRequest.proof));
        return HttpClientResponse.fromWeb(
          request,
          Response.json(signHealthResponse(healthRequest), { status: 200 }),
        );
      });

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const result = yield* connector.status({
        userId: "user_123",
        environmentId: "env-connector-test",
      });

      expect(seenUrls).toEqual(["https://env.example.test/api/bibcode-connect/health"]);
      expect(seenProofs[0]).toMatchObject({
        iss: "https://relay.example.test",
        aud: "bibcode-env:env-connector-test",
        sub: "user_123",
        environmentId: "env-connector-test",
        scope: ["environment:status"],
      });
      expect(result).toMatchObject({
        environmentId: "env-connector-test",
        status: "online",
        descriptor: {
          environmentId: "env-connector-test",
          label: "Connector Test Environment",
        },
      });
    }).pipe(Effect.provide(connectorTestLayer(execute)));
  });

  it.effect("rejects manual endpoints before sending a health request", () => {
    let requestCount = 0;
    const execute = () =>
      Effect.sync(() => {
        requestCount += 1;
        throw new Error("unexpected request");
      });

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const result = yield* Effect.result(
        connector.status({
          userId: "user_123",
          environmentId: "env-connector-test",
        }),
      );

      expect(Result.isFailure(result)).toBe(true);
      if (Result.isFailure(result)) {
        expect(isEnvironmentConnectNotAuthorized(result.failure)).toBe(true);
        if (isEnvironmentConnectNotAuthorized(result.failure)) {
          expect(result.failure).toMatchObject({
            operation: "status",
            reason: "endpoint_provider_not_managed",
          });
        }
      }
      expect(requestCount).toBe(0);
    }).pipe(
      Effect.provide(
        connectorTestLayer(execute, {
          links: makeLinks({
            endpoint: {
              httpBaseUrl: "https://127.0.0.1/",
              wsBaseUrl: "wss://127.0.0.1/ws",
              providerKind: "manual",
            },
          }),
        }),
      ),
    );
  });

  it.effect("rejects stale managed endpoints before sending a mint request", () => {
    let requestCount = 0;
    const spans: Array<Tracer.NativeSpan> = [];
    const tracer = Tracer.make({
      span: (options) => {
        const span = new Tracer.NativeSpan(options);
        spans.push(span);
        return span;
      },
    });
    const execute = () =>
      Effect.sync(() => {
        requestCount += 1;
        throw new Error("unexpected request");
      });

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const result = yield* Effect.result(
        connector.connect({
          userId: "user_123",
          environmentId: "env-connector-test",
          clientProofKeyThumbprint: "client-proof-key-thumbprint",
        }),
      );

      expect(Result.isFailure(result)).toBe(true);
      if (Result.isFailure(result)) {
        expect(isEnvironmentConnectNotAuthorized(result.failure)).toBe(true);
        if (isEnvironmentConnectNotAuthorized(result.failure)) {
          expect(result.failure).toMatchObject({
            operation: "connect",
            reason: "managed_endpoint_mismatch",
          });
        }
      }
      const resolutionSpan = spans.find(
        (span) => span.name === "relay.environment_connector.resolve_managed_endpoint",
      );
      expect(Object.fromEntries(resolutionSpan?.attributes ?? [])).toMatchObject({
        "relay.authorization.allocation_hostname": "env.example.test",
        "relay.authorization.allocation_has_ready_at": true,
        "relay.authorization.allocation_has_tunnel_id": true,
        "relay.authorization.allocation_has_dns_record_id": true,
        "relay.authorization.linked_http_base_url": "https://attacker.example.test/",
        "relay.authorization.linked_ws_base_url": "wss://attacker.example.test/ws",
        "relay.authorization.resolved_http_base_url": "https://env.example.test/",
        "relay.authorization.resolved_ws_base_url": "wss://env.example.test/ws",
      });
      expect(requestCount).toBe(0);
    }).pipe(
      Effect.provide(
        connectorTestLayer(execute, {
          links: makeLinks({
            endpoint: {
              httpBaseUrl: "https://attacker.example.test/",
              wsBaseUrl: "wss://attacker.example.test/ws",
              providerKind: "cloudflare_tunnel",
            },
          }),
        }),
      ),
      Effect.provideService(Tracer.Tracer, tracer),
    );
  });

  it.effect("rejects unready managed endpoint allocations before sending a request", () => {
    let requestCount = 0;
    const execute = () =>
      Effect.sync(() => {
        requestCount += 1;
        throw new Error("unexpected request");
      });

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const result = yield* Effect.result(
        connector.status({
          userId: "user_123",
          environmentId: "env-connector-test",
        }),
      );

      expect(Result.isFailure(result)).toBe(true);
      if (Result.isFailure(result)) {
        expect(isEnvironmentConnectNotAuthorized(result.failure)).toBe(true);
        if (isEnvironmentConnectNotAuthorized(result.failure)) {
          expect(result.failure).toMatchObject({
            operation: "status",
            reason: "managed_endpoint_allocation_not_ready",
          });
        }
      }
      expect(requestCount).toBe(0);
    }).pipe(
      Effect.provide(
        connectorTestLayer(execute, {
          allocations: makeAllocations({
            userId: "user_123",
            environmentId: "env-connector-test",
            hostname: "env.example.test",
            tunnelId: "tunnel-id",
            tunnelName: "tunnel-name",
            dnsRecordId: "dns-record-id",
            readyAt: null,
          }),
        }),
      ),
    );
  });

  it.effect("rejects signed health responses with stale checkedAt timestamps", () => {
    const execute = (request: HttpClientRequest.HttpClientRequest) =>
      Effect.sync(() => {
        const healthRequest = decodeHealthRequestBody(requestBodyText(request));
        return HttpClientResponse.fromWeb(
          request,
          Response.json(
            signHealthResponse(
              healthRequest,
              environmentKeyPair.privateKey,
              {},
              {
                checkedAt: "2026-05-24T00:00:00.000Z",
              },
            ),
            { status: 200 },
          ),
        );
      });

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const result = yield* Effect.exit(
        connector.status({
          userId: "user_123",
          environmentId: "env-connector-test",
        }),
      );

      expect(result._tag).toBe("Failure");
      if (result._tag === "Failure") {
        expect(result.cause.toString()).toContain("EnvironmentMintResponseInvalid");
      }
    }).pipe(Effect.provide(connectorTestLayer(execute)));
  });

  it.effect("reports offline status when the managed endpoint health request fails", () => {
    const execute = (request: HttpClientRequest.HttpClientRequest) =>
      Effect.succeed(
        HttpClientResponse.fromWeb(
          request,
          Response.json(
            {
              _tag: "EnvironmentHttpInternalServerError",
              message: "Environment is unavailable.",
            },
            { status: 500 },
          ),
        ),
      );

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const result = yield* connector.status({
        userId: "user_123",
        environmentId: "env-connector-test",
      });

      expect(result).toMatchObject({
        environmentId: "env-connector-test",
        status: "offline",
        error: "Managed endpoint health request failed: Environment is unavailable.",
        traceId: expect.any(String),
      });
    }).pipe(Effect.provide(connectorTestLayer(execute)));
  });

  it.effect("rejects health responses with a mismatched top-level environment id", () => {
    const execute = (request: HttpClientRequest.HttpClientRequest) =>
      Effect.sync(() => {
        const healthRequest = decodeHealthRequestBody(requestBodyText(request));
        return HttpClientResponse.fromWeb(
          request,
          Response.json(
            signHealthResponse(healthRequest, environmentKeyPair.privateKey, {
              environmentId: "other-env" as RelayEnvironmentHealthResponse["environmentId"],
            }),
            { status: 200 },
          ),
        );
      });

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const result = yield* Effect.exit(
        connector.status({
          userId: "user_123",
          environmentId: "env-connector-test",
        }),
      );

      expect(result._tag).toBe("Failure");
      if (result._tag === "Failure") {
        expect(result.cause.toString()).toContain("EnvironmentMintResponseInvalid");
      }
    }).pipe(Effect.provide(connectorTestLayer(execute)));
  });

  it.effect("rejects signed health responses with an invalid checkedAt timestamp", () => {
    const execute = (request: HttpClientRequest.HttpClientRequest) => {
      const body = decodeHealthRequestBody(requestBodyText(request));
      return Effect.succeed(
        HttpClientResponse.fromWeb(
          request,
          Response.json(signHealthResponse(body, undefined, {}, { checkedAt: "not-a-date" })),
        ),
      );
    };

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const error = yield* Effect.flip(
        connector.status({ userId: "user_123", environmentId: "env-connector-test" }),
      );
      expect(error).toMatchObject({
        _tag: "EnvironmentMintResponseInvalid",
        operation: "status",
      });
    }).pipe(Effect.provide(connectorTestLayer(execute)));
  });

  it.effect("rejects health responses with an unsigned top-level descriptor mutation", () => {
    const execute = (request: HttpClientRequest.HttpClientRequest) =>
      Effect.sync(() => {
        const healthRequest = decodeHealthRequestBody(requestBodyText(request));
        const response = signHealthResponse(healthRequest);
        return HttpClientResponse.fromWeb(
          request,
          Response.json(
            {
              ...response,
              descriptor: {
                ...response.descriptor,
                label: "Tampered Environment Label",
              },
            } satisfies RelayEnvironmentHealthResponse,
            { status: 200 },
          ),
        );
      });

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const result = yield* Effect.exit(
        connector.status({
          userId: "user_123",
          environmentId: "env-connector-test",
        }),
      );

      expect(result._tag).toBe("Failure");
      if (result._tag === "Failure") {
        expect(result.cause.toString()).toContain("EnvironmentMintResponseInvalid");
      }
    }).pipe(Effect.provide(connectorTestLayer(execute)));
  });

  it.effect("rejects health responses when the linked environment public key is malformed", () => {
    const execute = (request: HttpClientRequest.HttpClientRequest) =>
      Effect.sync(() => {
        const healthRequest = decodeHealthRequestBody(requestBodyText(request));
        return HttpClientResponse.fromWeb(
          request,
          Response.json(signHealthResponse(healthRequest), { status: 200 }),
        );
      });

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const result = yield* Effect.exit(
        connector.status({
          userId: "user_123",
          environmentId: "env-connector-test",
        }),
      );

      expect(result._tag).toBe("Failure");
      if (result._tag === "Failure") {
        expect(result.cause.toString()).toContain("EnvironmentMintResponseInvalid");
      }
    }).pipe(
      Effect.provide(
        connectorTestLayer(execute, {
          links: makeLinks({
            environmentPublicKey: "not a pem public key",
          }),
        }),
      ),
    );
  });

  it.effect("mints a one-time environment credential through the linked endpoint", () => {
    const seenUrls: Array<string> = [];
    const seenProofs: Array<RelayCloudMintCredentialProofPayload> = [];
    const execute = (request: HttpClientRequest.HttpClientRequest) =>
      Effect.sync(() => {
        const mintRequest = decodeMintRequestBody(requestBodyText(request));
        seenUrls.push(request.url);
        seenProofs.push(decodeRequestProof(mintRequest.proof));
        return HttpClientResponse.fromWeb(
          request,
          Response.json(signMintResponse(mintRequest), { status: 200 }),
        );
      });

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const result = yield* connector.connect({
        userId: "user_123",
        environmentId: "env-connector-test",
        clientProofKeyThumbprint: "client-proof-key-thumbprint",
      });

      expect(seenUrls).toEqual(["https://env.example.test/api/bibcode-connect/mint-credential"]);
      expect(seenProofs[0]).toMatchObject({
        iss: "https://relay.example.test",
        aud: "bibcode-env:env-connector-test",
        sub: "user_123",
        environmentId: "env-connector-test",
        clientProofKeyThumbprint: "client-proof-key-thumbprint",
        cnf: { jkt: "client-proof-key-thumbprint" },
        scope: ["environment:connect"],
      });
      expect(result).toMatchObject({
        environmentId: "env-connector-test",
        credential: "pairing_credential",
        endpoint: {
          httpBaseUrl: "https://env.example.test/",
          wsBaseUrl: "wss://env.example.test/ws",
        },
      });
    }).pipe(Effect.provide(connectorTestLayer(execute)));
  });

  it.effect("only accepts mint responses signed by the user's linked environment key", () => {
    const execute = (request: HttpClientRequest.HttpClientRequest) =>
      Effect.sync(() => {
        const mintRequest = decodeMintRequestBody(requestBodyText(request));
        return HttpClientResponse.fromWeb(
          request,
          Response.json(signMintResponse(mintRequest, {}, otherEnvironmentKeyPair.privateKey), {
            status: 200,
          }),
        );
      });

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const result = yield* Effect.exit(
        connector.connect({
          userId: "user_123",
          environmentId: "env-connector-test",
          clientProofKeyThumbprint: "client-proof-key-thumbprint",
        }),
      );

      expect(result._tag).toBe("Failure");
      if (result._tag === "Failure") {
        expect(result.cause.toString()).toContain("EnvironmentMintResponseInvalid");
      }
    }).pipe(Effect.provide(connectorTestLayer(execute)));
  });

  it.effect("rejects mint responses when the linked environment public key is malformed", () => {
    const execute = (request: HttpClientRequest.HttpClientRequest) =>
      Effect.sync(() => {
        const mintRequest = decodeMintRequestBody(requestBodyText(request));
        return HttpClientResponse.fromWeb(
          request,
          Response.json(signMintResponse(mintRequest), { status: 200 }),
        );
      });

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const result = yield* Effect.exit(
        connector.connect({
          userId: "user_123",
          environmentId: "env-connector-test",
          clientProofKeyThumbprint: "client-proof-key-thumbprint",
        }),
      );

      expect(result._tag).toBe("Failure");
      if (result._tag === "Failure") {
        expect(result.cause.toString()).toContain("EnvironmentMintResponseInvalid");
      }
    }).pipe(
      Effect.provide(
        connectorTestLayer(execute, {
          links: makeLinks({
            environmentPublicKey: "not a pem public key",
          }),
        }),
      ),
    );
  });

  it.effect("rejects environment mint responses with an overlong credential window", () => {
    const execute = (request: HttpClientRequest.HttpClientRequest) =>
      Effect.sync(() => {
        const mintRequest = decodeMintRequestBody(requestBodyText(request));
        return HttpClientResponse.fromWeb(
          request,
          Response.json(
            { ...signMintResponse(mintRequest), expiresAt: "2999-01-01T00:00:00.000Z" },
            { status: 200 },
          ),
        );
      });

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const result = yield* Effect.exit(
        connector.connect({
          userId: "user_123",
          environmentId: "env-connector-test",
          clientProofKeyThumbprint: "client-proof-key-thumbprint",
        }),
      );

      expect(result._tag).toBe("Failure");
      if (result._tag === "Failure") {
        expect(result.cause.toString()).toContain("EnvironmentMintResponseInvalid");
      }
    }).pipe(Effect.provide(connectorTestLayer(execute)));
  });

  it.effect("rejects mint responses with an invalid expiresAt timestamp", () => {
    const execute = (request: HttpClientRequest.HttpClientRequest) => {
      const body = decodeMintRequestBody(requestBodyText(request));
      const response = { ...signMintResponse(body), expiresAt: "not-a-date" };
      return Effect.succeed(HttpClientResponse.fromWeb(request, Response.json(response)));
    };

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const error = yield* Effect.flip(
        connector.connect({
          userId: "user_123",
          environmentId: "env-connector-test",
          clientProofKeyThumbprint: "proof-thumbprint",
        }),
      );
      expect(error).toMatchObject({
        _tag: "EnvironmentMintResponseInvalid",
        operation: "connect",
      });
    }).pipe(Effect.provide(connectorTestLayer(execute)));
  });

  it.effect("returns deterministic offline status when health requests time out", () => {
    let resolveRequestStarted: (() => void) | undefined;
    const requestStarted = new Promise<void>((resolve) => {
      resolveRequestStarted = resolve;
    });
    const execute = () =>
      Effect.sync(() => resolveRequestStarted?.()).pipe(
        Effect.andThen(Effect.never as Effect.Effect<HttpClientResponse.HttpClientResponse>),
      );
    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const fiber = yield* connector
        .status({
          userId: "user_123",
          environmentId: "env-connector-test",
        })
        .pipe(Effect.forkScoped);
      yield* Effect.promise(() => requestStarted);
      yield* TestClock.adjust(
        Duration.millis(EnvironmentConnector.ENVIRONMENT_MINT_REQUEST_TIMEOUT_MS),
      );
      const result = yield* Fiber.join(fiber);
      expect(result).toMatchObject({
        status: "offline",
        error: "Managed endpoint health request timed out.",
      });
    }).pipe(Effect.provide(Layer.merge(TestClock.layer(), connectorTestLayer(execute))));
  });

  it.effect("classifies UUID generation failures for both connector operations", () =>
    Effect.gen(function* () {
      const nodeCrypto = yield* Crypto.Crypto;
      const cause = PlatformError.badArgument({
        module: "Crypto",
        method: "randomUUIDv4",
        description: "uuid unavailable",
      });
      const makeCrypto = (failAt: number) => {
        let calls = 0;
        return Crypto.Crypto.of({
          ...nodeCrypto,
          randomUUIDv4: Effect.suspend(() =>
            ++calls === failAt ? Effect.fail(cause) : Effect.succeed(`uuid-${calls}`),
          ),
        });
      };
      const run = (operation: "status" | "connect", failAt: number) =>
        Effect.gen(function* () {
          const connector = yield* EnvironmentConnector.EnvironmentConnector;
          if (operation === "status") {
            return yield* Effect.flip(
              connector.status({ userId: "user_123", environmentId: "env-connector-test" }),
            );
          }
          return yield* Effect.flip(
            connector.connect({
              userId: "user_123",
              environmentId: "env-connector-test",
              clientProofKeyThumbprint: "proof-thumbprint",
            }),
          );
        }).pipe(
          Effect.provide(
            connectorTestLayer(() => Effect.die("HTTP must not run"), {
              crypto: makeCrypto(failAt),
            }),
          ),
        );

      const errors = yield* Effect.forEach(
        [
          ["status", 1],
          ["status", 2],
          ["connect", 1],
          ["connect", 2],
        ] as const,
        ([operation, failAt]) => run(operation, failAt),
      );
      expect(errors.every((error) => error._tag === "EnvironmentMintRequestFailed")).toBe(true);
      const mintErrors = errors.filter(
        (error): error is EnvironmentConnector.EnvironmentMintRequestFailed =>
          error._tag === "EnvironmentMintRequestFailed",
      );
      expect(mintErrors.map((error) => error.operation)).toEqual([
        "status",
        "status",
        "connect",
        "connect",
      ]);
      expect(mintErrors.every((error) => error.cause === cause)).toBe(true);
    }).pipe(Effect.provide(NodeCryptoLayer.layer)),
  );

  it.effect("classifies signing and connect transport failures", () => {
    const invalidSigningSettings = {
      ...settings,
      cloudMintPrivateKey: Redacted.make("invalid-private-key"),
    };
    const transport = new HttpClientError.HttpClientError({
      reason: new HttpClientError.TransportError({
        request: HttpClientRequest.post("https://env.example.test/connect/bibcode-mint-credential"),
        description: "offline",
      }),
    });
    const runStatus = Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      return yield* Effect.flip(
        connector.status({ userId: "user_123", environmentId: "env-connector-test" }),
      );
    }).pipe(
      Effect.provide(
        connectorTestLayer(() => Effect.die("HTTP must not run"), {
          settings: invalidSigningSettings,
        }),
      ),
    );
    const runConnectSigning = Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      return yield* Effect.flip(
        connector.connect({
          userId: "user_123",
          environmentId: "env-connector-test",
          clientProofKeyThumbprint: "proof-thumbprint",
        }),
      );
    }).pipe(
      Effect.provide(
        connectorTestLayer(() => Effect.die("HTTP must not run"), {
          settings: invalidSigningSettings,
        }),
      ),
    );
    const runTransport = Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      return yield* Effect.flip(
        connector.connect({
          userId: "user_123",
          environmentId: "env-connector-test",
          clientProofKeyThumbprint: "proof-thumbprint",
        }),
      );
    }).pipe(Effect.provide(connectorTestLayer(() => Effect.fail(transport))));

    return Effect.gen(function* () {
      const [statusSigning, connectSigning, connectTransport] = yield* Effect.all([
        runStatus,
        runConnectSigning,
        runTransport,
      ]);
      expect(statusSigning).toMatchObject({ operation: "status" });
      expect(connectSigning).toMatchObject({ operation: "connect" });
      expect(connectTransport).toMatchObject({ operation: "connect", cause: transport });
    });
  });

  it.effect("times out hung managed endpoint mint requests", () => {
    let resolveRequestStarted: (() => void) | undefined;
    const requestStarted = new Promise<void>((resolve) => {
      resolveRequestStarted = () => resolve();
    });
    const execute = () =>
      Effect.sync(() => {
        resolveRequestStarted?.();
      }).pipe(Effect.andThen(Effect.never as Effect.Effect<HttpClientResponse.HttpClientResponse>));

    return Effect.gen(function* () {
      const connector = yield* EnvironmentConnector.EnvironmentConnector;
      const resultFiber = yield* connector
        .connect({
          userId: "user_123",
          environmentId: "env-connector-test",
          clientProofKeyThumbprint: "client-proof-key-thumbprint",
        })
        .pipe(Effect.result, Effect.forkScoped);

      yield* Effect.promise(() => requestStarted);
      yield* TestClock.adjust(
        Duration.millis(EnvironmentConnector.ENVIRONMENT_MINT_REQUEST_TIMEOUT_MS),
      );
      const result = yield* Fiber.join(resultFiber);

      expect(Result.isFailure(result)).toBe(true);
      if (Result.isFailure(result)) {
        expect(result.failure._tag).toBe("EnvironmentMintRequestTimedOut");
        expect(result.failure).toMatchObject({
          environmentId: "env-connector-test",
          timeoutMs: EnvironmentConnector.ENVIRONMENT_MINT_REQUEST_TIMEOUT_MS,
        });
      }
    }).pipe(Effect.provide(Layer.merge(TestClock.layer(), connectorTestLayer(execute))));
  });
});
