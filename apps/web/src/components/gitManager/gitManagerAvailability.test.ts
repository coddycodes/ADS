import {
  AVAILABLE_CONNECTION_STATE,
  ConnectionTransientError,
  type SupervisorConnectionState,
} from "@coddy/client-runtime/connection";
import type { ServerConfig } from "@coddy/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import { describe, expect, it } from "vite-plus/test";

import {
  GIT_MANAGER_BRANCH_SYNC_DISABLED_REASON,
  GIT_MANAGER_LIVE_SIGNAL_DISABLED_REASON,
  GIT_MANAGER_MERGE_INTO_DISABLED_REASON,
  GIT_MANAGER_PULL_REQUESTS_DISABLED_REASON,
  GIT_MANAGER_REWRITE_DISABLED_REASON,
  GIT_MANAGER_STASH_MERGE_DISABLED_REASON,
  GIT_MANAGER_TAG_DISABLED_REASON,
  resolveGitManagerAvailability,
  resolveGitManagerCapabilityDisabledReasons,
} from "./gitManagerAvailability";

function connection(overrides: Partial<SupervisorConnectionState>): SupervisorConnectionState {
  return { ...AVAILABLE_CONNECTION_STATE, ...overrides };
}

function serverConfig(gitManagerReads: boolean): ServerConfig {
  return {
    environment: {
      capabilities: makeTestExecutionEnvironmentCapabilities({ gitManagerReads }),
    },
  } as ServerConfig;
}

describe("resolveGitManagerAvailability", () => {
  it("keeps a connected environment pending until its config arrives", () => {
    expect(
      resolveGitManagerAvailability(
        connection({ desired: true, phase: "connected", network: "online" }),
        null,
        "Local",
      ),
    ).toMatchObject({ kind: "pending" });
  });

  it("reports an explicitly disconnected environment without requesting a connection", () => {
    expect(
      resolveGitManagerAvailability(
        connection({ desired: false, phase: "available", network: "online" }),
        serverConfig(true),
        "Local",
      ),
    ).toEqual({
      kind: "disconnected",
      reason: "This environment is disconnected.",
    });
  });

  it("names reconnecting and missing-capability states", () => {
    expect(
      resolveGitManagerAvailability(
        connection({ desired: true, phase: "backoff", network: "online" }),
        serverConfig(true),
        "Local",
      ),
    ).toEqual({
      kind: "disconnected",
      reason: "Reconnecting to Local. Git Manager loads when the connection is back.",
    });
    expect(
      resolveGitManagerAvailability(
        connection({ desired: true, phase: "connected", network: "online" }),
        serverConfig(false),
        "Local",
      ),
    ).toEqual({ kind: "unsupported", missingCapability: "gitManagerReads" });
  });

  it("keeps the reconnecting copy while a reconnect attempt is connecting", () => {
    expect(
      resolveGitManagerAvailability(
        connection({
          desired: true,
          phase: "connecting",
          stage: "opening",
          attempt: 2,
          network: "online",
          lastFailure: new ConnectionTransientError({
            reason: "liveness-timeout",
            detail: "No data from Local for 30 seconds. The connection is too slow or was lost.",
          }),
        }),
        serverConfig(true),
        "Local",
      ),
    ).toEqual({
      kind: "pending",
      reason: "Reconnecting to Local. Git Manager loads when the connection is back.",
    });
  });

  it("is ready only for a connected environment with read support", () => {
    expect(
      resolveGitManagerAvailability(
        connection({ desired: true, phase: "connected", network: "online" }),
        serverConfig(true),
        "Local",
      ),
    ).toEqual({ kind: "ready" });
  });
});

describe("resolveGitManagerCapabilityDisabledReasons", () => {
  it("fails each optional surface closed when capability fields are absent", () => {
    const configWithAbsentFields = {
      environment: { capabilities: { gitManagerReads: true } },
    } as ServerConfig;

    expect(resolveGitManagerCapabilityDisabledReasons(configWithAbsentFields)).toEqual({
      branchSync: GIT_MANAGER_BRANCH_SYNC_DISABLED_REASON,
      stashMerge: GIT_MANAGER_STASH_MERGE_DISABLED_REASON,
      rewrite: GIT_MANAGER_REWRITE_DISABLED_REASON,
      tag: GIT_MANAGER_TAG_DISABLED_REASON,
      pullRequests: GIT_MANAGER_PULL_REQUESTS_DISABLED_REASON,
      liveSignal: GIT_MANAGER_LIVE_SIGNAL_DISABLED_REASON,
      mergeInto: GIT_MANAGER_MERGE_INTO_DISABLED_REASON,
    });
  });

  it("enables only explicitly advertised optional surfaces", () => {
    const configWithCapabilities = {
      environment: {
        capabilities: makeTestExecutionEnvironmentCapabilities({
          gitManagerReads: true,
          gitManagerBranchSyncOperations: true,
          gitManagerLiveSignal: true,
          gitManagerMergeIntoOperations: true,
        }),
      },
    } as ServerConfig;

    expect(resolveGitManagerCapabilityDisabledReasons(configWithCapabilities)).toEqual({
      branchSync: null,
      stashMerge: GIT_MANAGER_STASH_MERGE_DISABLED_REASON,
      rewrite: GIT_MANAGER_REWRITE_DISABLED_REASON,
      tag: GIT_MANAGER_TAG_DISABLED_REASON,
      pullRequests: GIT_MANAGER_PULL_REQUESTS_DISABLED_REASON,
      liveSignal: null,
      mergeInto: null,
    });
  });
});
