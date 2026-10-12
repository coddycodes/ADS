import {
  type DesktopBridge,
  EnvironmentId,
  type RelayClientStatus,
  type RelayClientInstallProgressEvent,
  WS_METHODS,
} from "@bibcode/contracts";
import {
  RelayAuthInvalidError,
  RelayEnvironmentConnectNotAuthorizedError,
  RelayEnvironmentEndpointTimedOutError,
  RelayEnvironmentEndpointUnavailableError,
  RelayEnvironmentLinkFailedError,
  RelayEnvironmentLinkProofExpiredError,
  RelayEnvironmentLinkProofInvalidError,
  RelayEnvironmentLinkUnavailableError,
  RelayInternalError,
  type RelayEnvironmentLinkResponse,
  RelayWebClientId,
  type RelayProtectedError,
} from "@bibcode/contracts/relay";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { HttpClient } from "effect/unstable/http";
import { afterEach, beforeEach, vi } from "vite-plus/test";
import {
  AVAILABLE_CONNECTION_STATE,
  EnvironmentSupervisor,
  type PreparedConnection,
  PrimaryConnectionTarget,
} from "@bibcode/client-runtime/connection";
import { type RpcSession } from "@bibcode/client-runtime/rpc";
import { EnvironmentRegistry } from "@bibcode/client-runtime/connection";
import { ManagedRelay } from "@bibcode/client-runtime/relay";
import { remoteHttpClientLayer } from "@bibcode/client-runtime/rpc";
import { makeTestExecutionEnvironmentCapabilities } from "@bibcode/shared/testSupport";
import {
  resetPrimaryEnvironmentDescriptorForTests,
  writePrimaryEnvironmentDescriptor,
} from "../environments/primary";

import {
  collectCloudLinkTargets,
  linkPrimaryEnvironmentToCloud,
  listManagedCloudEnvironments,
  needsManagedEndpointRelink,
  normalizeRelayBaseUrl,
  readPrimaryCloudLinkTarget,
  readPrimaryCloudLinkState,
  relinkPrimaryManagedEndpoint,
  type CloudLinkTarget,
  unlinkPrimaryEnvironmentFromCloud,
} from "./linkEnvironment";

const TARGET: CloudLinkTarget = {
  environmentId: "environment-1",
  label: "Desktop",
  httpBaseUrl: "http://127.0.0.1:3000",
  wsBaseUrl: "ws://127.0.0.1:3000",
};

const LINK_RESPONSE: RelayEnvironmentLinkResponse = {
  ok: true,
  environmentId: EnvironmentId.make(TARGET.environmentId),
  endpoint: {
    httpBaseUrl: "https://desktop.example.test",
    wsBaseUrl: "wss://desktop.example.test",
    providerKind: "cloudflare_tunnel",
  },
  endpointRuntime: null,
  relayIssuer: "https://relay.example.test",
  cloudUserId: "user-1",
  environmentCredential: "environment-credential",
  cloudMintPublicKey: "public-key",
};

const relayClientInstallDialog = vi.hoisted(() => ({
  requestConfirmation: vi.fn(),
  reportProgress: vi.fn(),
  finish: vi.fn(),
}));

vi.mock("./relayClientInstallDialog", () => ({
  requestRelayClientInstallConfirmation: relayClientInstallDialog.requestConfirmation,
  reportRelayClientInstallProgress: relayClientInstallDialog.reportProgress,
  finishRelayClientInstall: relayClientInstallDialog.finish,
}));

const createProof = vi.fn(() => Effect.succeed("dpop-proof"));
const dpopSignerLayer = Layer.succeed(
  ManagedRelay.ManagedRelayDpopSigner,
  ManagedRelay.ManagedRelayDpopSigner.of({
    thumbprint: Effect.succeed("thumbprint"),
    createProof,
  }),
);

function relayLayer() {
  const http = remoteHttpClientLayer(globalThis.fetch);
  return Layer.mergeAll(
    http,
    ManagedRelay.layer({
      relayUrl: "https://relay.example.test",
      clientId: RelayWebClientId,
    }).pipe(Layer.provideMerge(dpopSignerLayer), Layer.provide(http)),
  );
}

