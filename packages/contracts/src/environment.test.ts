import { Schema } from "effect";
import { describe, expect, it } from "vite-plus/test";

import {
  ExecutionEnvironmentDescriptor,
  ExecutionEnvironmentCapabilities,
  MIN_COMPATIBLE_REMOTE_PROTOCOL,
  REMOTE_PROTOCOL_VERSION,
} from "./environment.ts";
import { OrchestrationRpcSchemas } from "./orchestration.ts";

const decodeExecutionEnvironmentDescriptor = Schema.decodeUnknownSync(
  ExecutionEnvironmentDescriptor,
);

const gitManagerCapabilities = [
  "gitManagerReads",
  "gitManagerCommitOperations",
  "gitManagerBranchSyncOperations",
  "gitManagerStashMergeOperations",
  "gitManagerMergeIntoOperations",
  "gitManagerPartialStaging",
  "gitManagerRewriteOperations",
  "gitManagerTagOperations",
  "gitManagerLiveSignal",
  "gitManagerPullRequests",
] as const;

const descriptor = {
  environmentId: "local",
  label: "Local",
  platform: { os: "darwin" as const, arch: "arm64" as const },
  serverVersion: "0.1.0",
};

const LegacyExecutionEnvironmentDescriptor = Schema.Struct({
  environmentId: Schema.String,
  label: Schema.String,
  platform: Schema.Struct({
    os: Schema.Literals(["darwin", "linux", "windows", "unknown"]),
    arch: Schema.Literals(["arm64", "x64", "other"]),
  }),
  serverVersion: Schema.String,
  capabilities: Schema.Struct({
    repositoryIdentity: Schema.Boolean,
  }),
});
const decodeLegacyExecutionEnvironmentDescriptor = Schema.decodeUnknownSync(
  LegacyExecutionEnvironmentDescriptor,
);

const legacyClientDecoders = {
  "orchestration.dispatchCommand": Schema.decodeUnknownSync(
    OrchestrationRpcSchemas.dispatchCommand.output,
  ),
  "orchestration.subscribeThread": Schema.decodeUnknownSync(
    OrchestrationRpcSchemas.subscribeThread.output,
  ),
} as const;

const decodeTerminalCapabilities = Schema.decodeUnknownSync(ExecutionEnvironmentCapabilities);

