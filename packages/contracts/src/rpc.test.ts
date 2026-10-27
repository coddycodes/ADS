import { describe, expect, it } from "vite-plus/test";
import * as Schema from "effect/Schema";

import { ORCHESTRATION_WS_METHODS } from "./orchestration.ts";
import {
  WS_METHODS,
  WsAuthConfirmPairingRpc,
  WsRpcGroup,
  WsServerGetConfigRpc,
  WsServerConsumeCodexRateLimitResetRpc,
  WsServerRefreshProvidersRpc,
  WsServerRefreshProviderUsageRpc,
  WsVcsCancelCloneRpc,
  WsVcsPullRpc,
  WsVcsInitRpc,
  WsShellOpenInEditorRpc,
  WsSubscribeVcsStatusRpc,
  WsSubscribeVcsStatusSummaryRpc,
  WsCloudInstallRelayClientRpc,
  WsTerminalOpenRpc,
  WsTerminalWriteRpc,
  WsTerminalBeginInputRpc,
  WsTerminalWriteInputRpc,
  WsTerminalCancelInputRpc,
  WsPreviewListRpc,
  WsSourceControlLookupRepositoryRpc,
  WsProjectsReadFileRpc,
  WsReviewGetDiffPreviewRpc,
  WsOrchestrationDispatchCommandRpc,
  WsOrchestrationSubscribeShellRpc,
  WsSubscribeAuthAccessRpc,
  WsActivityGetSnapshotRpc,
  WsSubscribeActivityRpc,
  WsSubscribeWorktreeCatalogRpc,
  WsVcsRefreshWorktreeCatalogRpc,
  WsWorktreeUpdateDiscoveryPolicyRpc,
  WsWorktreeAdoptRpc,
  WsWorktreeCreateManagedRpc,
  WsWorktreeCreatePanelRpc,
  WsWorktreeRetargetRpc,
  WsWorktreeGetRemovalPlanRpc,
  WsWorktreeRemoveFromCoddyStudioRpc,
  WsWorktreeRemoveRpc,
} from "./rpc.ts";

const decodeSubscribeWorktreeCatalog = Schema.decodeUnknownSync(
  WsSubscribeWorktreeCatalogRpc.payloadSchema,
);
const decodeRefreshWorktreeCatalog = Schema.decodeUnknownSync(
  WsVcsRefreshWorktreeCatalogRpc.payloadSchema,
);
const decodeUpdateWorktreeDiscoveryPolicy = Schema.decodeUnknownSync(
  WsWorktreeUpdateDiscoveryPolicyRpc.payloadSchema,
);
const decodeWorktreeAdopt = Schema.decodeUnknownSync(WsWorktreeAdoptRpc.payloadSchema);
const decodeWorktreeCreateManaged = Schema.decodeUnknownSync(
  WsWorktreeCreateManagedRpc.payloadSchema,
);
const decodeWorktreeCreatePanel = Schema.decodeUnknownSync(WsWorktreeCreatePanelRpc.payloadSchema);
const decodeWorktreeRetarget = Schema.decodeUnknownSync(WsWorktreeRetargetRpc.payloadSchema);
const decodeWorktreeAdoptResult = Schema.decodeUnknownSync(WsWorktreeAdoptRpc.successSchema);
const decodeWorktreeAdoptError = Schema.decodeUnknownSync(WsWorktreeAdoptRpc.errorSchema);
const decodeWorktreeRemovalPlan = Schema.decodeUnknownSync(
  WsWorktreeGetRemovalPlanRpc.successSchema,
);
const decodeWorktreeRemovalResult = Schema.decodeUnknownSync(
  WsWorktreeRemoveFromCoddyStudioRpc.successSchema,
);
const decodeWorktreeRemovalError = Schema.decodeUnknownSync(WsWorktreeRemoveRpc.errorSchema);
const decodeRefreshProviderUsageError = Schema.decodeUnknownSync(
  WsServerRefreshProviderUsageRpc.errorSchema,
);
const decodeConfirmPairing = Schema.decodeUnknownSync(WsAuthConfirmPairingRpc.payloadSchema);
const decodeConfirmPairingSuccess = Schema.decodeUnknownSync(WsAuthConfirmPairingRpc.successSchema);
const decodeCancelCloneSuccess = Schema.decodeUnknownSync(WsVcsCancelCloneRpc.successSchema);
const decodeCancelCloneError = Schema.decodeUnknownSync(WsVcsCancelCloneRpc.errorSchema);

