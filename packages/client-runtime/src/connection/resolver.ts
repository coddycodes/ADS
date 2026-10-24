import { RelayEnvironmentConnectScope } from "@bibcode/contracts/relay";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as HttpClient from "effect/http/HttpClient";

import * as RemoteEnvironmentAuthorization from "../authorization/service.ts";
import * as ManagedRelay from "../relay/managedRelay.ts";
import * as ClientCapabilities from "../platform/capabilities.ts";
import { fetchRemoteEnvironmentDescriptor } from "../environment/descriptor.ts";
import {
  BearerConnectionCredential,
  BearerConnectionProfile,
  type ConnectionCatalogEntry,
  SshConnectionProfile,
} from "./catalog.ts";
import * as ConnectionCredentialStore from "./credentialStore.ts";
import {
  credentialMissingError,
  environmentMismatchError,
  isAuthenticationRejection,
  mapManagedRelayError,
  mapRemoteEnvironmentError,
  profileMissingError,
  sshCredentialRejectedError,
} from "./errors.ts";
import type {
  BearerConnectionTarget,
  ConnectionTarget,
  PreparedConnection,
  PrimaryConnectionTarget,
  RelayConnectionTarget,
  SshConnectionTarget,
} from "./model.ts";
import {
  ConnectionBlockedError,
  ConnectionTransientError,
  type ConnectionAttemptError,
} from "./model.ts";
import { bearerServerEnvironmentId } from "./remoteIdentity.ts";
import * as ConnectionProfileStore from "./profileStore.ts";

export class ConnectionResolver extends Context.Service<
  ConnectionResolver,
  {
    readonly prepare: (
      entry: ConnectionCatalogEntry,
    ) => Effect.Effect<PreparedConnection, ConnectionAttemptError>;
  }
>()("@bibcode/client-runtime/connection/resolver/ConnectionResolver") {}

const isBearerProfile = Schema.is(BearerConnectionProfile);
const isSshProfile = Schema.is(SshConnectionProfile);
const isBearerCredential = Schema.is(BearerConnectionCredential);

function primarySocketUrl(target: PrimaryConnectionTarget): string {
  const url = new URL(target.wsBaseUrl);
  if (url.pathname === "" || url.pathname === "/") {
    url.pathname = "/ws";
  }
  return url.toString();
}

const makePrimaryBroker = Effect.fn("clientRuntime.connection.broker.makePrimary")(function* () {
  const auth = yield* ClientCapabilities.PrimaryEnvironmentAuth;
  const remote = yield* RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization;
  const httpClient = yield* HttpClient.HttpClient;

  return Effect.fn("clientRuntime.connection.broker.primary")(function* (
    target: PrimaryConnectionTarget,
  ) {
    const bearerToken = yield* auth.bearerToken;
    if (Option.isNone(bearerToken)) {
      // The platform fetched this descriptor to register the environment; the first
      // attempt reuses it instead of paying another round trip.
      const descriptor = yield* fetchRemoteEnvironmentDescriptor({
        httpBaseUrl: target.httpBaseUrl,
        reuseHandOff: true,
      }).pipe(
        Effect.mapError(mapRemoteEnvironmentError),
        Effect.provideService(HttpClient.HttpClient, httpClient),
      );
      if (descriptor.environmentId !== target.environmentId) {
        return yield* environmentMismatchError({
          expected: target.environmentId,
          actual: descriptor.environmentId,
        });
      }
      return {
        environmentId: descriptor.environmentId,
        label: descriptor.label,
        descriptor,
        httpBaseUrl: target.httpBaseUrl,
        socketUrl: primarySocketUrl(target),
        httpAuthorization: null,
        e2ee: null,
        target,
      } satisfies PreparedConnection;
    }

    const authorized = yield* remote.authorizeBearer({
      expectedEnvironmentId: target.environmentId,
      httpBaseUrl: target.httpBaseUrl,
      wsBaseUrl: target.wsBaseUrl,
      bearerToken: bearerToken.value,
    });
    return {
      ...authorized,
      target,
    } satisfies PreparedConnection;
  });
});