describe("execution environment contracts", () => {
  it("defaults terminal size ownership off and preserves advertised support", () => {
    expect(decodeTerminalCapabilities({}).terminalSizeOwnership).toBe(false);
    expect(decodeTerminalCapabilities({ terminalSizeOwnership: true }).terminalSizeOwnership).toBe(
      true,
    );
  });
  it("defaults Pull Requests capabilities off for older servers and preserves advertised support", () => {
    const legacy = decodeExecutionEnvironmentDescriptor({
      environmentId: "env-1",
      label: "Local",
      platform: { os: "linux", arch: "x64" },
      serverVersion: "1.0.0",
      capabilities: {},
    });
    expect(legacy.capabilities).toMatchObject({
      pullRequestsReads: false,
      pullRequestsMutations: false,
      gitPullRequestBranchSelection: false,
    });
    const current = decodeExecutionEnvironmentDescriptor({
      ...legacy,
      capabilities: {
        pullRequestsReads: true,
        pullRequestsMutations: true,
        gitPullRequestBranchSelection: true,
      },
    });
    expect(current.capabilities).toMatchObject({
      pullRequestsReads: true,
      pullRequestsMutations: true,
      gitPullRequestBranchSelection: true,
    });
  });
  it("defaults the profiler capability off for older servers and preserves advertised support", () => {
    expect(decodeTerminalCapabilities({}).profiler).toBe(false);
    expect(decodeTerminalCapabilities({ profiler: true }).profiler).toBe(true);
  });
  it("defaults ordered terminal input support to false when omitted", () => {
    expect(decodeTerminalCapabilities({}).terminalOrderedInput).toBe(false);
  });
  it("defaults clone re-attach off for older servers and preserves advertised support", () => {
    expect(decodeTerminalCapabilities({}).vcsCloneReattach).toBe(false);
    expect(decodeTerminalCapabilities({ vcsCloneReattach: true }).vcsCloneReattach).toBe(true);
  });
  it("pins the remote protocol window constants", () => {
    expect(REMOTE_PROTOCOL_VERSION).toBe(1);
    expect(MIN_COMPATIBLE_REMOTE_PROTOCOL).toBe(1);
  });

  it("defaults the protocol window to 0/0 for an old descriptor", () => {
    const decoded = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      capabilities: { repositoryIdentity: true },
    });

    expect(decoded.remoteProtocolVersion).toBe(0);
    expect(decoded.minCompatibleRemoteProtocol).toBe(0);
  });

  it("decodes an advertised protocol window", () => {
    const decoded = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      remoteProtocolVersion: 1,
      minCompatibleRemoteProtocol: 1,
      capabilities: { repositoryIdentity: true },
    });

    expect(decoded.remoteProtocolVersion).toBe(1);
    expect(decoded.minCompatibleRemoteProtocol).toBe(1);
  });

  it("rejects negative protocol window values", () => {
    for (const field of ["remoteProtocolVersion", "minCompatibleRemoteProtocol"] as const) {
      expect(() =>
        decodeExecutionEnvironmentDescriptor({
          ...descriptor,
          [field]: -1,
          capabilities: { repositoryIdentity: true },
        }),
      ).toThrow();
    }
  });

  it("rejects fractional protocol window values", () => {
    for (const field of ["remoteProtocolVersion", "minCompatibleRemoteProtocol"] as const) {
      expect(() =>
        decodeExecutionEnvironmentDescriptor({
          ...descriptor,
          [field]: 1.5,
          capabilities: { repositoryIdentity: true },
        }),
      ).toThrow();
    }
  });

  it("defaults passive VCS summary support to false for an old descriptor", () => {
    expect(
      decodeExecutionEnvironmentDescriptor({
        ...descriptor,
        capabilities: { repositoryIdentity: true },
      }).capabilities.vcsStatusSummary,
    ).toBe(false);
  });

  it("defaults every Git Manager capability to false and decodes every advertised capability", () => {
    const absent = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      capabilities: { repositoryIdentity: true },
    });
    const advertised = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      capabilities: {
        repositoryIdentity: true,
        ...Object.fromEntries(gitManagerCapabilities.map((capability) => [capability, true])),
      },
    });

    for (const capability of gitManagerCapabilities) {
      expect(absent.capabilities[capability]).toBe(false);
      expect(advertised.capabilities[capability]).toBe(true);
    }
  });

  it("defaults worktree catalog support to false for an old descriptor", () => {
    expect(
      decodeExecutionEnvironmentDescriptor({
        ...descriptor,
        capabilities: { repositoryIdentity: true },
      }).capabilities.worktreeCatalog,
    ).toBe(false);
  });

  it("decodes an advertised complete worktree catalog surface", () => {
    expect(
      decodeExecutionEnvironmentDescriptor({
        ...descriptor,
        capabilities: { repositoryIdentity: true, worktreeCatalog: true },
      }).capabilities.worktreeCatalog,
    ).toBe(true);
  });

  it("defaults worktree catalog refresh-reason support to false for an old descriptor", () => {
    expect(
      decodeExecutionEnvironmentDescriptor({
        ...descriptor,
        capabilities: { repositoryIdentity: true, worktreeCatalog: true },
      }).capabilities.worktreeCatalogRefreshReason,
    ).toBe(false);
  });

  it("decodes advertised worktree catalog refresh-reason support", () => {
    expect(
      decodeExecutionEnvironmentDescriptor({
        ...descriptor,
        capabilities: {
          repositoryIdentity: true,
          worktreeCatalog: true,
          worktreeCatalogRefreshReason: true,
        },
      }).capabilities.worktreeCatalogRefreshReason,
    ).toBe(true);
  });

  it("defaults storage identity to null for an older remote descriptor", () => {
    const decoded = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      capabilities: { repositoryIdentity: true },
    });

    expect(decoded.storageInstanceId).toBeNull();
  });

  it("decodes a new server storage identity", () => {
    const decoded = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      storageInstanceId: "0d93cbea-f237-4f37-8829-d816667be35f",
      capabilities: { repositoryIdentity: true },
    });

    expect(decoded.storageInstanceId).toBe("0d93cbea-f237-4f37-8829-d816667be35f");
  });

  it("accepts a non-UUID storage identity from a third-party server", () => {
    const decoded = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      storageInstanceId: "third-party-store",
      capabilities: { repositoryIdentity: true },
    });

    expect(decoded.storageInstanceId).toBe("third-party-store");
  });

  it("trims surrounding whitespace from a supplied storage identity", () => {
    const decoded = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      storageInstanceId: "  third-party-store  ",
      capabilities: { repositoryIdentity: true },
    });

    expect(decoded.storageInstanceId).toBe("third-party-store");
  });

  it("rejects a whitespace-only storage identity", () => {
    expect(() =>
      decodeExecutionEnvironmentDescriptor({
        ...descriptor,
        storageInstanceId: "   ",
        capabilities: { repositoryIdentity: true },
      }),
    ).toThrow();
  });

  it("defaults the activity protocol version for an old descriptor", () => {
    expect(
      decodeExecutionEnvironmentDescriptor({
        ...descriptor,
        capabilities: { repositoryIdentity: true },
      }).capabilities.activityProtocolVersion,
    ).toBeNull();
  });

  it("allows a server descriptor to advertise activity protocol version 2", () => {
    expect(
      decodeExecutionEnvironmentDescriptor({
        ...descriptor,
        capabilities: {
          repositoryIdentity: true,
          activityProtocolVersion: 2,
        },
      }).capabilities.activityProtocolVersion,
    ).toBe(2);
  });

  it("rejects obsolete and future activity protocol versions", () => {
    for (const activityProtocolVersion of [0, 1, 3]) {
      expect(() =>
        decodeExecutionEnvironmentDescriptor({
          ...descriptor,
          capabilities: {
            repositoryIdentity: true,
            activityProtocolVersion,
          },
        }),
      ).toThrow();
    }
  });

  it("keeps a legacy non-activity client compatible with additive descriptors and conversation traffic", () => {
    const legacyDescriptor = decodeLegacyExecutionEnvironmentDescriptor({
      ...descriptor,
      storageInstanceId: "0d93cbea-f237-4f37-8829-d816667be35f",
      capabilities: {
        repositoryIdentity: true,
        activityProtocolVersion: 1,
      },
    });
    const dispatchResponse = legacyClientDecoders["orchestration.dispatchCommand"]({
      sequence: 7,
      projectId: "project-1",
    });
    const conversationEvent = legacyClientDecoders["orchestration.subscribeThread"]({
      kind: "event",
      event: {
        sequence: 8,
        eventId: "event-1",
        aggregateKind: "thread",
        aggregateId: "thread-1",
        occurredAt: "2026-07-29T12:00:00Z",
        commandId: null,
        causationEventId: null,
        correlationId: null,
        metadata: {},
        type: "thread.message-sent",
        payload: {
          threadId: "thread-1",
          messageId: "message-1",
          role: "assistant",
          text: "Conversation remains available.",
          turnId: null,
          streaming: false,
          createdAt: "2026-07-29T12:00:00Z",
          updatedAt: "2026-07-29T12:00:00Z",
        },
      },
    });

    expect(Object.keys(legacyClientDecoders)).toEqual([
      "orchestration.dispatchCommand",
      "orchestration.subscribeThread",
    ]);
    expect(legacyDescriptor.capabilities).toEqual({ repositoryIdentity: true });
    expect(dispatchResponse).toEqual({ sequence: 7, projectId: "project-1" });
    expect(conversationEvent.kind).toBe("event");
    if (conversationEvent.kind === "event") {
      expect(conversationEvent.event.type).toBe("thread.message-sent");
      expect(conversationEvent.event.payload).toMatchObject({
        threadId: "thread-1",
        text: "Conversation remains available.",
      });
    }
  });
});

