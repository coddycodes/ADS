import {
  EnvironmentId,
  type DesktopSshEnvironmentTarget,
  type ExecutionEnvironmentDescriptor,
} from "@coddy/contracts";
import { RelayEnvironmentConnectScope } from "@coddy/contracts/relay";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Ref from "effect/Ref";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";

import * as ManagedRelay from "../relay/managedRelay.ts";
import { fetchRemoteEnvironmentDescriptor } from "../environment/descriptor.ts";
import { remoteHttpClientLayer } from "../rpc/http.ts";
import * as ConnectionResolver from "./resolver.ts";
import * as ClientCapabilities from "../platform/capabilities.ts";
import * as RemoteEnvironmentAuthorization from "../authorization/service.ts";
import {
  BearerConnectionCredential,
  BearerConnectionProfile,
  type ConnectionCatalogEntry,
  SshConnectionProfile,
  type ConnectionCredential,
  type ConnectionProfile,
} from "./catalog.ts";
import * as ConnectionCredentialStore from "./credentialStore.ts";
import {
  BearerConnectionTarget,
  ConnectionBlockedError,
  ConnectionTransientError,
  PrimaryConnectionTarget,
  RelayConnectionTarget,
  SshConnectionTarget,
  type ConnectionTarget,
  UnavailableConnectionTarget,
} from "./model.ts";
import * as ConnectionProfileStore from "./profileStore.ts";