const makeBearerBroker = Effect.fn("clientRuntime.connection.broker.makeBearer")(function* () {
  const credentials = yield* ConnectionCredentialStore.ConnectionCredentialStore;
  const remote = yield* RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization;

  return Effect.fn("clientRuntime.connection.broker.bearer")(function* (
    entry: ConnectionCatalogEntry & { readonly target: BearerConnectionTarget },
  ) {
    const target = entry.target;
    const profile = yield* Option.match(entry.profile, {
      onNone: () => Effect.fail(profileMissingError(target.connectionId)),
      onSome: Effect.succeed,
    });
    if (!isBearerProfile(profile)) {
      return yield* new ConnectionBlockedError({
        reason: "configuration",
        detail: `Connection profile ${target.connectionId} is not a bearer connection.`,
      });
    }
    if (profile.environmentId !== target.environmentId) {
      return yield* environmentMismatchError({
        expected: target.environmentId,
        actual: profile.environmentId,
      });
    }
    const credential = yield* credentials.get(target.connectionId).pipe(
      Effect.flatMap(
        Option.match({
          onNone: () => Effect.fail(credentialMissingError(target.connectionId)),
          onSome: Effect.succeed,
        }),
      ),
    );
    if (!isBearerCredential(credential)) {
      return yield* credentialMissingError(target.connectionId);
    }
    const authorized = yield* remote.authorizeBearer({
      // The host declares its own id; the registry key is the client's. For a
      // paired remote those differ, so the endpoint is checked against what it
      // said when it was saved.
      expectedEnvironmentId: bearerServerEnvironmentId(target),
      httpBaseUrl: profile.httpBaseUrl,
      wsBaseUrl: profile.wsBaseUrl,
      bearerToken: credential.token,
      hostKey: profile.hostKey,
    });
    return {
      environmentId: target.environmentId,
      label: authorized.label,
      descriptor: authorized.descriptor,
      httpBaseUrl: authorized.httpBaseUrl,
      socketUrl: authorized.socketUrl,
      httpAuthorization: authorized.e2ee === null ? authorized.httpAuthorization : null,
      e2ee: authorized.e2ee,
      target,
    } satisfies PreparedConnection;
  });
});

const makeRelayBroker = Effect.fn("clientRuntime.connection.broker.makeRelay")(function* () {
  const relay = yield* ManagedRelay.ManagedRelayClient;
  const session = yield* ClientCapabilities.CloudSession;
  const remote = yield* RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization;

  return Effect.fnUntraced(function* (target: RelayConnectionTarget) {
    const authorized = yield* remote.authorizeDpop({
      expectedEnvironmentId: target.environmentId,
      obtainBootstrap: Effect.gen(function* () {
        const clerkToken = yield* session.clerkToken.pipe(
          Effect.withSpan("relay.connection.cloudSessionToken.resolve"),
        );
        const connected = yield* relay
          .connectEnvironment({
            clerkToken,
            scopes: [RelayEnvironmentConnectScope],
            environmentId: target.environmentId,
          })
          .pipe(Effect.mapError(mapManagedRelayError));
        if (connected.environmentId !== target.environmentId) {
          return yield* environmentMismatchError({
            expected: target.environmentId,
            actual: connected.environmentId,
          });
        }
        return connected;
      }).pipe(Effect.withSpan("relay.connection.bootstrap.obtain")),
    });
    return {
      environmentId: authorized.environmentId,
      label: authorized.label,
      descriptor: authorized.descriptor,
      httpBaseUrl: authorized.httpBaseUrl,
      socketUrl: authorized.socketUrl,
      httpAuthorization: authorized.httpAuthorization,
      e2ee: null,
      target,
    } satisfies PreparedConnection;
  }, Effect.withSpan("clientRuntime.connection.broker.relay"));
});

