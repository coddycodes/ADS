import { EnvironmentId } from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

import * as TokenStore from "../authorization/tokenStore.ts";
import {
  BearerConnectionCredential,
  BearerConnectionProfile,
  BearerConnectionRegistration,
  RelayConnectionRegistration,
  SshConnectionProfile,
  SshConnectionRegistration,
} from "../connection/catalog.ts";
import {
  BearerConnectionTarget,
  RelayConnectionTarget,
  SshConnectionTarget,
} from "../connection/model.ts";
import {
  ConnectionCatalogDocument,
  EMPTY_CONNECTION_CATALOG_DOCUMENT,
  registerConnectionInCatalog,
  relabelConnectionInCatalog,
  removeConnectionFromCatalog,
  removeConnectionRegistrationFromCatalog,
  replaceSavedConnectionCredential,
} from "./storageDocument.ts";

const ENVIRONMENT_ID = EnvironmentId.make("environment-1");

const BEARER_TARGET = new BearerConnectionTarget({
  environmentId: ENVIRONMENT_ID,
  label: "Remote",
  connectionId: "bearer-1",
  serverEnvironmentId: null,
});
const BEARER_PROFILE = new BearerConnectionProfile({
  connectionId: BEARER_TARGET.connectionId,
  environmentId: ENVIRONMENT_ID,
  label: BEARER_TARGET.label,
  httpBaseUrl: "https://remote.example.test",
  wsBaseUrl: "wss://remote.example.test",
  hostKey: null,
});
const BEARER_CREDENTIAL = new BearerConnectionCredential({
  token: "bearer-token",
});
const REMOTE_TOKEN = new TokenStore.RemoteDpopAccessToken({
  environmentId: ENVIRONMENT_ID,
  label: "Remote",
  endpoint: {
    httpBaseUrl: "https://remote.example.test",
    wsBaseUrl: "wss://remote.example.test",
    providerKind: "cloudflare_tunnel",
  },
  accessToken: "dpop-token",
  expiresAtEpochMs: 1_000_000,
  dpopThumbprint: "thumbprint",
});
const decodeConnectionCatalogDocument = Schema.decodeUnknownSync(ConnectionCatalogDocument);
const decodeBearerConnectionProfile = Schema.decodeUnknownSync(BearerConnectionProfile);

