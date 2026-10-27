import { EnvironmentId, type ExecutionEnvironmentDescriptor } from "@coddy/contracts";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";

import type { E2eeAuthRequest } from "../e2ee/socket.ts";

const SavedServerEnvironmentId = Schema.NullOr(EnvironmentId).pipe(
  Schema.withDecodingDefault(Effect.succeed(null)),
);

const ConnectionTargetBase = {
  environmentId: EnvironmentId,
  label: Schema.String,
};

export class PrimaryConnectionTarget extends Schema.TaggedClass<PrimaryConnectionTarget>()(
  "PrimaryConnectionTarget",
  {
    ...ConnectionTargetBase,
    httpBaseUrl: Schema.String,
    wsBaseUrl: Schema.String,
  },
) {}

export class BearerConnectionTarget extends Schema.TaggedClass<BearerConnectionTarget>()(
  "BearerConnectionTarget",
  {
    ...ConnectionTargetBase,
    connectionId: Schema.String,
    // What the host declares about itself, kept apart from `environmentId`,
    // which is the client's own key for this environment. Null on entries
    // saved before the two were separated: those carry the host-declared id
    // in `environmentId`, so readers fall back to it.
    serverEnvironmentId: SavedServerEnvironmentId,
  },
) {}

export class RelayConnectionTarget extends Schema.TaggedClass<RelayConnectionTarget>()(
  "RelayConnectionTarget",
  {
    ...ConnectionTargetBase,
  },
) {}

export class SshConnectionTarget extends Schema.TaggedClass<SshConnectionTarget>()(
  "SshConnectionTarget",
  {
    ...ConnectionTargetBase,
    connectionId: Schema.String,
  },
) {}

/**
 * A platform-owned environment that remains desired but currently has no
 * usable endpoint. It is deliberately not persistable: the host topology is
 * the sole owner and the resolver must fail before any transport/session work.
 */
export class UnavailableConnectionTarget extends Schema.TaggedClass<UnavailableConnectionTarget>()(
  "UnavailableConnectionTarget",
  {
    ...ConnectionTargetBase,
    connectionId: Schema.String,
    configuredDistro: Schema.NullOr(Schema.String),
    detail: Schema.String,
  },
) {}

export const ConnectionTarget = Schema.Union([
  PrimaryConnectionTarget,
  BearerConnectionTarget,
  RelayConnectionTarget,
  SshConnectionTarget,
  UnavailableConnectionTarget,
]);
export type ConnectionTarget = typeof ConnectionTarget.Type;

export const PersistedConnectionTarget = Schema.Union([
  BearerConnectionTarget,
  RelayConnectionTarget,
  SshConnectionTarget,
]);
export type PersistedConnectionTarget = typeof PersistedConnectionTarget.Type;

export const isPersistedConnectionTarget = Schema.is(PersistedConnectionTarget);

/** Returns a real target instance with only its client-local label replaced. */
export function withPersistedTargetLabel(
  target: PersistedConnectionTarget,
  label: string,
): PersistedConnectionTarget {
  switch (target._tag) {
    case "BearerConnectionTarget":
      return new BearerConnectionTarget({ ...target, label });
    case "RelayConnectionTarget":
      return new RelayConnectionTarget({ ...target, label });
    case "SshConnectionTarget":
      return new SshConnectionTarget({ ...target, label });
  }
}

export type ConnectionTargetKind = ConnectionTarget["_tag"];

export type NetworkStatus = "unknown" | "offline" | "online";

export const ConnectionTransientReason = Schema.Literals([
  "network",
  "timeout",
  "transport",
  "endpoint-unavailable",
  "relay-unavailable",
  "remote-unavailable",
  "liveness-timeout",
  "connection-closed",
  "connection-lost",
]);
export type ConnectionTransientReason = typeof ConnectionTransientReason.Type;

export const ConnectionBlockedReason = Schema.Literals([
  "authentication",
  "configuration",
  "host-identity",
  "permission",
  "recovery-required",
  "storage-changed",
  "unsupported",
]);
export type ConnectionBlockedReason = typeof ConnectionBlockedReason.Type;

export class ConnectionTransientError extends Schema.TaggedError<ConnectionTransientError>()(
  "ConnectionTransientError",
  {
    reason: ConnectionTransientReason,
    detail: Schema.String,
    traceId: Schema.optionalKey(Schema.String),
  },
) {
  override get message(): string {
    return this.detail;
  }
}

export class ConnectionBlockedError extends Schema.TaggedError<ConnectionBlockedError>()(
  "ConnectionBlockedError",
  {
    reason: ConnectionBlockedReason,
    detail: Schema.String,
    traceId: Schema.optionalKey(Schema.String),
  },
) {
  override get message(): string {
    return this.detail;
  }
}

export class ConnectionStorageChangedError extends Schema.TaggedError<ConnectionStorageChangedError>()(
  "ConnectionStorageChangedError",
  {
    reason: Schema.Literal("storage-changed"),
    detail: Schema.String,
    targetKey: Schema.String,
    acceptedStorageInstanceId: Schema.String,
    reportedStorageInstanceId: Schema.String,
  },
) {
  override get message(): string {
    return this.detail;
  }
}

export type ConnectionAttemptError =
  | ConnectionTransientError
  | ConnectionBlockedError
  | ConnectionStorageChangedError;

export type PreparedHttpAuthorization =
  | {
      readonly _tag: "Bearer";
      readonly token: string;
    }
  | {
      readonly _tag: "Dpop";
      readonly accessToken: string;
    };

export interface PreparedE2eeChannel {
  readonly hostKey: string;
  readonly auth: E2eeAuthRequest;
}

export interface PreparedConnection {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly descriptor: ExecutionEnvironmentDescriptor;
  readonly httpBaseUrl: string;
  readonly socketUrl: string;
  readonly httpAuthorization: PreparedHttpAuthorization | null;
  readonly e2ee: PreparedE2eeChannel | null;
  readonly target: ConnectionTarget;
}

export type SupervisorConnectionPhase =
  | "available"
  | "offline"
  | "connecting"
  | "backoff"
  | "connected"
  | "blocked";

/**
 * `configuring` means the socket is connected (and authenticated for E2EE),
 * awaiting the first config snapshot and identity check.
 */
export type ConnectionAttemptStage = "preparing" | "opening" | "configuring" | "synchronizing";

export interface SupervisorConnectionState {
  readonly desired: boolean;
  readonly network: NetworkStatus;
  readonly phase: SupervisorConnectionPhase;
  readonly stage: ConnectionAttemptStage | null;
  readonly attempt: number;
  readonly generation: number;
  readonly lastFailure: ConnectionAttemptError | null;
  readonly retryAt: number | null;
  /** Set by the supervisor while configuring is slow; absent otherwise. */
  readonly notice?: string;
}

export type ConnectionProjectionPhase = "disconnected" | "synchronizing" | "ready";

export function connectionProjectionPhase(
  state: SupervisorConnectionState,
): ConnectionProjectionPhase {
  switch (state.phase) {
    case "connecting":
      return "synchronizing";
    case "connected":
      return "ready";
    case "available":
    case "offline":
    case "backoff":
    case "blocked":
      return "disconnected";
  }
}

export const AVAILABLE_CONNECTION_STATE: SupervisorConnectionState = Object.freeze({
  desired: false,
  network: "unknown",
  phase: "available",
  stage: null,
  attempt: 0,
  generation: 0,
  lastFailure: null,
  retryAt: null,
});