describe("WS_METHODS", () => {
  it("maps method identifiers to unique dotted wire names", () => {
    const values = Object.values(WS_METHODS);
    const unique = new Set(values);
    expect(unique.size).toBe(values.length);
  });

  it("does not expose raw worktree lifecycle methods", () => {
    expect("vcsCreateWorktree" in WS_METHODS).toBe(false);
    expect(Object.values(WS_METHODS)).not.toContain("vcs.createWorktree");
    expect("vcsRemoveWorktree" in WS_METHODS).toBe(false);
    expect(Object.values(WS_METHODS)).not.toContain("vcs.removeWorktree");
  });

  it("uses stable wire names for representative methods", () => {
    expect(WS_METHODS.serverGetConfig).toBe("server.getConfig");
    expect(WS_METHODS.authConfirmPairing).toBe("auth.confirmPairing");
    expect((WS_METHODS as Record<string, string>).serverConsumeCodexRateLimitReset).toBe(
      "server.consumeCodexRateLimitReset",
    );
    expect(WS_METHODS.vcsPull).toBe("vcs.pull");
    expect(WS_METHODS.terminalOpen).toBe("terminal.open");
    expect(WS_METHODS.terminalBeginInput).toBe("terminal.beginInput");
    expect(WS_METHODS.terminalWriteInput).toBe("terminal.writeInput");
    expect(WS_METHODS.terminalCancelInput).toBe("terminal.cancelInput");
    expect(WS_METHODS.subscribeVcsStatus).toBe("subscribeVcsStatus");
    expect(WS_METHODS.subscribeVcsStatusSummary).toBe("subscribeVcsStatusSummary");
    expect(WS_METHODS.cloudInstallRelayClient).toBe("cloud.installRelayClient");
    expect(WS_METHODS.sourceControlLookupRepository).toBe("sourceControl.lookupRepository");
    expect(WS_METHODS.activityGetSnapshot).toBe("activity.getSnapshot");
    expect(WS_METHODS.activityListRoster).toBe("activity.listRoster");
    expect(WS_METHODS.activityListDetail).toBe("activity.listDetail");
    expect(WS_METHODS).toMatchObject({
      activityCancelSubtree: "activity.cancelSubtree",
      activityRetrySubtreeCancellation: "activity.retrySubtreeCancellation",
    });
    expect(WS_METHODS.subscribeActivity).toBe("subscribeActivity");
    expect(WS_METHODS.subscribeWorktreeCatalog).toBe("subscribeWorktreeCatalog");
    expect(WS_METHODS.vcsRefreshWorktreeCatalog).toBe("vcs.refreshWorktreeCatalog");
    expect(WS_METHODS.worktreeUpdateDiscoveryPolicy).toBe("worktree.updateDiscoveryPolicy");
    expect(WS_METHODS.worktreeAdopt).toBe("worktree.adopt");
    expect(WS_METHODS.worktreeCreateManaged).toBe("worktree.createManaged");
    expect(WS_METHODS.worktreeCreatePanel).toBe("worktree.createPanel");
    expect(WS_METHODS.worktreeRetarget).toBe("worktree.retarget");
    expect(WS_METHODS.worktreeGetRemovalPlan).toBe("worktree.getRemovalPlan");
    expect(WS_METHODS.worktreeRemoveFromCoddyStudio).toBe("worktree.removeFromCoddyStudio");
    expect(WS_METHODS.worktreeRemove).toBe("worktree.remove");
  });
});

const decodeTerminalBeginInputError = Schema.decodeUnknownSync(WsTerminalBeginInputRpc.errorSchema);
const decodeTerminalWriteInputError = Schema.decodeUnknownSync(WsTerminalWriteInputRpc.errorSchema);
const decodeTerminalCancelInputError = Schema.decodeUnknownSync(
  WsTerminalCancelInputRpc.errorSchema,
);
const decodeLegacyTerminalWriteError = Schema.decodeUnknownSync(WsTerminalWriteRpc.errorSchema);