describe("ConnectionCatalogDocument", () => {
  it("normalizes absent and blank legacy bearer host keys to null", () => {
    const rawProfile = {
      _tag: "BearerConnectionProfile",
      connectionId: "bearer:env-1",
      environmentId: "env-1",
      label: "Legacy",
      httpBaseUrl: "http://192.168.1.20:3773/",
      wsBaseUrl: "ws://192.168.1.20:3773/",
    };

    for (const hostKey of [undefined, null, "", " \t "] as const) {
      const decoded = decodeBearerConnectionProfile({
        ...rawProfile,
        ...(hostKey === undefined ? {} : { hostKey }),
      });
      expect(decoded.hostKey, String(hostKey)).toBeNull();
    }

    expect(decodeBearerConnectionProfile({ ...rawProfile, hostKey: "host-key" }).hostKey).toBe(
      "host-key",
    );
  });

  it("decodes a schema-v1 document without accepted storage identities", () => {
    const oldDocument = {
      schemaVersion: 1,
      targets: [BEARER_TARGET],
      profiles: [BEARER_PROFILE],
      credentials: [
        {
          connectionId: BEARER_TARGET.connectionId,
          credential: BEARER_CREDENTIAL,
        },
      ],
      remoteDpopTokens: [REMOTE_TOKEN],
    };

    expect(decodeConnectionCatalogDocument(oldDocument)).toEqual({
      ...oldDocument,
      acceptedStorageIdentities: [],
    });
    expect(EMPTY_CONNECTION_CATALOG_DOCUMENT.acceptedStorageIdentities).toEqual([]);
  });

  it("registers a bearer connection as one catalog mutation", () => {
    const document = registerConnectionInCatalog(
      {
        ...EMPTY_CONNECTION_CATALOG_DOCUMENT,
        acceptedStorageIdentities: [
          {
            targetKey: "bearer:bearer-1",
            storageInstanceId: "store-a",
          },
        ],
      },
      new BearerConnectionRegistration({
        target: BEARER_TARGET,
        profile: BEARER_PROFILE,
        credential: BEARER_CREDENTIAL,
      }),
    );

    expect(document.targets).toEqual([BEARER_TARGET]);
    expect(document.profiles).toEqual([BEARER_PROFILE]);
    expect(document.credentials).toEqual([
      {
        connectionId: BEARER_TARGET.connectionId,
        credential: BEARER_CREDENTIAL,
      },
    ]);
    expect(document.acceptedStorageIdentities).toEqual([
      {
        targetKey: "bearer:bearer-1",
        storageInstanceId: "store-a",
      },
    ]);
  });

  it("replaces obsolete connection metadata without discarding a reusable DPoP token", () => {
    const bearer = registerConnectionInCatalog(
      {
        ...EMPTY_CONNECTION_CATALOG_DOCUMENT,
        remoteDpopTokens: [REMOTE_TOKEN],
      },
      new BearerConnectionRegistration({
        target: BEARER_TARGET,
        profile: BEARER_PROFILE,
        credential: BEARER_CREDENTIAL,
      }),
    );
    const relayTarget = new RelayConnectionTarget({
      environmentId: ENVIRONMENT_ID,
      label: "Remote",
    });
    const relay = registerConnectionInCatalog(
      bearer,
      new RelayConnectionRegistration({ target: relayTarget }),
    );

    expect(relay.targets).toEqual([relayTarget]);
    expect(relay.profiles).toEqual([]);
    expect(relay.credentials).toEqual([]);
    expect(relay.remoteDpopTokens).toEqual([REMOTE_TOKEN]);
  });

  it("removes every catalog record owned by an explicit disconnect", () => {
    const registered = registerConnectionInCatalog(
      {
        ...EMPTY_CONNECTION_CATALOG_DOCUMENT,
        remoteDpopTokens: [REMOTE_TOKEN],
      },
      new BearerConnectionRegistration({
        target: BEARER_TARGET,
        profile: BEARER_PROFILE,
        credential: BEARER_CREDENTIAL,
      }),
    );

    expect(removeConnectionFromCatalog(registered, BEARER_TARGET)).toEqual(
      EMPTY_CONNECTION_CATALOG_DOCUMENT,
    );
  });

  it("does not conditionally remove a replacement registration", () => {
    const first = new BearerConnectionRegistration({
      target: BEARER_TARGET,
      profile: BEARER_PROFILE,
      credential: BEARER_CREDENTIAL,
    });
    const replacement = new BearerConnectionRegistration({
      target: new BearerConnectionTarget({
        ...BEARER_TARGET,
        label: "Replacement",
        serverEnvironmentId: null,
      }),
      profile: new BearerConnectionProfile({
        ...BEARER_PROFILE,
        label: "Replacement",
        httpBaseUrl: "https://replacement.example.test",
        wsBaseUrl: "wss://replacement.example.test",
      }),
      credential: new BearerConnectionCredential({ token: "replacement-token" }),
    });
    const document = registerConnectionInCatalog(
      registerConnectionInCatalog(EMPTY_CONNECTION_CATALOG_DOCUMENT, first),
      replacement,
    );

    expect(removeConnectionRegistrationFromCatalog(document, first)).toEqual({
      document,
      removed: false,
      current: { target: replacement.target, profile: Option.some(replacement.profile) },
    });
  });

  it("conditionally removes the exact registration", () => {
    const registration = new BearerConnectionRegistration({
      target: BEARER_TARGET,
      profile: BEARER_PROFILE,
      credential: BEARER_CREDENTIAL,
    });
    const document = registerConnectionInCatalog(
      {
        ...EMPTY_CONNECTION_CATALOG_DOCUMENT,
        remoteDpopTokens: [REMOTE_TOKEN],
      },
      registration,
    );

    expect(removeConnectionRegistrationFromCatalog(document, registration)).toEqual({
      document: {
        ...EMPTY_CONNECTION_CATALOG_DOCUMENT,
        remoteDpopTokens: [REMOTE_TOKEN],
      },
      removed: true,
      current: null,
    });
  });

  it("persists the normalized SSH profile beside its target", () => {
    const target = new SshConnectionTarget({
      environmentId: ENVIRONMENT_ID,
      label: "SSH",
      connectionId: "ssh-1",
    });
    const profile = new SshConnectionProfile({
      connectionId: target.connectionId,
      environmentId: target.environmentId,
      label: target.label,
      target: {
        alias: "devbox",
        hostname: "devbox.example.test",
        username: "developer",
        port: 22,
      },
    });
    const credential = new BearerConnectionCredential({ token: "ssh-bearer" });
    const document = registerConnectionInCatalog(
      EMPTY_CONNECTION_CATALOG_DOCUMENT,
      new SshConnectionRegistration({ target, profile, credential }),
    );

    expect(document.targets).toEqual([target]);
    expect(document.profiles).toEqual([profile]);
    expect(document.credentials).toEqual([{ connectionId: target.connectionId, credential }]);
  });

  it("replaces a saved SSH bearer only while its environment is saved", () => {
    const target = new SshConnectionTarget({
      environmentId: ENVIRONMENT_ID,
      label: "SSH",
      connectionId: "ssh-1",
    });
    const profile = new SshConnectionProfile({
      connectionId: target.connectionId,
      environmentId: target.environmentId,
      label: target.label,
      target: { alias: "devbox", hostname: "devbox", username: null, port: null },
    });
    const registered = registerConnectionInCatalog(
      EMPTY_CONNECTION_CATALOG_DOCUMENT,
      new SshConnectionRegistration({
        target,
        profile,
        credential: new BearerConnectionCredential({ token: "first" }),
      }),
    );
    const refreshed = new BearerConnectionCredential({ token: "second" });

    expect(replaceSavedConnectionCredential(registered, "ssh-1", refreshed)?.credentials).toEqual([
      { connectionId: "ssh-1", credential: refreshed },
    ]);

    const removed = removeConnectionFromCatalog(registered, target);
    expect(removed.credentials).toEqual([]);
    expect(replaceSavedConnectionCredential(removed, "ssh-1", refreshed)).toBeNull();
  });

  it("renames one saved target in place and leaves every other record alone", () => {
    const relay = new RelayConnectionTarget({
      environmentId: EnvironmentId.make("environment-relay"),
      label: "Relay",
    });
    const ssh = new SshConnectionTarget({
      environmentId: EnvironmentId.make("environment-ssh"),
      label: "SSH",
      connectionId: "ssh-1",
    });
    const pairedBearer = new BearerConnectionTarget({
      ...BEARER_TARGET,
      serverEnvironmentId: EnvironmentId.make("local"),
    });
    const document: ConnectionCatalogDocument = {
      ...EMPTY_CONNECTION_CATALOG_DOCUMENT,
      targets: [pairedBearer, relay, ssh],
      profiles: [BEARER_PROFILE],
      credentials: [{ connectionId: BEARER_TARGET.connectionId, credential: BEARER_CREDENTIAL }],
      remoteDpopTokens: [REMOTE_TOKEN],
      acceptedStorageIdentities: [{ targetKey: "bearer:bearer-1", storageInstanceId: "store-1" }],
    };

    const renamed = relabelConnectionInCatalog(document, ENVIRONMENT_ID, "GPU box");

    expect(renamed?.target).toEqual(
      new BearerConnectionTarget({
        environmentId: ENVIRONMENT_ID,
        label: "GPU box",
        connectionId: BEARER_TARGET.connectionId,
        serverEnvironmentId: EnvironmentId.make("local"),
      }),
    );
    expect(renamed?.document).toEqual({
      ...document,
      targets: [renamed?.target, relay, ssh],
    });
    // The profile keeps the name it was saved with; only the target label is
    // the saved name every surface reads.
    expect(renamed?.document.profiles).toEqual([BEARER_PROFILE]);

    const renamedRelay = relabelConnectionInCatalog(document, relay.environmentId, "Cloud box");
    expect(renamedRelay?.target).toEqual(
      new RelayConnectionTarget({ environmentId: relay.environmentId, label: "Cloud box" }),
    );
    const renamedSsh = relabelConnectionInCatalog(document, ssh.environmentId, "Build host");
    expect(renamedSsh?.target).toEqual(
      new SshConnectionTarget({
        environmentId: ssh.environmentId,
        label: "Build host",
        connectionId: "ssh-1",
      }),
    );
  });

  it("does not rename an environment that is not saved", () => {
    expect(
      relabelConnectionInCatalog(EMPTY_CONNECTION_CATALOG_DOCUMENT, ENVIRONMENT_ID, "GPU box"),
    ).toBeNull();
  });
});
