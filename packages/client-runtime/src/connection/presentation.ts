import type { ServerConfig } from "@coddy/contracts";
import * as Option from "effect/Option";

import type { ConnectionCatalogEntry } from "./catalog.ts";
import type { NetworkStatus, SupervisorConnectionState } from "./model.ts";

export const SERVER_NAME_REQUIRED_MESSAGE = "Enter a name for this server.";
export const PLATFORM_ENVIRONMENT_NAME_MESSAGE = "This device names its own environments.";

function failureTraceId(state: SupervisorConnectionState): string | null {
  return state.lastFailure !== null && "traceId" in state.lastFailure
    ? (state.lastFailure.traceId ?? null)
    : null;
}

export type EnvironmentConnectionPhase =
  | "available"
  | "offline"
  | "connecting"
  | "reconnecting"
  | "connected"
  | "error";

export interface EnvironmentConnectionPresentation {
  readonly phase: EnvironmentConnectionPhase;
  readonly error: string | null;
  readonly traceId: string | null;
  /** Progress copy for the current attempt; status text uses it instead of phase and error. */
  readonly notice?: string;
}

export interface EnvironmentPresentation {
  readonly entry: ConnectionCatalogEntry;
  readonly connection: EnvironmentConnectionPresentation;
  readonly serverConfig: ServerConfig | null;
}

export type ConnectionTransportSecurity = "local" | "e2ee" | "channel-secured" | "unencrypted";

export interface ConnectionTransportSecurityInput {
  readonly target:
    | { readonly _tag: "PrimaryConnectionTarget" }
    | { readonly _tag: "UnavailableConnectionTarget" }
    | { readonly _tag: "RelayConnectionTarget" }
    | { readonly _tag: "SshConnectionTarget" }
    | { readonly _tag: "BearerConnectionTarget"; readonly connectionId?: string };
  readonly profile:
    | { readonly _tag: "None" }
    | {
        readonly _tag: "Some";
        readonly value: { readonly _tag: string; readonly hostKey?: string | null };
      };
}

export const DESKTOP_LOCAL_CONNECTION_ID_PREFIX = "local:";

export function isDesktopLocalConnectionId(connectionId: string | undefined): boolean {
  return connectionId?.startsWith(DESKTOP_LOCAL_CONNECTION_ID_PREFIX) ?? false;
}

export function connectionTransportSecurity(
  entry: ConnectionTransportSecurityInput,
): ConnectionTransportSecurity {
  switch (entry.target._tag) {
    case "PrimaryConnectionTarget":
    case "UnavailableConnectionTarget":
      return "local";
    case "RelayConnectionTarget":
    case "SshConnectionTarget":
      return "channel-secured";
    case "BearerConnectionTarget": {
      if (isDesktopLocalConnectionId(entry.target.connectionId)) {
        return "local";
      }
      const profile =
        entry.profile._tag === "Some" && entry.profile.value._tag === "BearerConnectionProfile"
          ? entry.profile.value
          : null;
      return profile?.hostKey == null ? "unencrypted" : "e2ee";
    }
  }
}

export function presentConnectionState(
  state: SupervisorConnectionState,
): EnvironmentConnectionPresentation {
  switch (state.phase) {
    case "available":
      return { phase: "available", error: null, traceId: null };
    case "offline":
      return { phase: "offline", error: null, traceId: null };
    case "connecting": {
      const phase =
        state.attempt <= 1 && state.lastFailure === null ? "connecting" : "reconnecting";
      if (state.notice !== undefined) {
        return {
          phase,
          error: null,
          traceId: null,
          notice: state.notice,
        };
      }
      return {
        phase,
        error: state.lastFailure?.message ?? null,
        traceId: failureTraceId(state),
      };
    }
    case "connected":
      return { phase: "connected", error: null, traceId: null };
    case "backoff":
      return {
        phase: "reconnecting",
        error: state.lastFailure?.message ?? null,
        traceId: failureTraceId(state),
      };
    case "blocked":
      return {
        phase: "error",
        error: state.lastFailure?.message ?? null,
        traceId: failureTraceId(state),
      };
  }
}

/**
 * Whether an environment's connection is unusable: every phase but
 * "connected". The chat banner and the sidebar notice share this rule.
 */
export function isConnectionUnavailable(connection: EnvironmentConnectionPresentation): boolean {
  return connection.phase !== "connected";
}

/** Ends `text` with sentence punctuation so a follow-on sentence reads cleanly. */
function asSentence(text: string): string {
  const trimmed = text.trim();
  return /[.!?…]$/.test(trimmed) ? trimmed : `${trimmed}.`;
}

export function connectionStatusText(connection: EnvironmentConnectionPresentation): string {
  switch (connection.phase) {
    case "available":
      return "Available";
    case "offline":
      return "Offline";
    case "connecting":
      return connection.notice ?? "Connecting…";
    case "reconnecting":
      return (
        connection.notice ??
        (connection.error ? `${asSentence(connection.error)} Reconnecting…` : "Reconnecting…")
      );
    case "connected":
      return "Connected";
    case "error":
      return connection.error
        ? `Connection failed. Reason: ${connection.error}`
        : "Connection failed";
  }
}

export function presentEnvironmentConnection(
  state: SupervisorConnectionState,
): EnvironmentConnectionPresentation {
  return presentConnectionState(state);
}

export function connectionCatalogDisplayUrl(entry: ConnectionCatalogEntry): string | null {
  switch (entry.target._tag) {
    case "PrimaryConnectionTarget":
      return entry.target.httpBaseUrl;
    case "RelayConnectionTarget":
    case "UnavailableConnectionTarget":
      return null;
    case "BearerConnectionTarget":
      return Option.isSome(entry.profile) && entry.profile.value._tag === "BearerConnectionProfile"
        ? entry.profile.value.httpBaseUrl
        : null;
    case "SshConnectionTarget":
      return Option.isSome(entry.profile) && entry.profile.value._tag === "SshConnectionProfile"
        ? `${entry.profile.value.target.username}@${entry.profile.value.target.hostname}`
        : null;
  }
}

export function connectionPhaseMessage(
  phase: EnvironmentConnectionPhase,
  label: string,
  networkStatus: NetworkStatus,
): string {
  if (networkStatus === "offline" || phase === "offline") {
    return "You are offline";
  }
  switch (phase) {
    case "available":
      return "Available";
    case "connecting":
      return `Connecting to ${label}...`;
    case "reconnecting":
      return `Reconnecting to ${label}...`;
    case "connected":
      return "Connected";
    case "error":
      return "Connection failed";
  }
}