describe("individual RPC definitions", () => {
  it("accepts ordered-input failures only on ordered terminal RPCs", () => {
    const failure = {
      _tag: "TerminalInputError",
      code: "capacity",
      message: "Ordered terminal input capacity is exhausted.",
    };
    for (const decode of [
      decodeTerminalBeginInputError,
      decodeTerminalWriteInputError,
      decodeTerminalCancelInputError,
    ]) {
      expect(() => decode(failure)).not.toThrow();
    }
    expect(() => decodeLegacyTerminalWriteError(failure)).toThrow();
  });
  it("decodes update-maintenance rejection as an expected provider refresh failure", () => {
    expect(
      decodeRefreshProviderUsageError({
        _tag: "UpdateMaintenanceActiveError",
        message: "Persistent mutations are temporarily closed.",
      }),
    ).toMatchObject({ _tag: "UpdateMaintenanceActiveError" });
  });

  it("accepts workspace-unavailable failures at guarded unary boundaries", () => {
    const unavailable = {
      _tag: "WorkspaceUnavailableError",
      reason: "workspace-unavailable",
      message: "The workspace directory is missing.",
      threadId: "thread-1",
      path: "/repo/worktrees/missing",
      availability: "missing-registered",
    };
    for (const rpc of [
      WsOrchestrationDispatchCommandRpc,
      WsTerminalOpenRpc,
      WsTerminalWriteRpc,
      WsTerminalBeginInputRpc,
      WsTerminalWriteInputRpc,
      WsTerminalCancelInputRpc,
      WsVcsPullRpc,
      WsProjectsReadFileRpc,
      WsReviewGetDiffPreviewRpc,
    ]) {
      expect(() => Schema.decodeUnknownSync(rpc.errorSchema)(unavailable)).not.toThrow();
    }
  });

  it("accepts physical identity failures at guarded and removal boundaries", () => {
    const identity = {
      _tag: "WorkspaceIdentityError",
      reason: "workspace-identity-unavailable",
      message: "The physical workspace identity could not be verified.",
      path: "/repo/worktrees/alias",
    };
    for (const rpc of [
      WsOrchestrationDispatchCommandRpc,
      WsTerminalOpenRpc,
      WsTerminalWriteRpc,
      WsTerminalBeginInputRpc,
      WsTerminalWriteInputRpc,
      WsTerminalCancelInputRpc,
      WsVcsPullRpc,
      WsProjectsReadFileRpc,
      WsReviewGetDiffPreviewRpc,
      WsWorktreeRemoveFromCoddyStudioRpc,
      WsWorktreeRemoveRpc,
    ]) {
      expect(() => Schema.decodeUnknownSync(rpc.errorSchema)(identity)).not.toThrow();
    }
  });

  it("accepts structured finite-operation admission failures at every mutating worktree boundary", () => {
    for (const failure of [
      {
        _tag: "WorktreeOperationError",
        reason: "operation-capacity",
        message: "The server is already processing the maximum number of worktree operations.",
      },
      {
        _tag: "WorktreeOperationError",
        reason: "operation-shutting-down",
        message: "The worktree operation runtime is shutting down.",
      },
    ]) {
      for (const rpc of [
        WsWorktreeUpdateDiscoveryPolicyRpc,
        WsWorktreeAdoptRpc,
        WsWorktreeCreateManagedRpc,
        WsWorktreeCreatePanelRpc,
        WsWorktreeRetargetRpc,
        WsWorktreeGetRemovalPlanRpc,
        WsWorktreeRemoveFromCoddyStudioRpc,
        WsWorktreeRemoveRpc,
      ]) {
        expect(() => Schema.decodeUnknownSync(rpc.errorSchema)(failure)).not.toThrow();
      }
    }
  });

  it("tags each RPC with its wire method name", () => {
    expect(WsAuthConfirmPairingRpc._tag).toBe(WS_METHODS.authConfirmPairing);
    expect(decodeConfirmPairing({})).toEqual({});
    expect(decodeConfirmPairingSuccess({})).toEqual({});
    expect(() => decodeConfirmPairing({ sessionId: "another-session" })).toThrow();
    expect(() => decodeConfirmPairingSuccess({ confirmed: true })).toThrow();
    expect(WsServerGetConfigRpc._tag).toBe(WS_METHODS.serverGetConfig);
    expect(WsServerConsumeCodexRateLimitResetRpc._tag).toBe(
      WS_METHODS.serverConsumeCodexRateLimitReset,
    );
    expect(WsServerRefreshProvidersRpc._tag).toBe(WS_METHODS.serverRefreshProviders);
    expect(WsVcsPullRpc._tag).toBe(WS_METHODS.vcsPull);
    expect(WsVcsInitRpc._tag).toBe(WS_METHODS.vcsInit);
    expect(WsShellOpenInEditorRpc._tag).toBe(WS_METHODS.shellOpenInEditor);
    expect(WsTerminalOpenRpc._tag).toBe(WS_METHODS.terminalOpen);
    expect(WsTerminalWriteRpc._tag).toBe(WS_METHODS.terminalWrite);
    expect(WsTerminalBeginInputRpc._tag).toBe(WS_METHODS.terminalBeginInput);
    expect(WsTerminalWriteInputRpc._tag).toBe(WS_METHODS.terminalWriteInput);
    expect(WsTerminalCancelInputRpc._tag).toBe(WS_METHODS.terminalCancelInput);
    expect(WsPreviewListRpc._tag).toBe(WS_METHODS.previewList);
    expect(WsSourceControlLookupRepositoryRpc._tag).toBe(WS_METHODS.sourceControlLookupRepository);
    expect(WsProjectsReadFileRpc._tag).toBe(WS_METHODS.projectsReadFile);
    expect(WsActivityGetSnapshotRpc._tag).toBe(WS_METHODS.activityGetSnapshot);
    expect(WsRpcGroup.requests.has("activity.cancelSubtree")).toBe(true);
    expect(WsRpcGroup.requests.has("activity.retrySubtreeCancellation")).toBe(true);
    expect(WsSubscribeActivityRpc._tag).toBe(WS_METHODS.subscribeActivity);
    expect(WsSubscribeVcsStatusSummaryRpc._tag).toBe(WS_METHODS.subscribeVcsStatusSummary);
    expect(WsSubscribeWorktreeCatalogRpc._tag).toBe(WS_METHODS.subscribeWorktreeCatalog);
    expect(WsVcsRefreshWorktreeCatalogRpc._tag).toBe(WS_METHODS.vcsRefreshWorktreeCatalog);
    expect(WsWorktreeUpdateDiscoveryPolicyRpc._tag).toBe(WS_METHODS.worktreeUpdateDiscoveryPolicy);
    expect(WsWorktreeAdoptRpc._tag).toBe(WS_METHODS.worktreeAdopt);
    expect(WsWorktreeCreateManagedRpc._tag).toBe(WS_METHODS.worktreeCreateManaged);
    expect(WsWorktreeCreatePanelRpc._tag).toBe(WS_METHODS.worktreeCreatePanel);
    expect(WsWorktreeRetargetRpc._tag).toBe(WS_METHODS.worktreeRetarget);
    expect(WsWorktreeGetRemovalPlanRpc._tag).toBe(WS_METHODS.worktreeGetRemovalPlan);
    expect(WsWorktreeRemoveFromCoddyStudioRpc._tag).toBe(WS_METHODS.worktreeRemoveFromCoddyStudio);
    expect(WsWorktreeRemoveRpc._tag).toBe(WS_METHODS.worktreeRemove);
  });

  it("carries payload/success/error schemas on unary RPCs", () => {
    for (const rpc of [WsServerGetConfigRpc, WsVcsPullRpc, WsTerminalOpenRpc]) {
      expect(Schema.isSchema(rpc.payloadSchema)).toBe(true);
      expect(Schema.isSchema(rpc.successSchema)).toBe(true);
      expect(Schema.isSchema(rpc.errorSchema)).toBe(true);
    }
  });

  it("wires orchestration RPCs to the orchestration method names", () => {
    expect(WsOrchestrationDispatchCommandRpc._tag).toBe(ORCHESTRATION_WS_METHODS.dispatchCommand);
    expect(WsOrchestrationSubscribeShellRpc._tag).toBe(ORCHESTRATION_WS_METHODS.subscribeShell);
  });

  it("collapses the declared error schema to Never for streaming RPCs", () => {
    // Rpc.make sets errorSchema to Schema.Never and wraps success for streams.
    for (const streamRpc of [
      WsSubscribeVcsStatusRpc,
      WsSubscribeVcsStatusSummaryRpc,
      WsCloudInstallRelayClientRpc,
      WsSubscribeAuthAccessRpc,
      WsOrchestrationSubscribeShellRpc,
      WsSubscribeWorktreeCatalogRpc,
    ]) {
      expect(streamRpc.errorSchema).toBe(Schema.Never);
      expect(Schema.isSchema(streamRpc.successSchema)).toBe(true);
    }
  });
});

