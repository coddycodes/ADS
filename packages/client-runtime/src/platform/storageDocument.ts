import type { EnvironmentId } from "@coddy/contracts";
import * as Effect from "effect/Effect";
import * as Equal from "effect/Equal";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

import {
  type ConnectionCatalogEntry,
  type ConnectionRegistration,
  ConnectionCredential,
  ConnectionProfile,
} from "../connection/catalog.ts";
import {
  type ConnectionTarget,
  PersistedConnectionTarget,
  withPersistedTargetLabel,
} from "../connection/model.ts";
import * as TokenStore from "../authorization/tokenStore.ts";
import {
  AcceptedStorageIdentitySchema,
  type ConnectionRegistrationRemovalResult,
} from "./persistence.ts";

export const StoredConnectionCredential = Schema.Struct({
  connectionId: Schema.String,
  credential: ConnectionCredential,
});
export type StoredConnectionCredential = typeof StoredConnectionCredential.Type;

export const ConnectionCatalogDocument = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  targets: Schema.Array(PersistedConnectionTarget),
  profiles: Schema.Array(ConnectionProfile),
  credentials: Schema.Array(StoredConnectionCredential),
  remoteDpopTokens: Schema.Array(TokenStore.RemoteDpopAccessToken),
  acceptedStorageIdentities: Schema.Array(AcceptedStorageIdentitySchema).pipe(
    Schema.withDecodingDefault(Effect.succeed([])),
  ),
});
export type ConnectionCatalogDocument = typeof ConnectionCatalogDocument.Type;

export const EMPTY_CONNECTION_CATALOG_DOCUMENT: ConnectionCatalogDocument = Object.freeze({
  schemaVersion: 1,
  targets: [],
  profiles: [],
  credentials: [],
  remoteDpopTokens: [],
  acceptedStorageIdentities: [],
});

export function replaceCatalogValue<A>(
  values: ReadonlyArray<A>,
  key: (value: A) => string,
  next: A,
): ReadonlyArray<A> {
  const nextKey = key(next);
  return [...values.filter((value) => key(value) !== nextKey), next];
}

export function removeCatalogValue<A>(
  values: ReadonlyArray<A>,
  key: (value: A) => string,
  removedKey: string,
): ReadonlyArray<A> {
  return values.filter((value) => key(value) !== removedKey);
}

function connectionIdOf(target: ConnectionTarget): string | null {
  switch (target._tag) {
    case "PrimaryConnectionTarget":
    case "RelayConnectionTarget":
      return null;
    case "BearerConnectionTarget":
    case "SshConnectionTarget":
    case "UnavailableConnectionTarget":
      return target.connectionId;
  }
}

function removeConnectionMetadata(
  document: ConnectionCatalogDocument,
  target: ConnectionTarget,
  removeRemoteToken: boolean,
): ConnectionCatalogDocument {
  const connectionId = connectionIdOf(target);
  return {
    ...document,
    targets: removeCatalogValue(
      document.targets,
      (value) => value.environmentId,
      target.environmentId,
    ),
    profiles:
      connectionId === null
        ? document.profiles
        : removeCatalogValue(document.profiles, (value) => value.connectionId, connectionId),
    credentials:
      connectionId === null
        ? document.credentials
        : removeCatalogValue(document.credentials, (value) => value.connectionId, connectionId),
    remoteDpopTokens: removeRemoteToken
      ? removeCatalogValue(
          document.remoteDpopTokens,
          (value) => value.environmentId,
          target.environmentId,
        )
      : document.remoteDpopTokens,
  };
}

export function registerConnectionInCatalog(
  document: ConnectionCatalogDocument,
  registration: ConnectionRegistration,
): ConnectionCatalogDocument {
  const target = registration.target;
  const previous = document.targets.find(
    (candidate) => candidate.environmentId === target.environmentId,
  );
  const cleaned =
    previous === undefined ? document : removeConnectionMetadata(document, previous, false);
  const next: ConnectionCatalogDocument = {
    ...cleaned,
    targets: replaceCatalogValue(cleaned.targets, (value) => value.environmentId, target),
  };

  switch (registration._tag) {
    case "RelayConnectionRegistration":
      return next;
    case "BearerConnectionRegistration":
      return {
        ...next,
        profiles: replaceCatalogValue(
          next.profiles,
          (value) => value.connectionId,
          registration.profile,
        ),
        credentials: replaceCatalogValue(next.credentials, (value) => value.connectionId, {
          connectionId: registration.target.connectionId,
          credential: registration.credential,
        }),
      };
    case "SshConnectionRegistration":
      return {
        ...next,
        profiles: replaceCatalogValue(
          next.profiles,
          (value) => value.connectionId,
          registration.profile,
        ),
        credentials: replaceCatalogValue(next.credentials, (value) => value.connectionId, {
          connectionId: registration.target.connectionId,
          credential: registration.credential,
        }),
      };
  }
}

