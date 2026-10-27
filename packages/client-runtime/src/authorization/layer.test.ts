import {
  AuthStandardClientScopes,
  EnvironmentId,
  type ExecutionEnvironmentDescriptor,
} from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Ref from "effect/Ref";
import * as Schema from "effect/Schema";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";

import * as ManagedRelay from "../relay/managedRelay.ts";
import { remoteHttpClientLayer } from "../rpc/http.ts";
import * as ClientCapabilities from "../platform/capabilities.ts";
import * as RemoteEnvironmentAuthorization from "./service.ts";
import * as TokenStore from "./tokenStore.ts";
import { BearerConnectionProfile } from "../connection/catalog.ts";

const ENVIRONMENT_ID = EnvironmentId.make("environment-1");
const ENDPOINT = {
  httpBaseUrl: "https://environment.example.test",
  wsBaseUrl: "wss://environment.example.test",
  providerKind: "cloudflare_tunnel" as const,
};
const DESCRIPTOR = {
  environmentId: ENVIRONMENT_ID,
  label: "Remote environment",
  platform: {
    os: "linux",
    arch: "x64",
  },
  serverVersion: "0.0.0-test",
  storageInstanceId: "store-current",
  bootId: null,
  remoteUpdateSupport: null,
  remoteProtocolVersion: 1,
  minCompatibleRemoteProtocol: 1,
  capabilities: makeTestExecutionEnvironmentCapabilities({ repositoryIdentity: true }),
} satisfies ExecutionEnvironmentDescriptor;
const BOOTSTRAP: RemoteEnvironmentAuthorization.RelayEnvironmentAuthorization = {
  environmentId: ENVIRONMENT_ID,
  endpoint: ENDPOINT,
  credential: "relay-bootstrap",
};

function recordedFetch(responses: ReadonlyArray<Response>) {
  const calls: Array<readonly [RequestInfo | URL, RequestInit]> = [];
  let responseIndex = 0;
  const fetchFn = ((input, init) => {
    calls.push([input, init ?? {}]);
    const response = responses[responseIndex++];
    return response === undefined
      ? Promise.reject(new Error(`Unexpected fetch call to ${String(input)}`))
      : Promise.resolve(response);
  }) satisfies typeof fetch;
  return { calls, fetchFn };
}

const websocketTicket = (ticket: string) =>
  Response.json({
    ticket,
    expiresAt: "2026-06-06T01:00:00.000Z",
  });

const decodeBearerConnectionProfile = Schema.decodeUnknownEffect(BearerConnectionProfile);

const accessToken = (token: string) =>
  Response.json({
    access_token: token,
    issued_token_type: "urn:ietf:params:oauth:token-type:access_token",
    token_type: "DPoP",
    expires_in: 3_600,
    scope: AuthStandardClientScopes.join(" "),
  });

const authInvalid = () =>
  Response.json(
    {
      _tag: "EnvironmentAuthInvalidError",
      code: "auth_invalid",
      reason: "invalid_credential",
      traceId: "trace-auth-invalid",
    },
    { status: 401 },
  );

const makeHarness = Effect.fn("TestRemoteAuthorization.makeHarness")(function* (input: {
  readonly initialToken?: TokenStore.RemoteDpopAccessToken;
  readonly responses: ReadonlyArray<Response>;
}) {
  const tokens = yield* Ref.make(
    new Map(
      input.initialToken === undefined
        ? []
        : [[input.initialToken.environmentId, input.initialToken]],
    ),
  );
  const bootstrapCalls = yield* Ref.make(0);
  const proofInputs = yield* Ref.make<
    ReadonlyArray<{
      readonly method: string;
      readonly url: string;
      readonly accessToken?: string;
    }>
  >([]);
  const fetch = recordedFetch(input.responses);

  const tokenStore = TokenStore.RemoteDpopAccessTokenStore.of({
    get: (environmentId) =>
      Ref.get(tokens).pipe(
        Effect.map((current) => Option.fromUndefinedOr(current.get(environmentId))),
      ),
    put: (token) =>
      Ref.update(tokens, (current) => {
        const next = new Map(current);
        next.set(token.environmentId, token);
        return next;
      }),
    remove: (environmentId) =>
      Ref.update(tokens, (current) => {
        const next = new Map(current);
        next.delete(environmentId);
        return next;
      }),
  });
  const signer = ManagedRelay.ManagedRelayDpopSigner.of({
    thumbprint: Effect.succeed("thumbprint-1"),
    createProof: (proofInput) =>
      Ref.update(proofInputs, (current) => [...current, proofInput]).pipe(
        Effect.as(`proof:${proofInput.url}`),
      ),
  });
  const layer = RemoteEnvironmentAuthorization.layer.pipe(
    Layer.provide(
      Layer.mergeAll(
        remoteHttpClientLayer(fetch.fetchFn),
        Layer.succeed(ManagedRelay.ManagedRelayDpopSigner, signer),
        Layer.succeed(TokenStore.RemoteDpopAccessTokenStore, tokenStore),
        Layer.succeed(
          ClientCapabilities.ClientPresentation,
          ClientCapabilities.ClientPresentation.of({
            metadata: {
              label: "Coddy Studio Test",
              deviceType: "desktop",
              os: "test",
            },
            scopes: AuthStandardClientScopes,
          }),
        ),
      ),
    ),
  );
  const obtainBootstrap = Ref.update(bootstrapCalls, (count) => count + 1).pipe(
    Effect.as(BOOTSTRAP),
  );

  return {
    layer,
    tokens,
    bootstrapCalls,
    proofInputs,
    fetch,
    obtainBootstrap,
  };
});