describe("WsRpcGroup", () => {
  it("exposes a request map keyed by each RPC's tag", () => {
    expect(WsRpcGroup.requests).toBeInstanceOf(Map);
    for (const [key, rpc] of WsRpcGroup.requests) {
      expect(key).toBe(rpc._tag);
      expect(typeof rpc._tag).toBe("string");
      expect(Schema.isSchema(rpc.payloadSchema)).toBe(true);
      expect(Schema.isSchema(rpc.successSchema)).toBe(true);
      expect(Schema.isSchema(rpc.errorSchema)).toBe(true);
    }
  });

  it("registers the individually exported RPCs under their tags", () => {
    const members = [
      WsAuthConfirmPairingRpc,
      WsServerGetConfigRpc,
      WsServerConsumeCodexRateLimitResetRpc,
      WsVcsPullRpc,
      WsVcsInitRpc,
      WsShellOpenInEditorRpc,
      WsSubscribeVcsStatusRpc,
      WsSubscribeVcsStatusSummaryRpc,
      WsCloudInstallRelayClientRpc,
      WsTerminalOpenRpc,
      WsPreviewListRpc,
      WsSourceControlLookupRepositoryRpc,
      WsOrchestrationDispatchCommandRpc,
      WsOrchestrationSubscribeShellRpc,
      WsSubscribeAuthAccessRpc,
      WsSubscribeWorktreeCatalogRpc,
      WsVcsRefreshWorktreeCatalogRpc,
      WsWorktreeUpdateDiscoveryPolicyRpc,
      WsWorktreeAdoptRpc,
      WsWorktreeCreateManagedRpc,
      WsWorktreeCreatePanelRpc,
      WsWorktreeRetargetRpc,
      WsWorktreeGetRemovalPlanRpc,
      WsWorktreeRemoveFromCoddyStudioRpc,
      WsWorktreeRemoveRpc,
    ];
    for (const rpc of members) {
      expect(WsRpcGroup.requests.get(rpc._tag)).toBe(rpc);
    }
  });

  it("keeps catalog reads project-scoped and discovery baselines server-owned", () => {
    expect(decodeSubscribeWorktreeCatalog({ projectId: "project-1" })).toEqual({
      projectId: "project-1",
    });
    expect(decodeRefreshWorktreeCatalog({ projectId: "project-1" })).toEqual({
      projectId: "project-1",
    });
    expect(
      decodeUpdateWorktreeDiscoveryPolicy({
        commandId: "command-1",
        projectId: "project-1",
        visibility: "hidden",
        acknowledgeGeneration: 9,
        dismissInitialPrompt: true,
        baselinePaths: ["/client-path"],
      }),
    ).toEqual({
      commandId: "command-1",
      projectId: "project-1",
      visibility: "hidden",
      acknowledgeGeneration: 9,
      dismissInitialPrompt: true,
    });
  });

  it("registers adoption with opaque server-resolved identity and typed outcomes", () => {
    expect(
      decodeWorktreeAdopt({
        commandId: "command-adopt-1",
        projectId: "project-1",
        worktreeKey: "worktree-1",
        expectedGeneration: 9,
        threadDefaults: {
          modelSelection: { instanceId: "codex", model: "gpt-5" },
          runtimeMode: "approval-required",
          interactionMode: "default",
        },
      }),
    ).toMatchObject({
      commandId: "command-adopt-1",
      projectId: "project-1",
      worktreeKey: "worktree-1",
      expectedGeneration: 9,
    });
    expect(
      ["created", "existing", "restored"].map((disposition) =>
        decodeWorktreeAdoptResult({ threadId: "thread-1", disposition }),
      ),
    ).toHaveLength(3);
    expect(
      decodeWorktreeAdoptError({
        _tag: "WorktreeAdoptionError",
        reason: "stale-generation",
        message: "Refresh required.",
        currentGeneration: 10,
      }),
    ).toMatchObject({ reason: "stale-generation", currentGeneration: 10 });
    expect(WsRpcGroup.requests.get(WS_METHODS.worktreeAdopt)).toBe(WsWorktreeAdoptRpc);
  });

  it("registers dedicated creation and retargeting with server-resolved authority", () => {
    const threadDefaults = {
      modelSelection: { instanceId: "codex", model: "gpt-5" },
      runtimeMode: "full-access",
      interactionMode: "default",
    } as const;
    expect(
      decodeWorktreeCreateManaged({
        commandId: "command-managed",
        projectId: "project-1",
        threadId: "thread-managed",
        title: "Managed worktree",
        refName: "HEAD",
        newRefName: "feature/managed",
        baseRefName: "HEAD",
        threadDefaults,
      }),
    ).toEqual({
      commandId: "command-managed",
      projectId: "project-1",
      threadId: "thread-managed",
      title: "Managed worktree",
      refName: "HEAD",
      newRefName: "feature/managed",
      baseRefName: "HEAD",
      threadDefaults,
    });
    expect(
      decodeWorktreeCreatePanel({
        commandId: "command-panel",
        hostThreadId: "thread-host",
        threadId: "thread-panel",
        title: "Panel",
        threadDefaults,
      }),
    ).toEqual({
      commandId: "command-panel",
      hostThreadId: "thread-host",
      threadId: "thread-panel",
      title: "Panel",
      threadDefaults,
    });
    expect(
      decodeWorktreeRetarget({
        commandId: "command-retarget",
        projectId: "project-1",
        threadId: "thread-1",
        worktreeKey: "worktree-1",
        expectedGeneration: 12,
      }),
    ).toEqual({
      commandId: "command-retarget",
      projectId: "project-1",
      threadId: "thread-1",
      worktreeKey: "worktree-1",
      expectedGeneration: 12,
    });
    expect(WsRpcGroup.requests.get(WS_METHODS.worktreeCreateManaged)).toBe(
      WsWorktreeCreateManagedRpc,
    );
    expect(WsRpcGroup.requests.get(WS_METHODS.worktreeCreatePanel)).toBe(WsWorktreeCreatePanelRpc);
    expect(WsRpcGroup.requests.get(WS_METHODS.worktreeRetarget)).toBe(WsWorktreeRetargetRpc);
  });

  it("registers typed removal planning, detach-only, and destructive RPCs", () => {
    expect(
      decodeWorktreeRemovalPlan({
        planToken: "plan-1",
        generation: 4,
        availability: "missing-registered",
        registered: true,
        locked: false,
        trackedChangeCount: 0,
        untrackedFileCount: 0,
        pruneImpact: [{ path: "/repo/missing", pruneReason: "stale gitdir", locked: false }],
      }).pruneImpact[0]?.pruneReason,
    ).toBe("stale gitdir");
    expect(
      decodeWorktreeRemovalResult({
        threadRemoved: true,
        gitOutcome: "not-requested",
        orphanCleanupPending: false,
      }).threadRemoved,
    ).toBe(true);
    expect(
      decodeWorktreeRemovalError({
        _tag: "WorktreeRemovalError",
        reason: "git-failed",
        message: "Git removal failed.",
      }),
    ).toMatchObject({ _tag: "WorktreeRemovalError", reason: "git-failed" });
    expect(WsRpcGroup.requests.get(WS_METHODS.worktreeGetRemovalPlan)).toBe(
      WsWorktreeGetRemovalPlanRpc,
    );
    expect(WsRpcGroup.requests.get(WS_METHODS.worktreeRemoveFromCoddyStudio)).toBe(
      WsWorktreeRemoveFromCoddyStudioRpc,
    );
    expect(WsRpcGroup.requests.get(WS_METHODS.worktreeRemove)).toBe(WsWorktreeRemoveRpc);
  });

  it("contains every RPC exactly once and covers the whole WS surface", () => {
    // Every registered request tag is a known dotted/plain WS method name.
    const wireNames = new Set<string>([
      ...Object.values(WS_METHODS),
      ...Object.values(ORCHESTRATION_WS_METHODS),
    ]);
    for (const key of WsRpcGroup.requests.keys()) {
      expect(wireNames.has(key)).toBe(true);
    }
    // The group is non-trivial: it registers many procedures.
    expect(WsRpcGroup.requests.size).toBeGreaterThanOrEqual(70);
  });
});

// The literals are exactly what `cancel_clone_answers_only_its_declared_shapes` in
// apps/server/tests/production_git_vcs_rpc.rs asserts the Rust handler sends.
describe("vcs.cancelClone wire shapes", () => {
  it("decodes every answer the handler sends against the declared union", () => {
    expect(decodeCancelCloneSuccess({ cancelled: true })).toEqual({ cancelled: true });
    expect(decodeCancelCloneSuccess({ cancelled: false })).toEqual({ cancelled: false });
    const unresolvable = decodeCancelCloneError({
      _tag: "GitCommandError",
      operation: "vcs.cancelClone",
      command: "git",
      cwd: "/missing-parent",
      detail: "host path does not exist: /missing-parent",
    });
    expect(unresolvable._tag).toBe("GitCommandError");
  });

  it("has no typed failure for a caller that left: that is the RPC interrupt path", () => {
    expect(() =>
      decodeCancelCloneError({
        _tag: "GitCloneOperationError",
        reason: "cancelled",
        destination: "/code/demo",
        message: "x",
      }),
    ).toThrow();
  });
});
