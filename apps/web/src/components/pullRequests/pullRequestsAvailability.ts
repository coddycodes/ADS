import type { SupervisorConnectionState } from "@bibcode/client-runtime/connection";
import type { ServerConfig, SourceControlProviderKind } from "@bibcode/contracts";

export type PullRequestsAvailability =
  | { readonly kind: "ready" }
  | { readonly kind: "disabled_in_settings" }
  | { readonly kind: "pending"; readonly reason: string }
  | { readonly kind: "disconnected"; readonly reason: string }
  | { readonly kind: "unsupported"; readonly missingCapability: "pullRequestsReads" };

function disconnectedReason(connectionState: SupervisorConnectionState): string {
  switch (connectionState.phase) {
    case "available":
      return "This environment is disconnected.";
    case "offline":
      return "This environment is offline.";
    case "backoff":
      return connectionState.lastFailure?.message ?? "This environment is reconnecting.";
    case "blocked":
      return connectionState.lastFailure?.message ?? "This environment connection is blocked.";
    case "connecting":
    case "connected":
      return "This environment is unavailable.";
  }
}

export function resolvePullRequestsAvailability(
  connectionState: SupervisorConnectionState | null,
  serverConfig: ServerConfig | null,
  pullRequestsEnabled: boolean,
): PullRequestsAvailability {
  if (!pullRequestsEnabled) return { kind: "disabled_in_settings" };
  if (connectionState === null) {
    return { kind: "pending", reason: "Waiting for the environment connection state." };
  }
  if (!connectionState.desired || connectionState.phase === "available") {
    return { kind: "disconnected", reason: "This environment is disconnected." };
  }
  if (connectionState.phase === "connecting") {
    return {
      kind: "pending",
      reason:
        connectionState.stage === "synchronizing"
          ? "This environment is synchronizing."
          : "This environment is connecting.",
    };
  }
  if (connectionState.phase !== "connected") {
    return { kind: "disconnected", reason: disconnectedReason(connectionState) };
  }
  if (serverConfig === null) {
    return { kind: "pending", reason: "Waiting for Pull Requests capabilities." };
  }
  if (serverConfig.environment.capabilities.pullRequestsReads !== true) {
    return { kind: "unsupported", missingCapability: "pullRequestsReads" };
  }
  return { kind: "ready" };
}

export const PULL_REQUESTS_DISABLED_IN_SETTINGS_MESSAGE =
  "Pull requests are turned off in Settings → Source Control.";

export function resolvePullRequestsMutationsDisabledReason(
  serverConfig: ServerConfig | null,
): string | null {
  return serverConfig?.environment.capabilities.pullRequestsMutations === true
    ? null
    : "This environment does not support Pull Requests actions.";
}

/**
 * Whether a request on this host can be shown in a thread's right panel. Transient states (a
 * disconnected environment) still open the tab, which explains them; a setting that is off or a
 * host without request details would only leave a dead-end tab.
 */
export function canOpenPullRequestInPanel(input: {
  readonly enabled: boolean;
  readonly providerKind: SourceControlProviderKind;
}): boolean {
  return (
    input.enabled && input.providerKind !== "bitbucket" && input.providerKind !== "azure-devops"
  );
}