function registryLayer(options?: {
  readonly status?: RelayClientStatus;
  readonly statusFailure?: unknown;
  readonly installEvents?: ReadonlyArray<RelayClientInstallProgressEvent>;
}) {
  return Layer.effect(
    EnvironmentRegistry,
    Effect.gen(function* () {
      const client = {
        [WS_METHODS.cloudGetRelayClientStatus]: () =>
          options?.statusFailure
            ? Effect.fail(options.statusFailure)
            : Effect.succeed(
                options?.status ?? {
                  status: "available",
                  executablePath: "/usr/local/bin/bibcode-relay",
                  source: "managed",
                  version: "2026.6.0",
                },
              ),
        [WS_METHODS.cloudInstallRelayClient]: () =>
          Stream.fromIterable(options?.installEvents ?? []),
      } as unknown as RpcSession["client"];
      const session: RpcSession = {
        client,
        initialConfig: Effect.never,
        ready: Effect.void,
        probe: Effect.void,
        closed: Effect.never,
        e2eeAuthenticated: Effect.succeed(null),
      };
      const target = new PrimaryConnectionTarget({
        environmentId: EnvironmentId.make(TARGET.environmentId),
        label: TARGET.label,
        httpBaseUrl: TARGET.httpBaseUrl,
        wsBaseUrl: TARGET.wsBaseUrl,
      });
      const supervisor = EnvironmentSupervisor.of({
        target,
        state: yield* SubscriptionRef.make(AVAILABLE_CONNECTION_STATE),
        session: yield* SubscriptionRef.make(Option.some(session)),
        prepared: yield* SubscriptionRef.make(Option.none<PreparedConnection>()),
        connect: Effect.void,
        disconnect: Effect.void,
        retryNow: Effect.void,
      } satisfies EnvironmentSupervisor["Service"]);
      const registry = {
        run: <A, E, R>(_environmentId: EnvironmentId, effect: Effect.Effect<A, E, R>) =>
          Effect.provideService(effect, EnvironmentSupervisor, supervisor),
        runStream: <A, E, R>(_environmentId: EnvironmentId, stream: Stream.Stream<A, E, R>) =>
          Stream.provideService(stream, EnvironmentSupervisor, supervisor),
      } as unknown as EnvironmentRegistry["Service"];
      return EnvironmentRegistry.of(registry);
    }),
  );
}

function services(options?: Parameters<typeof registryLayer>[0]) {
  return Layer.mergeAll(relayLayer(), registryLayer(options));
}

function relayServiceLayer(
  overrides: Partial<ManagedRelay.ManagedRelayClient["Service"]>,
): Layer.Layer<ManagedRelay.ManagedRelayClient> {
  const unexpected = () => Effect.die(new Error("Unexpected relay client call."));
  const service = ManagedRelay.ManagedRelayClient.of({
    relayUrl: "https://relay.example.test",
    listEnvironments: unexpected,
    createEnvironmentLinkChallenge: unexpected,
    linkEnvironment: unexpected,
    unlinkEnvironment: unexpected,
    getEnvironmentStatus: unexpected,
    connectEnvironment: unexpected,
    resetTokenCache: Effect.void,
    ...overrides,
  } as ManagedRelay.ManagedRelayClient["Service"]);
  return Layer.succeed(ManagedRelay.ManagedRelayClient, service);
}

function withRelayService<A, E, R>(
  effect: Effect.Effect<A, E, R>,
  overrides: Partial<ManagedRelay.ManagedRelayClient["Service"]>,
  registryOptions?: Parameters<typeof registryLayer>[0],
) {
  return effect.pipe(
    Effect.provide(
      Layer.mergeAll(
        remoteHttpClientLayer(globalThis.fetch),
        relayServiceLayer(overrides),
        registryLayer(registryOptions),
      ),
    ),
  );
}

function withServices<A, E>(
  effect: Effect.Effect<
    A,
    E,
    HttpClient.HttpClient | ManagedRelay.ManagedRelayClient | EnvironmentRegistry
  >,
  options?: Parameters<typeof registryLayer>[0],
) {
  return effect.pipe(Effect.provide(services(options)));
}

function bodyText(body: BodyInit | null | undefined): string {
  return body instanceof Uint8Array ? new TextDecoder().decode(body) : String(body ?? "");
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.stubEnv("VITE_BIBCODE_RELAY_URL", "https://relay.example.test");
  relayClientInstallDialog.requestConfirmation.mockResolvedValue(true);
});

afterEach(() => {
  resetPrimaryEnvironmentDescriptorForTests();
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
});