/**
 * Replaces the credential of a saved environment. Returns null, and writes
 * nothing, when no saved target owns `connectionId` any more.
 */
export function replaceSavedConnectionCredential(
  document: ConnectionCatalogDocument,
  connectionId: string,
  credential: ConnectionCredential,
): ConnectionCatalogDocument | null {
  if (!document.targets.some((target) => connectionIdOf(target) === connectionId)) {
    return null;
  }
  return {
    ...document,
    credentials: replaceCatalogValue(document.credentials, (value) => value.connectionId, {
      connectionId,
      credential,
    }),
  };
}

export function removeConnectionFromCatalog(
  document: ConnectionCatalogDocument,
  target: ConnectionTarget,
): ConnectionCatalogDocument {
  return removeConnectionMetadata(document, target, true);
}

export interface ConnectionTargetRelabel {
  readonly document: ConnectionCatalogDocument;
  readonly target: PersistedConnectionTarget;
}

/**
 * Renames one saved environment: replaces only its target's display label, in
 * place. Profiles, credentials, DPoP tokens, accepted identities, and target
 * order are untouched. Returns null when the environment is not saved.
 */
export function relabelConnectionInCatalog(
  document: ConnectionCatalogDocument,
  environmentId: EnvironmentId,
  label: string,
): ConnectionTargetRelabel | null {
  const current = document.targets.find((target) => target.environmentId === environmentId);
  if (current === undefined) {
    return null;
  }
  const target = withPersistedTargetLabel(current, label);
  return {
    document: {
      ...document,
      targets: document.targets.map((candidate) =>
        candidate.environmentId === environmentId ? target : candidate,
      ),
    },
    target,
  };
}

export interface ConditionalConnectionRegistrationRemoval extends ConnectionRegistrationRemovalResult {
  readonly document: ConnectionCatalogDocument;
}

function connectionCatalogEntryFromDocument(
  document: ConnectionCatalogDocument,
  target: ConnectionCatalogDocument["targets"][number],
): ConnectionCatalogEntry {
  const profile =
    target._tag === "BearerConnectionTarget" || target._tag === "SshConnectionTarget"
      ? Option.fromUndefinedOr(
          document.profiles.find((candidate) => candidate.connectionId === target.connectionId),
        )
      : Option.none();
  return { target, profile };
}

export function removeConnectionRegistrationFromCatalog(
  document: ConnectionCatalogDocument,
  registration: ConnectionRegistration,
): ConditionalConnectionRegistrationRemoval {
  const target = document.targets.find(
    (candidate) => candidate.environmentId === registration.target.environmentId,
  );
  if (target === undefined) {
    return { document, removed: false, current: null };
  }
  const current = connectionCatalogEntryFromDocument(document, target);
  if (!Equal.equals(target, registration.target)) {
    return { document, removed: false, current };
  }

  if (registration._tag === "BearerConnectionRegistration") {
    const profile = document.profiles.find(
      (candidate) => candidate.connectionId === registration.target.connectionId,
    );
    const credential = document.credentials.find(
      (candidate) => candidate.connectionId === registration.target.connectionId,
    );
    if (
      profile === undefined ||
      !Equal.equals(profile, registration.profile) ||
      credential === undefined ||
      !Equal.equals(credential.credential, registration.credential)
    ) {
      return { document, removed: false, current };
    }
  } else if (registration._tag === "SshConnectionRegistration") {
    // The SSH credential is not compared: reconnects replace it when the host
    // rejects it, and the registration is still the same one.
    const profile = document.profiles.find(
      (candidate) => candidate.connectionId === registration.target.connectionId,
    );
    if (profile === undefined || !Equal.equals(profile, registration.profile)) {
      return { document, removed: false, current };
    }
  }

  return {
    document: removeConnectionMetadata(document, registration.target, false),
    removed: true,
    current: null,
  };
}