const makeSshBroker = Effect.fn("clientRuntime.connection.broker.makeSsh")(function* () {
  const profiles = yield* ConnectionProfileStore.ConnectionProfileStore;
  const credentials = yield* ConnectionCredentialStore.ConnectionCredentialStore;
  const ssh = yield* ClientCapabilities.SshEnvironmentGateway;
  const remote = yield* RemoteEnvironmentAuthorization.RemoteEnvironmentAuthorization;

  return Effect.fn("clientRuntime.connection.broker.ssh")(function* (
    entry: ConnectionCatalogEntry & { readonly target: SshConnectionTarget },
  ) {
    const target = entry.target;
    const profile = yield* Option.match(entry.profile, {
      onNone: () => Effect.fail(profileMissingError(target.connectionId)),
      onSome: Effect.succeed,
    });
    if (!isSshProfile(profile)) {
      return yield* new ConnectionBlockedError({
        reason: "configuration",
        detail: `Connection profile ${target.connectionId} is not an SSH connection.`,
      });
    }
    if (profile.environmentId !== target.environmentId) {
      return yield* environmentMismatchError({
        expected: target.environmentId,
        actual: profile.environmentId,
      });
    }
    const input = {
      connectionId: target.connectionId,
      expectedEnvironmentId: target.environmentId,
      target: profile.target,
    };
    // A live tunnel is reused without running SSH; the pairing token that
    // came with it is spent, so the saved bearer authenticates instead.
    const bootstrap = yield* ssh.ensureTunnel(input);
    yield* profiles.put(
      new SshConnectionProfile({
        connectionId: profile.connectionId,
        environmentId: profile.environmentId,
        label: profile.label,
        target: bootstrap.target,
      }),
    );
    const authorize = (endpoint: typeof bootstrap, bearerToken: string) =>
      remote.authorizeBearer({
        expectedEnvironmentId: target.environmentId,
        httpBaseUrl: endpoint.httpBaseUrl,
        wsBaseUrl: endpoint.wsBaseUrl,
        bearerToken,
      });
    const host = profile.target.alias || profile.target.hostname;
    const blockOnRejection = (error: ConnectionAttemptError): ConnectionAttemptError =>
      isAuthenticationRejection(error) ? sshCredentialRejectedError(host, error.traceId) : error;
    // One mint per attempt: SSH access is the authority, so a missing or
    // rejected bearer is replaced, but a rejection of the replacement blocks.
    const mintAndAuthorize = Effect.gen(function* () {
      // The gateway classifies its own failures: a cancelled password prompt
      // stays as it is, and it reports a refused exchange with the same copy.
      const minted = yield* ssh.mintBearer(input);
      yield* credentials.putIfSaved(
        target.connectionId,
        new BearerConnectionCredential({ token: minted.bearerToken }),
      );
      return yield* authorize(minted.bootstrap, minted.bearerToken).pipe(
        Effect.mapError(blockOnRejection),
      );
    });
    const saved = yield* credentials.get(target.connectionId);
    const authorized =
      Option.isSome(saved) && isBearerCredential(saved.value)
        ? yield* authorize(bootstrap, saved.value.token).pipe(
            Effect.catchIf(isAuthenticationRejection, () => mintAndAuthorize),
          )
        : yield* mintAndAuthorize;
    return {
      environmentId: authorized.environmentId,
      label: authorized.label,
      descriptor: authorized.descriptor,
      httpBaseUrl: authorized.httpBaseUrl,
      socketUrl: authorized.socketUrl,
      httpAuthorization: authorized.httpAuthorization,
      e2ee: null,
      target,
    } satisfies PreparedConnection;
  });
});

export const make = Effect.gen(function* () {
  const primary = yield* makePrimaryBroker();
  const bearer = yield* makeBearerBroker();
  const relay = yield* makeRelayBroker();
  const ssh = yield* makeSshBroker();

  const prepare = Effect.fn("clientRuntime.connection.broker.prepare")(function* (
    entry: ConnectionCatalogEntry,
  ) {
    const target: ConnectionTarget = entry.target;
    yield* Effect.annotateCurrentSpan({
      "connection.environment.id": target.environmentId,
      "connection.target.kind": target._tag,
    });
    switch (target._tag) {
      case "PrimaryConnectionTarget":
        return yield* primary(target);
      case "BearerConnectionTarget":
        return yield* bearer({ ...entry, target });
      case "RelayConnectionTarget":
        return yield* relay(target);
      case "SshConnectionTarget":
        return yield* ssh({ ...entry, target });
      case "UnavailableConnectionTarget":
        return yield* new ConnectionTransientError({
          reason: "endpoint-unavailable",
          detail: target.detail,
        });
    }
  });

  return ConnectionResolver.of({ prepare });
});

export const layer = Layer.effect(ConnectionResolver, make);