describe("RemoteEnvironmentAuthorization", () => {
  it.effect("authorizes a decoded blank legacy host key as plain bearer transport", () =>
    Effect.gen(function* () {
      const profile = yield* decodeBearerConnectionProfile({
        _tag: "BearerConnectionProfile",
        connectionId: "bearer:environment-1",
        environmentId: ENVIRONMENT_ID,
        label: "Legacy remote",
        httpBaseUrl: ENDPOINT.httpBaseUrl,
        wsBaseUrl: ENDPOINT.wsBaseUrl,
        hostKey: " \t ",
      });
      expect(profile.hostKey).toBeNull();
      const harness = yield* makeHarness({
        responses: [Response.json(DESCRIPTOR), websocketTicket("legacy-ticket")],
      });

      const authorized = yield* Effect.gen(function* () {
        const remote = yield* RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization;
        return yield* remote.authorizeBearer({
          expectedEnvironmentId: ENVIRONMENT_ID,
          httpBaseUrl: profile.httpBaseUrl,
          wsBaseUrl: profile.wsBaseUrl,
          bearerToken: "legacy-bearer",
          hostKey: profile.hostKey,
        });
      }).pipe(Effect.provide(harness.layer));

      expect(authorized.socketUrl).toContain("/ws?wsTicket=legacy-ticket");
      expect(authorized.httpAuthorization).toEqual({
        _tag: "Bearer",
        token: "legacy-bearer",
      });
      expect(authorized.e2ee).toBeNull();
      expect(harness.fetch.calls).toHaveLength(2);
    }),
  );

  it.effect("keeps host-key bearer credentials inside the E2EE channel", () =>
    Effect.gen(function* () {
      const hostKey = "HcMLXPPBHFNvcbHrCVMH-DMh49rd5AGCzSCqAVJ49hM";
      const harness = yield* makeHarness({ responses: [Response.json(DESCRIPTOR)] });

      const authorized = yield* Effect.gen(function* () {
        const remote = yield* RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization;
        return yield* remote.authorizeBearer({
          expectedEnvironmentId: ENVIRONMENT_ID,
          httpBaseUrl: ENDPOINT.httpBaseUrl,
          wsBaseUrl: ENDPOINT.wsBaseUrl,
          bearerToken: "stored-bearer",
          hostKey,
        });
      }).pipe(Effect.provide(harness.layer));

      expect(authorized.socketUrl).toBe("wss://environment.example.test/ws-e2ee");
      expect(authorized.e2ee).toEqual({
        hostKey,
        auth: { kind: "bearer", credential: "stored-bearer" },
      });
      expect(authorized.httpAuthorization).toBeNull();
      expect(harness.fetch.calls).toHaveLength(1);
      expect(String(harness.fetch.calls[0]?.[0])).toBe(
        "https://environment.example.test/.well-known/coddy-studio/environment",
      );
    }),
  );

  it.effect("returns the descriptor fetched while authorizing a bearer connection", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        responses: [Response.json(DESCRIPTOR), websocketTicket("bearer-ticket")],
      });

      const authorized = yield* Effect.gen(function* () {
        const remote = yield* RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization;
        return yield* remote.authorizeBearer({
          expectedEnvironmentId: ENVIRONMENT_ID,
          httpBaseUrl: ENDPOINT.httpBaseUrl,
          wsBaseUrl: ENDPOINT.wsBaseUrl,
          bearerToken: "bearer-access-token",
        });
      }).pipe(Effect.provide(harness.layer));

      expect(authorized.descriptor).toEqual(DESCRIPTOR);
      expect(authorized.label).toBe(DESCRIPTOR.label);
      expect(harness.fetch.calls).toHaveLength(2);
    }),
  );

  it.effect("refreshes the descriptor for every cached DPoP connection attempt", () =>
    Effect.gen(function* () {
      const cached = new TokenStore.RemoteDpopAccessToken({
        environmentId: ENVIRONMENT_ID,
        label: "Stale cached label",
        endpoint: ENDPOINT,
        accessToken: "cached-access-token",
        expiresAtEpochMs: Number.MAX_SAFE_INTEGER,
        dpopThumbprint: "thumbprint-1",
      });
      const firstDescriptor = {
        ...DESCRIPTOR,
        label: "Current environment A",
        storageInstanceId: "store-a",
      };
      const secondDescriptor = {
        ...DESCRIPTOR,
        label: "Current environment B",
        storageInstanceId: "store-b",
      };
      const harness = yield* makeHarness({
        initialToken: cached,
        responses: [
          Response.json(firstDescriptor),
          websocketTicket("cached-ticket-a"),
          Response.json(secondDescriptor),
          websocketTicket("cached-ticket-b"),
        ],
      });

      const [first, second] = yield* Effect.gen(function* () {
        const remote = yield* RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization;
        const first = yield* remote.authorizeDpop({
          expectedEnvironmentId: ENVIRONMENT_ID,
          obtainBootstrap: harness.obtainBootstrap,
        });
        const second = yield* remote.authorizeDpop({
          expectedEnvironmentId: ENVIRONMENT_ID,
          obtainBootstrap: harness.obtainBootstrap,
        });
        return [first, second] as const;
      }).pipe(Effect.provide(harness.layer));

      expect(first.descriptor).toEqual(firstDescriptor);
      expect(first.label).toBe("Current environment A");
      expect(first.socketUrl).toContain("wsTicket=cached-ticket-a");
      expect(second.descriptor).toEqual(secondDescriptor);
      expect(second.label).toBe("Current environment B");
      expect(second.socketUrl).toContain("wsTicket=cached-ticket-b");
      expect(yield* Ref.get(harness.bootstrapCalls)).toBe(0);
      expect(harness.fetch.calls).toHaveLength(4);
      expect(String(harness.fetch.calls[0]?.[0])).toBe(
        "https://environment.example.test/.well-known/coddy-studio/environment",
      );
      expect(String(harness.fetch.calls[1]?.[0])).toBe(
        "https://environment.example.test/api/auth/websocket-ticket",
      );
      expect(String(harness.fetch.calls[2]?.[0])).toBe(
        "https://environment.example.test/.well-known/coddy-studio/environment",
      );
    }),
  );

  it.effect("refreshes and persists an expired environment token", () =>
    Effect.gen(function* () {
      const expired = new TokenStore.RemoteDpopAccessToken({
        environmentId: ENVIRONMENT_ID,
        label: DESCRIPTOR.label,
        endpoint: ENDPOINT,
        accessToken: "expired-access-token",
        expiresAtEpochMs: 0,
        dpopThumbprint: "thumbprint-1",
      });
      const harness = yield* makeHarness({
        initialToken: expired,
        responses: [
          Response.json(DESCRIPTOR),
          accessToken("fresh-access-token"),
          websocketTicket("fresh-ticket"),
        ],
      });

      const authorized = yield* Effect.gen(function* () {
        const remote = yield* RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization;
        return yield* remote.authorizeDpop({
          expectedEnvironmentId: ENVIRONMENT_ID,
          obtainBootstrap: harness.obtainBootstrap,
        });
      }).pipe(Effect.provide(harness.layer));

      expect(authorized.socketUrl).toContain("wsTicket=fresh-ticket");
      expect(authorized.descriptor).toEqual(DESCRIPTOR);
      expect(yield* Ref.get(harness.bootstrapCalls)).toBe(1);
      expect((yield* Ref.get(harness.tokens)).get(ENVIRONMENT_ID)).toEqual(
        expect.objectContaining({
          accessToken: "fresh-access-token",
          dpopThumbprint: "thumbprint-1",
        }),
      );
      expect(harness.fetch.calls).toHaveLength(3);
    }),
  );

  it.effect("evicts an auth-invalid cached token and obtains a fresh bootstrap", () =>
    Effect.gen(function* () {
      const cached = new TokenStore.RemoteDpopAccessToken({
        environmentId: ENVIRONMENT_ID,
        label: DESCRIPTOR.label,
        endpoint: ENDPOINT,
        accessToken: "invalid-access-token",
        expiresAtEpochMs: Number.MAX_SAFE_INTEGER,
        dpopThumbprint: "thumbprint-1",
      });
      const harness = yield* makeHarness({
        initialToken: cached,
        responses: [
          Response.json(DESCRIPTOR),
          authInvalid(),
          Response.json(DESCRIPTOR),
          accessToken("replacement-access-token"),
          websocketTicket("replacement-ticket"),
        ],
      });

      const authorized = yield* Effect.gen(function* () {
        const remote = yield* RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization;
        return yield* remote.authorizeDpop({
          expectedEnvironmentId: ENVIRONMENT_ID,
          obtainBootstrap: harness.obtainBootstrap,
        });
      }).pipe(Effect.provide(harness.layer));

      expect(authorized.socketUrl).toContain("wsTicket=replacement-ticket");
      expect(yield* Ref.get(harness.bootstrapCalls)).toBe(1);
      expect((yield* Ref.get(harness.tokens)).get(ENVIRONMENT_ID)).toEqual(
        expect.objectContaining({
          accessToken: "replacement-access-token",
        }),
      );
      expect(harness.fetch.calls).toHaveLength(5);
    }),
  );

  it.effect("refreshes a cached endpoint after consecutive transient failures", () =>
    Effect.gen(function* () {
      const cached = new TokenStore.RemoteDpopAccessToken({
        environmentId: ENVIRONMENT_ID,
        label: DESCRIPTOR.label,
        endpoint: ENDPOINT,
        accessToken: "cached-access-token",
        expiresAtEpochMs: Number.MAX_SAFE_INTEGER,
        dpopThumbprint: "thumbprint-1",
      });
      const harness = yield* makeHarness({
        initialToken: cached,
        responses: [
          Response.json(DESCRIPTOR),
          new Response("endpoint unavailable", { status: 503 }),
          Response.json(DESCRIPTOR),
          new Response("endpoint still unavailable", { status: 503 }),
          Response.json(DESCRIPTOR),
          accessToken("replacement-access-token"),
          websocketTicket("replacement-ticket"),
        ],
      });

      const authorized = yield* Effect.gen(function* () {
        const remote = yield* RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization;
        const firstFailure = yield* remote
          .authorizeDpop({
            expectedEnvironmentId: ENVIRONMENT_ID,
            obtainBootstrap: harness.obtainBootstrap,
          })
          .pipe(Effect.flip);

        expect(firstFailure._tag).toBe("ConnectionTransientError");
        expect(yield* Ref.get(harness.bootstrapCalls)).toBe(0);
        expect((yield* Ref.get(harness.tokens)).get(ENVIRONMENT_ID)).toBe(cached);

        return yield* remote.authorizeDpop({
          expectedEnvironmentId: ENVIRONMENT_ID,
          obtainBootstrap: harness.obtainBootstrap,
        });
      }).pipe(Effect.provide(harness.layer));

      expect(authorized.socketUrl).toContain("wsTicket=replacement-ticket");
      expect(yield* Ref.get(harness.bootstrapCalls)).toBe(1);
      expect((yield* Ref.get(harness.tokens)).get(ENVIRONMENT_ID)).toEqual(
        expect.objectContaining({
          accessToken: "replacement-access-token",
        }),
      );
      expect(harness.fetch.calls).toHaveLength(7);
    }),
  );

  it.effect("does not persist a refreshed token until its websocket ticket succeeds", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        responses: [
          Response.json(DESCRIPTOR),
          accessToken("unusable-access-token"),
          new Response("endpoint unavailable", { status: 503 }),
        ],
      });

      yield* Effect.gen(function* () {
        const remote = yield* RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization;
        return yield* remote.authorizeDpop({
          expectedEnvironmentId: ENVIRONMENT_ID,
          obtainBootstrap: harness.obtainBootstrap,
        });
      }).pipe(Effect.provide(harness.layer), Effect.flip);

      expect((yield* Ref.get(harness.tokens)).has(ENVIRONMENT_ID)).toBe(false);
      expect(yield* Ref.get(harness.bootstrapCalls)).toBe(1);
      expect(harness.fetch.calls).toHaveLength(3);
    }),
  );
});
