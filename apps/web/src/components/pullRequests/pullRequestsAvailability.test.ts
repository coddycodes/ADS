import {
  AVAILABLE_CONNECTION_STATE,
  type SupervisorConnectionState,
} from "@coddy/client-runtime/connection";
import type { ServerConfig } from "@coddy/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import { describe, expect, it } from "vite-plus/test";
import {
  canOpenPullRequestInPanel,
  resolvePullRequestsAvailability,
  resolvePullRequestsMutationsDisabledReason,
} from "./pullRequestsAvailability";
const connected: SupervisorConnectionState = {
  ...AVAILABLE_CONNECTION_STATE,
  desired: true,
  phase: "connected",
  network: "online",
};
const config = (reads: boolean, mutations = false) =>
  ({
    environment: {
      capabilities: makeTestExecutionEnvironmentCapabilities({
        pullRequestsReads: reads,
        pullRequestsMutations: mutations,
      }),
    },
  }) as ServerConfig;
describe("resolvePullRequestsAvailability", () => {
  it("settings-off wins over connection and capabilities", () => {
    expect(resolvePullRequestsAvailability(null, null, false)).toEqual({
      kind: "disabled_in_settings",
    });
    expect(resolvePullRequestsAvailability(connected, config(true), false)).toEqual({
      kind: "disabled_in_settings",
    });
  });
  it("waits for connection and configuration", () => {
    expect(resolvePullRequestsAvailability(null, config(true), true).kind).toBe("pending");
    expect(resolvePullRequestsAvailability(connected, null, true).kind).toBe("pending");
  });
  it("keeps explicitly disconnected environments disconnected", () => {
    expect(resolvePullRequestsAvailability(AVAILABLE_CONNECTION_STATE, config(true), true)).toEqual(
      { kind: "disconnected", reason: "This environment is disconnected." },
    );
    expect(
      resolvePullRequestsAvailability({ ...connected, desired: false }, config(true), true).kind,
    ).toBe("disconnected");
  });
  it.each(["offline", "backoff", "blocked"] as const)("does not treat %s as ready", (phase) => {
    expect(resolvePullRequestsAvailability({ ...connected, phase }, config(true), true).kind).toBe(
      "disconnected",
    );
  });
  it("waits while synchronizing", () => {
    expect(
      resolvePullRequestsAvailability(
        { ...connected, phase: "connecting", stage: "synchronizing" },
        config(true),
        true,
      ),
    ).toEqual({ kind: "pending", reason: "This environment is synchronizing." });
  });
  it("names the unsupported capability and permits a supported connected environment", () => {
    expect(resolvePullRequestsAvailability(connected, config(false), true)).toEqual({
      kind: "unsupported",
      missingCapability: "pullRequestsReads",
    });
    expect(resolvePullRequestsAvailability(connected, config(true), true)).toEqual({
      kind: "ready",
    });
  });
  it("fails mutations closed independently from read capability", () => {
    expect(resolvePullRequestsMutationsDisabledReason(config(true))).toBe(
      "This environment does not support Pull Requests actions.",
    );
    expect(resolvePullRequestsMutationsDisabledReason(null)).not.toBeNull();
    expect(resolvePullRequestsMutationsDisabledReason(config(true, true))).toBeNull();
  });
});

describe("canOpenPullRequestInPanel", () => {
  it.each(["github", "gitlab", "unknown"] as const)("opens a %s request when enabled", (kind) => {
    expect(canOpenPullRequestInPanel({ enabled: true, providerKind: kind })).toBe(true);
  });
  it.each(["bitbucket", "azure-devops"] as const)("never opens a %s request", (kind) => {
    expect(canOpenPullRequestInPanel({ enabled: true, providerKind: kind })).toBe(false);
  });
  it.each(["github", "gitlab", "unknown", "bitbucket", "azure-devops"] as const)(
    "never opens a %s request while Pull Requests is turned off",
    (kind) => {
      expect(canOpenPullRequestInPanel({ enabled: false, providerKind: kind })).toBe(false);
    },
  );
});