describe("remote update descriptor surface", () => {
  it("defaults bootId and remoteUpdateProgress for an older server", () => {
    const decoded = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      capabilities: {},
    });
    expect(decoded.bootId).toBeNull();
    expect(decoded.capabilities.remoteUpdateProgress).toBe(false);
  });

  it("preserves an advertised bootId and remote update progress support", () => {
    const decoded = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      bootId: "00000000-0000-4000-8000-000000000003",
      capabilities: { remoteUpdateProgress: true },
    });
    expect(decoded.bootId).toBe("00000000-0000-4000-8000-000000000003");
    expect(decoded.capabilities.remoteUpdateProgress).toBe(true);
  });

  it("defaults remoteUpdateControl to false and remoteUpdateSupport to null for older servers", () => {
    const decoded = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      capabilities: { repositoryIdentity: true },
    });
    expect(decoded.capabilities.remoteUpdateControl).toBe(false);
    expect(decoded.remoteUpdateSupport).toBeNull();
  });

  it("decodes an embedded remote update support block", () => {
    const decoded = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      capabilities: { repositoryIdentity: true, remoteUpdateControl: true },
      remoteUpdateSupport: { installMode: "manual", reason: "manual-update-required" },
    });
    expect(decoded.capabilities.remoteUpdateControl).toBe(true);
    expect(decoded.remoteUpdateSupport).toEqual({
      installMode: "manual",
      reason: "manual-update-required",
      installKind: "unknown",
    });
  });
});

it("defaults attachment staging to false for legacy descriptors", () => {
  expect(decodeTerminalCapabilities({}).attachmentStaging).toBe(false);
  expect(decodeTerminalCapabilities({ attachmentStaging: true }).attachmentStaging).toBe(true);
});
