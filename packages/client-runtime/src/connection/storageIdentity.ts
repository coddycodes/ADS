import * as Effect from "effect/Effect";
import type { ExecutionEnvironmentDescriptor } from "@coddy/contracts";

import * as Persistence from "../platform/persistence.ts";
import {
  ConnectionBlockedError,
  ConnectionStorageChangedError,
  type ConnectionTarget,
  type PreparedConnection,
} from "./model.ts";

export type StorageIdentityDecision =
  | {
      readonly _tag: "Bootstrap";
      readonly reported: string;
    }
  | {
      readonly _tag: "Accepted";
      readonly value: string;
    }
  | {
      readonly _tag: "Changed";
      readonly accepted: string;
      readonly reported: string;
    }
  | {
      readonly _tag: "Unverifiable";
      readonly accepted: string | null;
    };

export function storageIdentityTargetKey(target: ConnectionTarget): string {
  switch (target._tag) {
    case "PrimaryConnectionTarget":
      return "platform:primary";
    case "BearerConnectionTarget":
    case "UnavailableConnectionTarget":
      return `bearer:${target.connectionId}`;
    case "RelayConnectionTarget":
      return `relay:${target.environmentId}`;
    case "SshConnectionTarget":
      return `ssh:${target.connectionId}`;
  }
}

export function decideStorageIdentity(
  accepted: string | null,
  reported: string | null,
): StorageIdentityDecision {
  if (reported === null) {
    return { _tag: "Unverifiable", accepted };
  }
  if (accepted === null) {
    return { _tag: "Bootstrap", reported };
  }
  if (accepted === reported) {
    return { _tag: "Accepted", value: accepted };
  }
  return { _tag: "Changed", accepted, reported };
}

function identityPersistenceError(prepared: PreparedConnection): ConnectionBlockedError {
  return new ConnectionBlockedError({
    reason: "configuration",
    detail: `${prepared.label} persistent storage identity could not be verified.`,
  });
}

export const verifyPreparedStorageIdentity = Effect.fn("verifyPreparedStorageIdentity")(function* (
  prepared: PreparedConnection,
  descriptor: ExecutionEnvironmentDescriptor = prepared.descriptor,
) {
  const identities = yield* Persistence.AcceptedStorageIdentityStore;
  const targetKey = storageIdentityTargetKey(prepared.target);
  const reported = descriptor.storageInstanceId;
  const decision = yield* identities
    .transition(targetKey, (accepted) => {
      const decision = decideStorageIdentity(accepted, reported);
      return {
        result: decision,
        mutation:
          decision._tag === "Bootstrap"
            ? { _tag: "Set" as const, storageInstanceId: decision.reported }
            : { _tag: "Keep" as const },
      };
    })
    .pipe(Effect.mapError(() => identityPersistenceError(prepared)));

  switch (decision._tag) {
    case "Bootstrap":
    case "Accepted":
    case "Unverifiable":
      return;
    case "Changed":
      return yield* new ConnectionStorageChangedError({
        reason: "storage-changed",
        detail: `${prepared.label} is reporting a different persistent store.`,
        targetKey,
        acceptedStorageInstanceId: decision.accepted,
        reportedStorageInstanceId: decision.reported,
      });
  }
});