const ENVIRONMENT_ID = EnvironmentId.make("environment-1");
const ENDPOINT = {
  httpBaseUrl: "https://environment.example.test",
  wsBaseUrl: "wss://environment.example.test",
  providerKind: "cloudflare_tunnel" as const,
};
const DESCRIPTOR = {
  environmentId: ENVIRONMENT_ID,
  label: "Current environment",
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
const SSH_TARGET: DesktopSshEnvironmentTarget = {
  alias: "development",
  hostname: "development.example.test",
  username: "developer",
  port: 22,
};

function catalogEntry(
  target: ConnectionTarget,
  profile: Option.Option<ConnectionProfile> = Option.none(),
): ConnectionCatalogEntry {
  return { target, profile };
}

function unsupported<A>(name: string): Effect.Effect<A> {
  return Effect.die(new Error(`Unexpected relay call: ${name}`));
}

function relayClient(
  connectEnvironment: ManagedRelay.ManagedRelayClient["Service"]["connectEnvironment"],
) {
  return ManagedRelay.ManagedRelayClient.of({
    relayUrl: "https://relay.example.test",
    listEnvironments: () => unsupported("listEnvironments"),
    createEnvironmentLinkChallenge: () => unsupported("createEnvironmentLinkChallenge"),
    linkEnvironment: () => unsupported("linkEnvironment"),
    unlinkEnvironment: () => unsupported("unlinkEnvironment"),
    getEnvironmentStatus: () => unsupported("getEnvironmentStatus"),
    connectEnvironment,
    resetTokenCache: Effect.void,
  });
}

const makeDependencies = Effect.fn("TestConnectionResolver.makeDependencies")((options?: {
  readonly profiles?: ReadonlyArray<ConnectionProfile>;
  readonly credentials?: ReadonlyArray<readonly [string, ConnectionCredential]>;
  readonly connectEnvironment?: ManagedRelay.ManagedRelayClient["Service"]["connectEnvironment"];
  readonly authorizeBearer?: RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization["Service"]["authorizeBearer"];
  readonly authorizeDpop?: RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization["Service"]["authorizeDpop"];
  readonly primaryBearerToken?: string;
  readonly ensureSshTunnel?: ClientCapabilities.SshEnvironmentGateway["Service"]["ensureTunnel"];
  readonly mintSshBearer?: ClientCapabilities.SshEnvironmentGateway["Service"]["mintBearer"];
  readonly descriptor?: ExecutionEnvironmentDescriptor;
  /** Receives the URL of every HTTP request. */
  readonly httpRequests?: Array<string>;
  /** Shared with the test to observe credential writes. */
  readonly credentialMap?: Map<string, ConnectionCredential>;
  /** Connection ids still saved in the catalog; defaults to every profile. */
  readonly savedConnectionIds?: ReadonlySet<string>;
}) => {
  const profiles = new Map(
    (options?.profiles ?? []).map((profile) => [profile.connectionId, profile]),
  );
  const credentials = options?.credentialMap ?? new Map(options?.credentials ?? []);
  const savedConnectionIds = options?.savedConnectionIds ?? new Set(profiles.keys());

  const profileStore = ConnectionProfileStore.ConnectionProfileStore.of({
    get: (connectionId) => Effect.succeed(Option.fromNullishOr(profiles.get(connectionId))),
    put: (profile) => Effect.sync(() => void profiles.set(profile.connectionId, profile)),
    remove: (connectionId) => Effect.sync(() => void profiles.delete(connectionId)),
  });
  const credentialStore = ConnectionCredentialStore.ConnectionCredentialStore.of({
    get: (connectionId) => Effect.succeed(Option.fromNullishOr(credentials.get(connectionId))),
    put: (connectionId, credential) =>
      Effect.sync(() => void credentials.set(connectionId, credential)),
    remove: (connectionId) => Effect.sync(() => void credentials.delete(connectionId)),
    putIfSaved: (connectionId, credential) =>
      Effect.sync(() => {
        if (!savedConnectionIds.has(connectionId)) return false;
        credentials.set(connectionId, credential);
        return true;
      }),
  });
  const remote = RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization.of({
    authorizeBearer:
      options?.authorizeBearer ??
      ((input) =>
        Effect.succeed({
          descriptor: options?.descriptor ?? DESCRIPTOR,
          environmentId: input.expectedEnvironmentId,
          label: "Authorized bearer environment",
          httpBaseUrl: input.httpBaseUrl,
          socketUrl: "wss://authorized.example.test/ws?wsTicket=bearer",
          httpAuthorization: {
            _tag: "Bearer" as const,
            token: input.bearerToken,
          },
          e2ee: null,
        })),
    authorizeDpop:
      options?.authorizeDpop ??
      ((input) =>
        input.obtainBootstrap.pipe(
          Effect.as({
            descriptor: options?.descriptor ?? DESCRIPTOR,
            environmentId: input.expectedEnvironmentId,
            label: "Authorized relay environment",
            httpBaseUrl: ENDPOINT.httpBaseUrl,
            socketUrl: "wss://authorized.example.test/ws?wsTicket=dpop",
            httpAuthorization: {
              _tag: "Dpop" as const,
              accessToken: "dpop-access-token",
            },
            e2ee: null,
          }),
        )),
  });
  const ssh = ClientCapabilities.SshEnvironmentGateway.of({
    provision: () => Effect.die("unused"),
    ensureTunnel:
      options?.ensureSshTunnel ??
      ((input) =>
        Effect.succeed({
          target: input.target,
          httpBaseUrl: "http://127.0.0.1:4010",
          wsBaseUrl: "ws://127.0.0.1:4010",
          pairingToken: null,
        })),
    mintBearer:
      options?.mintSshBearer ??
      ((input) =>
        Effect.succeed({
          bootstrap: {
            target: input.target,
            httpBaseUrl: "http://127.0.0.1:4010",
            wsBaseUrl: "ws://127.0.0.1:4010",
            pairingToken: "pairing-token",
          },
          bearerToken: "ssh-bearer",
        })),
    disconnect: () => Effect.void,
  });

  const dependencies = Layer.mergeAll(
    remoteHttpClientLayer((input) => {
      options?.httpRequests?.push(String(input instanceof Request ? input.url : input));
      return Promise.resolve(Response.json(options?.descriptor ?? DESCRIPTOR));
    }),
    Layer.succeed(ConnectionProfileStore.ConnectionProfileStore, profileStore),
    Layer.succeed(ConnectionCredentialStore.ConnectionCredentialStore, credentialStore),
    Layer.succeed(
      ClientCapabilities.CloudSession,
      ClientCapabilities.CloudSession.of({ clerkToken: Effect.succeed("clerk-session") }),
    ),
    Layer.succeed(
      ClientCapabilities.PrimaryEnvironmentAuth,
      ClientCapabilities.PrimaryEnvironmentAuth.of({
        bearerToken: Effect.succeed(Option.fromNullishOr(options?.primaryBearerToken)),
      }),
    ),
    Layer.succeed(RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization, remote),
    Layer.succeed(ClientCapabilities.SshEnvironmentGateway, ssh),
    Layer.succeed(
      ManagedRelay.ManagedRelayClient,
      relayClient(
        options?.connectEnvironment ??
          ((input) =>
            Effect.succeed({
              environmentId: input.environmentId,
              endpoint: ENDPOINT,
              credential: "relay-bootstrap",
              expiresAt: "2026-06-06T00:00:00.000Z",
            })),
      ),
    ),
  );

  return Effect.succeed(ConnectionResolver.layer.pipe(Layer.provide(dependencies)));
});

describe("ConnectionResolver", () => {
  it.effect("rejects an unavailable desired environment before resolving any endpoint", () =>
    Effect.gen(function* () {
      const brokerLayer = yield* makeDependencies();
      const broker = yield* ConnectionResolver.ConnectionResolver.pipe(Effect.provide(brokerLayer));
      const target = new UnavailableConnectionTarget({
        environmentId: ENVIRONMENT_ID,
        label: "WSL (Ubuntu)",
        connectionId: "local:wsl:Ubuntu",
        configuredDistro: "Ubuntu",
        detail: "the configured WSL distribution could not start",
      });

      const error = yield* broker.prepare(catalogEntry(target)).pipe(Effect.flip);

      expect(error).toBeInstanceOf(ConnectionTransientError);
      expect(error).toMatchObject({
        reason: "endpoint-unavailable",
        detail: "the configured WSL distribution could not start",
      });
    }),
  );

  it.effect("prepares a primary environment without remote capabilities", () =>
    Effect.gen(function* () {
      const brokerLayer = yield* makeDependencies();
      const broker = yield* ConnectionResolver.ConnectionResolver.pipe(Effect.provide(brokerLayer));
      const target = new PrimaryConnectionTarget({
        environmentId: ENVIRONMENT_ID,
        label: "Primary",
        httpBaseUrl: "http://127.0.0.1:3777",
        wsBaseUrl: "ws://127.0.0.1:3777",
      });

      expect(yield* broker.prepare(catalogEntry(target))).toEqual({
        environmentId: ENVIRONMENT_ID,
        label: DESCRIPTOR.label,
        descriptor: DESCRIPTOR,
        httpBaseUrl: "http://127.0.0.1:3777",
        socketUrl: "ws://127.0.0.1:3777/ws",
        httpAuthorization: null,
        e2ee: null,
        target,
      });
    }),
  );

  it.effect("takes the descriptor the platform just fetched for its first attempt only", () =>
    Effect.gen(function* () {
      const httpRequests: Array<string> = [];
      const brokerLayer = yield* makeDependencies({ httpRequests });
      const broker = yield* ConnectionResolver.ConnectionResolver.pipe(Effect.provide(brokerLayer));
      const httpBaseUrl = "http://127.0.0.1:3778";
      const target = new PrimaryConnectionTarget({
        environmentId: ENVIRONMENT_ID,
        label: "Primary",
        httpBaseUrl,
        wsBaseUrl: "ws://127.0.0.1:3778",
      });
      // The platform registers the environment from this fetch.
      yield* fetchRemoteEnvironmentDescriptor({ httpBaseUrl, handOff: true }).pipe(
        Effect.provide(
          remoteHttpClientLayer((input) => {
            httpRequests.push(String(input instanceof Request ? input.url : input));
            return Promise.resolve(Response.json(DESCRIPTOR));
          }),
        ),
      );
      expect(httpRequests).toHaveLength(1);

      // The first attempt follows at once: no second round trip for the same descriptor.
      expect((yield* broker.prepare(catalogEntry(target))).descriptor).toEqual(DESCRIPTOR);
      expect(httpRequests).toHaveLength(1);

      // A later attempt (a reconnect) verifies a fresh descriptor.
      yield* broker.prepare(catalogEntry(target));
      expect(httpRequests).toHaveLength(2);
    }),
  );

  it.effect("authorizes a desktop primary environment with its platform bearer token", () =>
    Effect.gen(function* () {
      const bearerInputs = yield* Ref.make<ReadonlyArray<string>>([]);
      const brokerLayer = yield* makeDependencies({
        primaryBearerToken: "desktop-bearer",
        authorizeBearer: (input) =>
          Ref.update(bearerInputs, (values) => [...values, input.bearerToken]).pipe(
            Effect.as({
              descriptor: DESCRIPTOR,
              environmentId: input.expectedEnvironmentId,
              label: "Primary",
              httpBaseUrl: input.httpBaseUrl,
              socketUrl: "ws://127.0.0.1:3777/ws?wsTicket=desktop",
              httpAuthorization: {
                _tag: "Bearer" as const,
                token: input.bearerToken,
              },
              e2ee: null,
            }),
          ),
      });
      const broker = yield* ConnectionResolver.ConnectionResolver.pipe(Effect.provide(brokerLayer));
      const target = new PrimaryConnectionTarget({
        environmentId: ENVIRONMENT_ID,
        label: "Primary",
        httpBaseUrl: "http://127.0.0.1:3777",
        wsBaseUrl: "ws://127.0.0.1:3777",
      });

      expect(yield* broker.prepare(catalogEntry(target))).toMatchObject({
        descriptor: DESCRIPTOR,
        socketUrl: "ws://127.0.0.1:3777/ws?wsTicket=desktop",
        httpAuthorization: { _tag: "Bearer", token: "desktop-bearer" },
        target,
      });
      expect(yield* Ref.get(bearerInputs)).toEqual(["desktop-bearer"]);
    }),
  );

  it.effect("uses the registered bearer profile without re-reading the profile store", () =>
    Effect.gen(function* () {
      const bearerInputs = yield* Ref.make<ReadonlyArray<string>>([]);
      const target = new BearerConnectionTarget({
        environmentId: ENVIRONMENT_ID,
        label: "Saved",
        connectionId: "saved-1",
        serverEnvironmentId: null,
      });
      const profile = new BearerConnectionProfile({
        connectionId: "saved-1",
        environmentId: ENVIRONMENT_ID,
        label: "Saved",
        httpBaseUrl: ENDPOINT.httpBaseUrl,
        wsBaseUrl: ENDPOINT.wsBaseUrl,
        hostKey: null,
      });
      const brokerLayer = yield* makeDependencies({
        credentials: [["saved-1", new BearerConnectionCredential({ token: "secret-bearer" })]],
        authorizeBearer: (input) =>
          Ref.update(bearerInputs, (values) => [...values, input.bearerToken]).pipe(
            Effect.as({
              descriptor: DESCRIPTOR,
              environmentId: input.expectedEnvironmentId,
              label: "Saved",
              httpBaseUrl: input.httpBaseUrl,
              socketUrl: "wss://environment.example.test/ws?wsTicket=ticket",
              httpAuthorization: {
                _tag: "Bearer" as const,
                token: input.bearerToken,
              },
              e2ee: null,
            }),
          ),
      });
      const broker = yield* ConnectionResolver.ConnectionResolver.pipe(Effect.provide(brokerLayer));

      const prepared = yield* broker.prepare(catalogEntry(target, Option.some(profile)));
      expect(prepared.socketUrl).toContain("wsTicket=ticket");
      expect(prepared.e2ee).toBeNull();
      expect(prepared.descriptor).toEqual(DESCRIPTOR);
      expect(yield* Ref.get(bearerInputs)).toEqual(["secret-bearer"]);
    }),
  );

  it.effect("selects the E2EE channel for a host-key bearer profile", () =>
    Effect.gen(function* () {
      const hostKey = "HcMLXPPBHFNvcbHrCVMH-DMh49rd5AGCzSCqAVJ49hM";
      const target = new BearerConnectionTarget({
        environmentId: ENVIRONMENT_ID,
        label: "Pinned",
        connectionId: "saved-e2ee",
        serverEnvironmentId: null,
      });
      const profile = new BearerConnectionProfile({
        connectionId: target.connectionId,
        environmentId: ENVIRONMENT_ID,
        label: target.label,
        httpBaseUrl: ENDPOINT.httpBaseUrl,
        wsBaseUrl: ENDPOINT.wsBaseUrl,
        hostKey,
      });
      const brokerLayer = yield* makeDependencies({
        credentials: [
          [target.connectionId, new BearerConnectionCredential({ token: "stored-secret" })],
        ],
        authorizeBearer: (input) =>
          Effect.succeed({
            descriptor: DESCRIPTOR,
            environmentId: input.expectedEnvironmentId,
            label: target.label,
            httpBaseUrl: input.httpBaseUrl,
            socketUrl: "wss://environment.example.test/ws-e2ee",
            httpAuthorization: { _tag: "Bearer" as const, token: input.bearerToken },
            e2ee:
              input.hostKey === null || input.hostKey === undefined
                ? null
                : {
                    hostKey: input.hostKey,
                    auth: { kind: "bearer" as const, credential: input.bearerToken },
                  },
          }),
      });
      const broker = yield* ConnectionResolver.ConnectionResolver.pipe(Effect.provide(brokerLayer));

      const prepared = yield* broker.prepare(catalogEntry(target, Option.some(profile)));

      expect(prepared.socketUrl).toBe("wss://environment.example.test/ws-e2ee");
      expect(new URL(prepared.socketUrl).search).toBe("");
      expect(prepared.e2ee).toEqual({
        hostKey,
        auth: { kind: "bearer", credential: "stored-secret" },
      });
      expect(prepared.httpAuthorization).toBeNull();
    }),
  );

  it.effect("brokers relay credentials with the current cloud session", () =>
    Effect.gen(function* () {
      const relayInputs = yield* Ref.make<
        ReadonlyArray<{
          readonly clerkToken: string;
          readonly scopes: ReadonlyArray<string>;
        }>
      >([]);
      const bootstrapCredentials = yield* Ref.make<ReadonlyArray<string>>([]);
      const target = new RelayConnectionTarget({
        environmentId: ENVIRONMENT_ID,
        label: "Cloud",
      });
      const brokerLayer = yield* makeDependencies({
        connectEnvironment: (input) =>
          Ref.update(relayInputs, (values) => [
            ...values,
            {
              clerkToken: input.clerkToken,
              scopes: input.scopes,
            },
          ]).pipe(
            Effect.as({
              environmentId: input.environmentId,
              endpoint: ENDPOINT,
              credential: "relay-bootstrap",
              expiresAt: "2026-06-06T00:00:00.000Z",
            }),
          ),
        authorizeDpop: (input) =>
          input.obtainBootstrap.pipe(
            Effect.tap((bootstrap) =>
              Ref.update(bootstrapCredentials, (values) => [...values, bootstrap.credential]),
            ),
            Effect.as({
              descriptor: DESCRIPTOR,
              environmentId: input.expectedEnvironmentId,
              label: "Cloud",
              httpBaseUrl: ENDPOINT.httpBaseUrl,
              socketUrl: "wss://environment.example.test/ws?wsTicket=dpop",
              httpAuthorization: {
                _tag: "Dpop" as const,
                accessToken: "dpop-access-token",
              },
              e2ee: null,
            }),
          ),
      });
      const broker = yield* ConnectionResolver.ConnectionResolver.pipe(Effect.provide(brokerLayer));

      const prepared = yield* broker.prepare(catalogEntry(target));
      expect(prepared.socketUrl).toContain("wsTicket=dpop");
      expect(prepared.descriptor).toEqual(DESCRIPTOR);
      expect(yield* Ref.get(relayInputs)).toEqual([
        {
          clerkToken: "clerk-session",
          scopes: [RelayEnvironmentConnectScope],
        },
      ]);
      expect(yield* Ref.get(bootstrapCredentials)).toEqual(["relay-bootstrap"]);
    }),
  );

  it.effect("classifies relay request timeouts as retryable connection failures", () =>
    Effect.gen(function* () {
      const target = new RelayConnectionTarget({
        environmentId: ENVIRONMENT_ID,
        label: "Cloud",
      });
      const brokerLayer = yield* makeDependencies({
        connectEnvironment: () =>
          Effect.fail(
            new ManagedRelay.ManagedRelayRequestTimeoutError({
              activity: "Relay environment connection",
              timeoutMs: ManagedRelay.MANAGED_RELAY_REQUEST_TIMEOUT_MS,
            }),
          ),
      });
      const broker = yield* ConnectionResolver.ConnectionResolver.pipe(Effect.provide(brokerLayer));
      const error = yield* Effect.flip(broker.prepare(catalogEntry(target)));

      expect(error).toBeInstanceOf(ConnectionTransientError);
      expect(error).toMatchObject({ reason: "timeout" });
    }),
  );
});

describe("ConnectionResolver SSH credentials", () => {
  const target = new SshConnectionTarget({
    environmentId: ENVIRONMENT_ID,
    label: "SSH",
    connectionId: "ssh-1",
  });
  const profile = new SshConnectionProfile({
    connectionId: "ssh-1",
    environmentId: ENVIRONMENT_ID,
    label: "SSH",
    target: SSH_TARGET,
  });
  const rejected = () =>
    new ConnectionBlockedError({
      reason: "authentication",
      detail: "The environment credential is invalid.",
    });

  const setup = Effect.fn("TestConnectionResolver.ssh.setup")(function* (options: {
    readonly saved?: string;
    readonly rejectTokens?: (token: string) => boolean;
    readonly ensureSshTunnel?: ClientCapabilities.SshEnvironmentGateway["Service"]["ensureTunnel"];
    readonly savedConnectionIds?: ReadonlySet<string>;
  }) {
    const credentialMap = new Map<string, ConnectionCredential>(
      options.saved === undefined
        ? []
        : [["ssh-1", new BearerConnectionCredential({ token: options.saved })]],
    );
    const tunnels = yield* Ref.make<ReadonlyArray<DesktopSshEnvironmentTarget>>([]);
    const mints = yield* Ref.make(0);
    const authorized = yield* Ref.make<ReadonlyArray<string>>([]);
    const layer = yield* makeDependencies({
      profiles: [profile],
      credentialMap,
      ...(options.savedConnectionIds === undefined
        ? {}
        : { savedConnectionIds: options.savedConnectionIds }),
      ensureSshTunnel:
        options.ensureSshTunnel ??
        ((input) =>
          Ref.update(tunnels, (values) => [...values, input.target]).pipe(
            Effect.as({
              target: input.target,
              httpBaseUrl: "http://127.0.0.1:4010",
              wsBaseUrl: "ws://127.0.0.1:4010",
              pairingToken: null,
            }),
          )),
      mintSshBearer: (input) =>
        Ref.updateAndGet(mints, (count) => count + 1).pipe(
          Effect.map((count) => ({
            bootstrap: {
              target: input.target,
              httpBaseUrl: "http://127.0.0.1:4010",
              wsBaseUrl: "ws://127.0.0.1:4010",
              pairingToken: `pairing-${count}`,
            },
            bearerToken: `minted-${count}`,
          })),
        ),
      authorizeBearer: (input) =>
        Ref.update(authorized, (values) => [...values, input.bearerToken]).pipe(
          Effect.andThen(
            options.rejectTokens?.(input.bearerToken) === true
              ? Effect.fail(rejected())
              : Effect.succeed({
                  descriptor: DESCRIPTOR,
                  environmentId: input.expectedEnvironmentId,
                  label: "Authorized SSH environment",
                  httpBaseUrl: input.httpBaseUrl,
                  socketUrl: "wss://authorized.example.test/ws?wsTicket=bearer",
                  httpAuthorization: { _tag: "Bearer" as const, token: input.bearerToken },
                  e2ee: null,
                }),
          ),
        ),
    });
    const broker = yield* ConnectionResolver.ConnectionResolver.pipe(Effect.provide(layer));
    return {
      prepare: broker.prepare(catalogEntry(target, Option.some(profile))),
      savedToken: () => {
        const credential = credentialMap.get("ssh-1");
        return credential === undefined ? null : credential.token;
      },
      tunnels: Ref.get(tunnels),
      mints: Ref.get(mints),
      authorized: Ref.get(authorized),
    };
  });

  it.effect("connects with the saved bearer through the tunnel without minting", () =>
    Effect.gen(function* () {
      const ssh = yield* setup({ saved: "saved-bearer" });

      const prepared = yield* ssh.prepare;

      expect(prepared.socketUrl).toContain("wsTicket=bearer");
      expect(yield* ssh.tunnels).toEqual([SSH_TARGET]);
      expect(yield* ssh.authorized).toEqual(["saved-bearer"]);
      expect(yield* ssh.mints).toBe(0);
    }),
  );

  it.effect("mints once and saves the bearer when no credential is saved", () =>
    Effect.gen(function* () {
      const ssh = yield* setup({});

      yield* ssh.prepare;

      expect(yield* ssh.mints).toBe(1);
      expect(yield* ssh.authorized).toEqual(["minted-1"]);
      expect(ssh.savedToken()).toBe("minted-1");
    }),
  );

  it.effect("mints once and retries when the saved bearer is rejected", () =>
    Effect.gen(function* () {
      const ssh = yield* setup({
        saved: "revoked-bearer",
        rejectTokens: (token) => token === "revoked-bearer",
      });

      yield* ssh.prepare;

      expect(yield* ssh.authorized).toEqual(["revoked-bearer", "minted-1"]);
      expect(yield* ssh.mints).toBe(1);
      expect(ssh.savedToken()).toBe("minted-1");
    }),
  );

  it.effect(
    "blocks until the user connects again when the freshly minted bearer is rejected too",
    () =>
      Effect.gen(function* () {
        const ssh = yield* setup({ saved: "revoked-bearer", rejectTokens: () => true });

        const error = yield* Effect.flip(ssh.prepare);

        expect(error).toBeInstanceOf(ConnectionBlockedError);
        expect(error).toMatchObject({
          reason: "authentication",
          detail:
            "development rejected a new pairing credential. Connect again; if it keeps failing, remove the environment and add it again.",
        });
        expect(yield* ssh.mints).toBe(1);
        expect(yield* ssh.authorized).toEqual(["revoked-bearer", "minted-1"]);
      }),
  );

  it.effect("keeps the saved bearer when the tunnel fails transiently", () =>
    Effect.gen(function* () {
      const ssh = yield* setup({
        saved: "saved-bearer",
        ensureSshTunnel: () =>
          Effect.fail(
            new ConnectionTransientError({
              reason: "remote-unavailable",
              detail: "Could not prepare the SSH environment: connection refused",
            }),
          ),
      });

      const error = yield* Effect.flip(ssh.prepare);

      expect(error).toBeInstanceOf(ConnectionTransientError);
      expect(ssh.savedToken()).toBe("saved-bearer");
      expect(yield* ssh.mints).toBe(0);
      expect(yield* ssh.authorized).toEqual([]);
    }),
  );

  it.effect("does not save a minted bearer for an environment removed meanwhile", () =>
    Effect.gen(function* () {
      const ssh = yield* setup({ savedConnectionIds: new Set() });

      yield* ssh.prepare;

      expect(yield* ssh.mints).toBe(1);
      expect(ssh.savedToken()).toBeNull();
    }),
  );
});