describe("web cloud link environment client", () => {
  it("normalizes relay URLs and de-duplicates cloud link targets", () => {
    expect(normalizeRelayBaseUrl(" https://relay.example.test/// ")).toBe(
      "https://relay.example.test",
    );
    expect(normalizeRelayBaseUrl(" ")).toBeNull();
    expect(normalizeRelayBaseUrl(undefined)).toBeNull();
    expect(normalizeRelayBaseUrl("https://relay.example.test")).toBe("https://relay.example.test");
    expect(
      collectCloudLinkTargets({
        primary: TARGET,
        saved: [TARGET, { ...TARGET, environmentId: "environment-2" }],
      }).map((target) => target.environmentId),
    ).toEqual(["environment-1", "environment-2"]);
    expect(collectCloudLinkTargets({ primary: null, saved: [TARGET] })).toEqual([TARGET]);
  });

  it("reads the primary target only after its descriptor is available", () => {
    vi.stubGlobal("window", {
      location: new URL("http://localhost:3773/"),
      desktopBridge: undefined,
    });
    expect(readPrimaryCloudLinkTarget()).toBeNull();

    writePrimaryEnvironmentDescriptor({
      environmentId: EnvironmentId.make(TARGET.environmentId),
      label: TARGET.label,
      platform: { os: "darwin", arch: "arm64" },
      serverVersion: "0.0.0-test",
      storageInstanceId: null,
      bootId: null,
      remoteUpdateSupport: null,
      remoteProtocolVersion: 1,
      minCompatibleRemoteProtocol: 1,
      capabilities: makeTestExecutionEnvironmentCapabilities({ repositoryIdentity: true }),
    });

    expect(readPrimaryCloudLinkTarget()).toEqual({
      environmentId: TARGET.environmentId,
      label: TARGET.label,
      httpBaseUrl: "http://localhost:3773/",
      wsBaseUrl: "ws://localhost:3773/",
    });
  });

  it.effect("lists relay-managed environments through the typed relay client", () =>
    Effect.gen(function* () {
      const fetchMock = vi.fn().mockResolvedValue(
        Response.json({
          environments: [
            {
              environmentId: "environment-1",
              label: "Desktop",
              endpoint: {
                httpBaseUrl: "https://desktop.example.test",
                wsBaseUrl: "wss://desktop.example.test",
                providerKind: "cloudflare_tunnel",
              },
              linkedAt: "2026-06-06T00:00:00.000Z",
            },
          ],
        }),
      );
      vi.stubGlobal("fetch", fetchMock);

      const environments = yield* withServices(
        listManagedCloudEnvironments({ clerkToken: "clerk-token" }),
      );

      expect(environments).toHaveLength(1);
      expect(fetchMock.mock.calls[0]?.[1]?.headers.authorization).toBe("Bearer clerk-token");
    }),
  );

  it.effect("reports missing relay configuration and relay listing failures", () =>
    Effect.gen(function* () {
      vi.stubEnv("VITE_BIBCODE_RELAY_URL", "");
      const missingConfig = yield* withServices(
        listManagedCloudEnvironments({ clerkToken: "clerk-token" }),
      ).pipe(Effect.flip);
      expect(missingConfig.message).toBe("BIBCODE_RELAY_URL is not configured.");

      vi.stubEnv("VITE_BIBCODE_RELAY_URL", "https://relay.example.test");
      const cause = new ManagedRelay.ManagedRelayRequestTimeoutError({
        activity: "Relay environment listing",
        timeoutMs: 10_000,
      });
      const failed = yield* withRelayService(
        listManagedCloudEnvironments({ clerkToken: "clerk-token" }),
        { listEnvironments: () => Effect.fail(cause) },
      ).pipe(Effect.flip);
      expect(failed).toMatchObject({
        message: "Could not list relay-managed environments.",
        cause,
      });
    }),
  );

  it.effect("reads primary cloud link state from the explicit target", () =>
    Effect.gen(function* () {
      const fetchMock = vi.fn().mockResolvedValue(
        Response.json({
          linked: true,
          cloudUserId: "user-1",
          relayUrl: "https://relay.example.test",
          relayIssuer: "https://relay.example.test",
        }),
      );
      vi.stubGlobal("fetch", fetchMock);

      const state = yield* withServices(readPrimaryCloudLinkState({ target: TARGET }));

      expect(Option.fromNullishOr(state)).toEqual(
        Option.some({
          linked: true,
          cloudUserId: "user-1",
          relayUrl: "https://relay.example.test",
          relayIssuer: "https://relay.example.test",
        }),
      );
      expect(String(fetchMock.mock.calls[0]?.[0])).toBe(
        "http://127.0.0.1:3000/api/connect/link-state",
      );
    }),
  );

  it("needs a preview-host re-link only for a linked managed tunnel without preview hostnames", () => {
    const state = (overrides: Record<string, unknown>) =>
      ({
        linked: true,
        cloudUserId: "user-1",
        relayUrl: "https://relay.example.test",
        relayIssuer: null,
        managedEndpoint: true,
        previewHostnames: false,
        ...overrides,
      }) as Parameters<typeof needsManagedEndpointRelink>[0];
    expect(needsManagedEndpointRelink(state({}))).toBe(true);
    // A tunnel provisioned for a port the server no longer listens on needs one too.
    expect(
      needsManagedEndpointRelink(state({ previewHostnames: true, managedEndpointStale: true })),
    ).toBe(true);
    expect(needsManagedEndpointRelink(state({ previewHostnames: true }))).toBe(false);
    expect(needsManagedEndpointRelink(state({ managedEndpoint: false }))).toBe(false);
    expect(needsManagedEndpointRelink(state({ linked: false }))).toBe(false);
    // A server that predates the flags reports neither.
    expect(
      needsManagedEndpointRelink(
        state({ managedEndpoint: undefined, previewHostnames: undefined }),
      ),
    ).toBe(false);
    expect(needsManagedEndpointRelink(null)).toBe(false);
  });

  it.effect("re-links for preview hostnames only for the linked account while still needed", () =>
    Effect.gen(function* () {
      const serveLinkState = (overrides: Record<string, unknown>) =>
        vi.stubGlobal(
          "fetch",
          vi.fn().mockResolvedValue(
            Response.json({
              linked: true,
              cloudUserId: "user-1",
              relayUrl: "https://relay.example.test",
              relayIssuer: "https://relay.example.test",
              managedEndpoint: true,
              previewHostnames: false,
              linkGeneration: "generation-1",
              ...overrides,
            }),
          ),
        );
      const challenge = vi.fn(() =>
        Effect.fail(
          new ManagedRelay.ManagedRelayRequestTimeoutError({
            activity: "Relay environment link challenge",
            timeoutMs: 10_000,
          }),
        ),
      );
      const relink = (accountId: string) =>
        withRelayService(
          relinkPrimaryManagedEndpoint({
            target: TARGET,
            clerkToken: "clerk-token",
            accountId,
          }),
          { createEnvironmentLinkChallenge: challenge },
        );

      // Re-read on the link scheduler: a link or unlink that ran first wins.
      serveLinkState({ previewHostnames: true });
      expect(yield* relink("user-1")).toBe(false);
      serveLinkState({ linked: false, cloudUserId: null, managedEndpoint: false });
      expect(yield* relink("user-1")).toBe(false);
      // Another signed-in account never re-links someone else's environment.
      serveLinkState({});
      expect(yield* relink("user-2")).toBe(false);
      // A server that names no link generation can't fence a re-link, so none runs.
      serveLinkState({ linkGeneration: null });
      expect(yield* relink("user-1")).toBe(false);
      expect(challenge).not.toHaveBeenCalled();

      serveLinkState({});
      yield* relink("user-1").pipe(Effect.flip);
      expect(challenge).toHaveBeenCalledOnce();
    }),
  );

  it.effect("names its account so the server can refuse a re-link of a changed link", () =>
    Effect.gen(function* () {
      const fetchMock = vi
        .fn()
        .mockResolvedValueOnce(
          Response.json({
            linked: true,
            cloudUserId: "user-1",
            relayUrl: "https://relay.example.test",
            relayIssuer: "https://relay.example.test",
            managedEndpoint: true,
            previewHostnames: true,
            managedEndpointStale: true,
            linkGeneration: "generation-1",
          }),
        )
        .mockResolvedValueOnce(Response.json("signed-proof"))
        .mockResolvedValueOnce(
          Response.json(
            {
              _tag: "EnvironmentHttpConflictError",
              message:
                "This environment's BiBCode Connect link changed. Link it again from Settings.",
            },
            { status: 409 },
          ),
        );
      vi.stubGlobal("fetch", fetchMock);
      const unlinkEnvironment = vi.fn(() => Effect.succeed({ ok: true }));

      const refused = yield* withRelayService(
        relinkPrimaryManagedEndpoint({
          target: TARGET,
          clerkToken: "clerk-token",
          accountId: "user-1",
        }),
        {
          createEnvironmentLinkChallenge: () =>
            Effect.succeed({ challenge: "challenge", expiresAt: "2026-06-06T00:05:00.000Z" }),
          linkEnvironment: () => Effect.succeed(LINK_RESPONSE),
          unlinkEnvironment,
        },
      ).pipe(Effect.flip);

      expect(refused.message).toContain("link changed");
      // @effect-diagnostics-next-line preferSchemaOverJson:off
      expect(JSON.parse(bodyText(fetchMock.mock.calls[2]?.[1]?.body))).toMatchObject({
        requireLinkGeneration: "generation-1",
      });
      // Removing the relay link could delete a newer link another client made since.
      expect(unlinkEnvironment).not.toHaveBeenCalled();
    }),
  );

  it.effect("uses desktop bearer auth for primary cloud link state", () =>
    Effect.gen(function* () {
      const fetchMock = vi.fn().mockResolvedValue(
        Response.json({
          linked: true,
          cloudUserId: "user-1",
          relayUrl: "https://relay.example.test",
          relayIssuer: "https://relay.example.test",
        }),
      );
      vi.stubGlobal("fetch", fetchMock);
      vi.stubGlobal("window", {
        location: { origin: "bibcode://app" },
        desktopBridge: {
          getLocalEnvironmentBearerToken: vi.fn().mockResolvedValue("desktop-bearer-token"),
        } as unknown as DesktopBridge,
      });

      yield* withServices(readPrimaryCloudLinkState({ target: TARGET }));

      const request = new Request(fetchMock.mock.calls[0]?.[0], fetchMock.mock.calls[0]?.[1]);
      expect(request.credentials).not.toBe("include");
      expect(request.headers.get("authorization")).toBe("Bearer desktop-bearer-token");
    }),
  );

  it.effect("preserves environment API error details when link-state reads fail", () =>
    Effect.gen(function* () {
      vi.stubGlobal(
        "fetch",
        vi
          .fn()
          .mockResolvedValue(
            Response.json(
              { _tag: "EnvironmentHttpBadRequestError", message: "The link target is invalid." },
              { status: 400 },
            ),
          ),
      );

      const error = yield* withServices(readPrimaryCloudLinkState({ target: TARGET })).pipe(
        Effect.flip,
      );

      expect(error.message).toBe(
        "Could not read environment cloud link state: The link target is invalid.",
      );
    }),
  );

  it.effect("uses the operation message for unstructured environment API failures", () =>
    Effect.gen(function* () {
      vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("connection reset")));

      const error = yield* withServices(readPrimaryCloudLinkState({ target: TARGET })).pipe(
        Effect.flip,
      );

      expect(error.message).toBe("Could not read environment cloud link state.");
      expect(error.cause).toBeDefined();
    }),
  );

  it.effect("links an available primary environment without invoking installation", () =>
    Effect.gen(function* () {
      const fetchMock = vi
        .fn()
        .mockResolvedValueOnce(
          Response.json({
            challenge: "challenge",
            expiresAt: "2026-06-06T00:05:00.000Z",
          }),
        )
        .mockResolvedValueOnce(Response.json("signed-proof"))
        .mockResolvedValueOnce(
          Response.json({
            ok: true,
            environmentId: TARGET.environmentId,
            endpoint: {
              httpBaseUrl: "https://desktop.example.test",
              wsBaseUrl: "wss://desktop.example.test",
              providerKind: "cloudflare_tunnel",
            },
            endpointRuntime: null,
            relayIssuer: "https://relay.example.test",
            cloudUserId: "user-1",
            environmentCredential: "environment-credential",
            cloudMintPublicKey: "public-key",
          }),
        )
        .mockResolvedValueOnce(
          Response.json({ ok: true, endpointRuntimeStatus: { status: "configured" } }),
        );
      vi.stubGlobal("fetch", fetchMock);

      yield* withServices(
        linkPrimaryEnvironmentToCloud({
          target: TARGET,
          clerkToken: "clerk-token",
        }),
      );

      expect(relayClientInstallDialog.requestConfirmation).not.toHaveBeenCalled();
      expect(String(fetchMock.mock.calls[1]?.[0])).toBe(
        "http://127.0.0.1:3000/api/connect/link-proof",
      );
      // @effect-diagnostics-next-line preferSchemaOverJson:off
      expect(JSON.parse(bodyText(fetchMock.mock.calls[1]?.[1]?.body))).toMatchObject({
        challenge: "challenge",
        endpoint: {
          httpBaseUrl: TARGET.httpBaseUrl,
          wsBaseUrl: TARGET.wsBaseUrl,
        },
      });
    }),
  );

  it.effect("turns structured relay failures into actionable link errors", () =>
    Effect.gen(function* () {
      const traceId = "trace-1";
      const cases: ReadonlyArray<readonly [RelayProtectedError, string]> = [
        [
          new RelayAuthInvalidError({
            code: "auth_invalid",
            reason: "missing_bearer",
            traceId,
          }),
          "Relay rejected the cloud session token.",
        ],
        [
          new RelayAuthInvalidError({
            code: "auth_invalid",
            reason: "invalid_bearer",
            traceId,
          }),
          "Relay rejected the cloud session token.",
        ],
        [
          new RelayAuthInvalidError({
            code: "auth_invalid",
            reason: "invalid_dpop",
            traceId,
          }),
          "Relay rejected the DPoP proof.",
        ],
        [
          new RelayAuthInvalidError({
            code: "auth_invalid",
            reason: "not_authorized",
            traceId,
          }),
          "Relay rejected the authenticated request.",
        ],
        [
          new RelayEnvironmentLinkProofExpiredError({
            code: "environment_link_proof_expired",
            traceId,
          }),
          "Relay rejected an expired environment link proof.",
        ],
        [
          new RelayEnvironmentLinkProofInvalidError({
            code: "environment_link_proof_invalid",
            reason: "descriptor_mismatch",
            traceId,
          }),
          "Relay rejected the environment link proof (descriptor_mismatch).",
        ],
        [
          new RelayEnvironmentConnectNotAuthorizedError({
            code: "environment_connect_not_authorized",
            traceId,
          }),
          "Relay rejected the environment connection request.",
        ],
        [
          new RelayEnvironmentEndpointUnavailableError({
            code: "environment_endpoint_unavailable",
            reason: "endpoint_request_failed",
            traceId,
          }),
          "Relay could not reach the environment endpoint (endpoint_request_failed).",
        ],
        [
          new RelayEnvironmentEndpointTimedOutError({
            code: "environment_endpoint_timed_out",
            traceId,
          }),
          "Relay timed out while contacting the environment endpoint.",
        ],
        [
          new RelayEnvironmentLinkFailedError({
            code: "environment_link_failed",
            reason: "link_persistence_failed",
            traceId,
          }),
          "Relay could not link the environment (link_persistence_failed).",
        ],
        [
          new RelayEnvironmentLinkUnavailableError({
            code: "environment_link_unavailable",
            reason: "managed_endpoint_not_configured",
            traceId,
          }),
          "Relay cannot provision the managed endpoint (managed_endpoint_not_configured).",
        ],
        [
          new RelayInternalError({
            code: "internal_error",
            reason: "database_unavailable",
            traceId,
          }),
          "Relay encountered an internal error (database_unavailable).",
        ],
      ];

      for (const [relayError, expectedDetail] of cases) {
        const cause = new ManagedRelay.ManagedRelayRequestFailedError({
          action: "create relay environment link challenge",
          cause: relayError,
          relayError,
          traceId,
        });
        const error = yield* withRelayService(
          linkPrimaryEnvironmentToCloud({ target: TARGET, clerkToken: "clerk-token" }),
          { createEnvironmentLinkChallenge: () => Effect.fail(cause) },
        ).pipe(Effect.flip);

        expect(error.message).toContain(expectedDetail);
        expect(error.traceId).toBe(traceId);
      }

      const unstructuredCause = new ManagedRelay.ManagedRelayRequestTimeoutError({
        activity: "Relay environment link challenge",
        timeoutMs: 10_000,
      });
      const unstructured = yield* withRelayService(
        linkPrimaryEnvironmentToCloud({ target: TARGET, clerkToken: "clerk-token" }),
        { createEnvironmentLinkChallenge: () => Effect.fail(unstructuredCause) },
      ).pipe(Effect.flip);
      expect(unstructured.message).toBe(
        "https://relay.example.test/v1/client/environment-link-challenges failed",
      );
      expect(unstructured.traceId).toBeUndefined();
    }),
  );

  it.effect("rejects relay credentials for the wrong environment or provider", () =>
    Effect.gen(function* () {
      const relayOverrides = (
        response: RelayEnvironmentLinkResponse,
      ): Partial<ManagedRelay.ManagedRelayClient["Service"]> => ({
        createEnvironmentLinkChallenge: () =>
          Effect.succeed({
            challenge: "challenge",
            expiresAt: "2026-06-06T00:05:00.000Z",
          }),
        linkEnvironment: () => Effect.succeed(response),
      });

      vi.stubGlobal(
        "fetch",
        vi.fn().mockImplementation(() => Promise.resolve(Response.json("signed-proof"))),
      );
      const wrongEnvironment = yield* withRelayService(
        linkPrimaryEnvironmentToCloud({
          target: {
            ...TARGET,
            httpBaseUrl: "https://desktop.local",
            wsBaseUrl: "wss://desktop.local",
          },
          clerkToken: "clerk-token",
        }),
        relayOverrides({ ...LINK_RESPONSE, environmentId: EnvironmentId.make("environment-2") }),
      ).pipe(Effect.flip);
      expect(wrongEnvironment.message).toBe(
        "Relay returned credentials for a different environment.",
      );

      const wrongProvider = yield* withRelayService(
        linkPrimaryEnvironmentToCloud({
          target: {
            ...TARGET,
            httpBaseUrl: "http://desktop.local",
            wsBaseUrl: "ws://desktop.local",
          },
          clerkToken: "clerk-token",
        }),
        relayOverrides({
          ...LINK_RESPONSE,
          endpoint: { ...LINK_RESPONSE.endpoint, providerKind: "manual" },
        }),
      ).pipe(Effect.flip);
      expect(wrongProvider.message).toBe(
        "Relay returned credentials for a different endpoint provider.",
      );
    }),
  );

  it.effect("reports missing relay configuration before linking", () =>
    Effect.gen(function* () {
      vi.stubEnv("VITE_BIBCODE_RELAY_URL", "");

      const error = yield* withServices(
        linkPrimaryEnvironmentToCloud({ target: TARGET, clerkToken: "clerk-token" }),
      ).pipe(Effect.flip);

      expect(error.message).toBe("BIBCODE_RELAY_URL is not configured.");
    }),
  );

  it.effect("installs a missing relay client before linking", () =>
    Effect.gen(function* () {
      vi.stubGlobal("fetch", vi.fn().mockResolvedValue(Response.json({ malformed: true })));

      yield* withServices(
        linkPrimaryEnvironmentToCloud({
          target: TARGET,
          clerkToken: "clerk-token",
        }),
        {
          status: { status: "missing", version: "2026.6.0" },
          installEvents: [
            { type: "progress", stage: "downloading" },
            {
              type: "complete",
              status: {
                status: "available",
                executablePath: "/usr/local/bin/bibcode-relay",
                source: "managed",
                version: "2026.6.0",
              },
            },
          ],
        },
      ).pipe(Effect.flip);

      expect(relayClientInstallDialog.requestConfirmation).toHaveBeenCalledWith("2026.6.0");
      expect(relayClientInstallDialog.reportProgress).toHaveBeenCalledTimes(2);
      expect(relayClientInstallDialog.finish).toHaveBeenCalledOnce();
    }),
  );

  it.effect("fails without prompting when a silent link finds no relay client", () =>
    Effect.gen(function* () {
      const error = yield* withServices(
        linkPrimaryEnvironmentToCloud({
          target: TARGET,
          clerkToken: "clerk-token",
          installRelayClient: false,
        }),
        { status: { status: "missing", version: "2026.6.0" } },
      ).pipe(Effect.flip);

      expect(error.message).toBe("The relay client is not installed.");
      expect(relayClientInstallDialog.requestConfirmation).not.toHaveBeenCalled();
    }),
  );

  it.effect("reports unsupported, cancelled, and incomplete relay client installs", () =>
    Effect.gen(function* () {
      const unsupported = yield* withServices(
        linkPrimaryEnvironmentToCloud({ target: TARGET, clerkToken: "clerk-token" }),
        {
          status: {
            status: "unsupported",
            platform: "freebsd",
            arch: "riscv64",
            version: "2026.6.0",
          },
        },
      ).pipe(Effect.flip);
      expect(unsupported.message).toContain("freebsd-riscv64");

      relayClientInstallDialog.requestConfirmation.mockResolvedValueOnce(false);
      const cancelled = yield* withServices(
        linkPrimaryEnvironmentToCloud({ target: TARGET, clerkToken: "clerk-token" }),
        { status: { status: "missing", version: "2026.6.0" } },
      ).pipe(Effect.flip);
      expect(cancelled.message).toBe("Relay client installation was cancelled.");

      const noFinalStatus = yield* withServices(
        linkPrimaryEnvironmentToCloud({ target: TARGET, clerkToken: "clerk-token" }),
        {
          status: { status: "missing", version: "2026.6.0" },
          installEvents: [{ type: "progress", stage: "validating" }],
        },
      ).pipe(Effect.flip);
      expect(noFinalStatus.message).toBe(
        "The relay client install completed without a final status.",
      );

      const stillMissing = yield* withServices(
        linkPrimaryEnvironmentToCloud({ target: TARGET, clerkToken: "clerk-token" }),
        {
          status: { status: "missing", version: "2026.6.0" },
          installEvents: [
            {
              type: "complete",
              status: { status: "missing", version: "2026.6.0" },
            },
          ],
        },
      ).pipe(Effect.flip);
      expect(stillMissing.message).toBe(
        "The relay client is still unavailable after installation.",
      );

      const installUnsupported = yield* withServices(
        linkPrimaryEnvironmentToCloud({ target: TARGET, clerkToken: "clerk-token" }),
        {
          status: { status: "missing", version: "2026.6.0" },
          installEvents: [
            {
              type: "complete",
              status: {
                status: "unsupported",
                platform: "linux",
                arch: "other",
                version: "2026.6.0",
              },
            },
          ],
        },
      ).pipe(Effect.flip);
      expect(installUnsupported.message).toContain("linux-other");
    }),
  );

  it.effect("wraps relay client status RPC failures", () =>
    Effect.gen(function* () {
      const cause = new Error("session closed");

      const error = yield* withServices(
        linkPrimaryEnvironmentToCloud({ target: TARGET, clerkToken: "clerk-token" }),
        { statusFailure: cause },
      ).pipe(Effect.flip);

      expect(error).toMatchObject({
        message: "Could not check relay client availability.",
        cause,
      });
    }),
  );

  it.effect("unlinks locally before revoking the relay record", () =>
    Effect.gen(function* () {
      const fetchMock = vi
        .fn()
        .mockResolvedValueOnce(
          Response.json({ ok: true, endpointRuntimeStatus: { status: "disabled" } }),
        )
        .mockResolvedValueOnce(Response.json({ ok: true }));
      vi.stubGlobal("fetch", fetchMock);

      yield* withServices(
        unlinkPrimaryEnvironmentFromCloud({
          target: TARGET,
          clerkToken: "clerk-token",
        }),
      );

      expect(String(fetchMock.mock.calls[0]?.[0])).toBe("http://127.0.0.1:3000/api/connect/unlink");
      expect(String(fetchMock.mock.calls[1]?.[0])).toContain(
        `/v1/client/environment-links/${TARGET.environmentId}`,
      );
    }),
  );

  it.effect("allows local unlink without a cloud token or configured relay", () =>
    Effect.gen(function* () {
      const fetchMock = vi
        .fn()
        .mockImplementation(() =>
          Promise.resolve(
            Response.json({ ok: true, endpointRuntimeStatus: { status: "disabled" } }),
          ),
        );
      vi.stubGlobal("fetch", fetchMock);

      yield* withServices(unlinkPrimaryEnvironmentFromCloud({ target: TARGET, clerkToken: null }));
      expect(fetchMock).toHaveBeenCalledOnce();

      vi.stubEnv("VITE_BIBCODE_RELAY_URL", "");
      yield* withServices(
        unlinkPrimaryEnvironmentFromCloud({ target: TARGET, clerkToken: "clerk-token" }),
      );
      expect(fetchMock).toHaveBeenCalledTimes(2);
    }),
  );

  it.effect("keeps local unlink successful when relay revocation fails", () =>
    Effect.gen(function* () {
      vi.stubGlobal(
        "fetch",
        vi
          .fn()
          .mockResolvedValue(
            Response.json({ ok: true, endpointRuntimeStatus: { status: "disabled" } }),
          ),
      );
      const cause = new ManagedRelay.ManagedRelayRequestTimeoutError({
        activity: "Relay environment unlinking",
        timeoutMs: 10_000,
      });

      yield* withRelayService(
        unlinkPrimaryEnvironmentFromCloud({ target: TARGET, clerkToken: "clerk-token" }),
        { unlinkEnvironment: () => Effect.fail(cause) },
      );
    }),
  );
});
