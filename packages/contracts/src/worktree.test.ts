import * as Schema from "effect/Schema";
import { describe, expect, it } from "vite-plus/test";

import {
  ProjectWorktreeDiscoveryPolicy,
  VcsAdoptedWorktreeStatus,
  VcsWorktreeCatalogSnapshot,
  VcsWorktreeDescriptor,
  WorktreeAdoptResult,
  WorktreeAdoptionError,
  WorktreeCatalogError,
  WorktreeOperationError,
  WorktreeRemovalError,
  WorktreeRemovalPlan,
  WorktreeRemovalResult,
  WorkspaceUnavailableError,
  WorkspaceIdentityError,
  WorktreeCatalogInput,
  WorktreeCatalogRefreshInput,
  WorktreeDiscoveryPolicyUpdateInput,
} from "./worktree.ts";
import {
  WorktreeAdoptInput,
  WorktreeGetRemovalPlanInput,
  WorktreeRemoveFromCoddyStudioInput,
  WorktreeRemoveInput,
} from "./rpc.ts";

const decodeDescriptor = Schema.decodeUnknownSync(VcsWorktreeDescriptor);
const encodeDescriptor = Schema.encodeSync(VcsWorktreeDescriptor);
const decodeWorktreeDiscoveryPolicy = Schema.decodeUnknownSync(ProjectWorktreeDiscoveryPolicy);
const decodeSnapshot = Schema.decodeUnknownSync(VcsWorktreeCatalogSnapshot);
const encodeSnapshot = Schema.encodeSync(VcsWorktreeCatalogSnapshot);
const decodeAdoptedWorkspace = Schema.decodeUnknownSync(VcsAdoptedWorktreeStatus);
const encodeAdoptedWorkspace = Schema.encodeSync(VcsAdoptedWorktreeStatus);
const decodeRemovalPlan = Schema.decodeUnknownSync(WorktreeRemovalPlan);
const decodeRemovalResult = Schema.decodeUnknownSync(WorktreeRemovalResult);
const encodeRemovalResult = Schema.encodeSync(WorktreeRemovalResult);
const decodeCatalogError = Schema.decodeUnknownSync(WorktreeCatalogError);
const encodeCatalogError = Schema.encodeSync(WorktreeCatalogError);
const decodeAdoptionError = Schema.decodeUnknownSync(WorktreeAdoptionError);
const encodeAdoptionError = Schema.encodeSync(WorktreeAdoptionError);
const decodeRemovalError = Schema.decodeUnknownSync(WorktreeRemovalError);
const encodeRemovalError = Schema.encodeSync(WorktreeRemovalError);
const decodeOperationError = Schema.decodeUnknownSync(WorktreeOperationError);
const encodeOperationError = Schema.encodeSync(WorktreeOperationError);
const decodeWorkspaceUnavailableError = Schema.decodeUnknownSync(WorkspaceUnavailableError);
const encodeWorkspaceUnavailableError = Schema.encodeSync(WorkspaceUnavailableError);
const decodeWorkspaceIdentityError = Schema.decodeUnknownSync(WorkspaceIdentityError);
const encodeWorkspaceIdentityError = Schema.encodeSync(WorkspaceIdentityError);
const decodeCatalogInput = Schema.decodeUnknownSync(WorktreeCatalogInput);
const decodeAdoptInput = Schema.decodeUnknownSync(WorktreeAdoptInput);
const encodeAdoptInput = Schema.encodeSync(WorktreeAdoptInput);
const decodeAdoptResult = Schema.decodeUnknownSync(WorktreeAdoptResult);
const decodeGetRemovalPlanInput = Schema.decodeUnknownSync(WorktreeGetRemovalPlanInput);
const decodeRemoveFromCoddyStudioInput = Schema.decodeUnknownSync(WorktreeRemoveFromCoddyStudioInput);
const decodeRemoveInput = Schema.decodeUnknownSync(WorktreeRemoveInput);
const encodeGetRemovalPlanInput = Schema.encodeSync(WorktreeGetRemovalPlanInput);
const encodeRemoveFromCoddyStudioInput = Schema.encodeSync(WorktreeRemoveFromCoddyStudioInput);
const encodeRemoveInput = Schema.encodeSync(WorktreeRemoveInput);
const decodeCatalogRefreshInput = Schema.decodeUnknownSync(WorktreeCatalogRefreshInput);
const decodeDiscoveryPolicyUpdateInput = Schema.decodeUnknownSync(
  WorktreeDiscoveryPolicyUpdateInput,
);

const baseDescriptor = {
  worktreeKey: "worktree-primary",
  path: "/repo",
  branch: "main",
  head: "abc123",
  isPrimary: true,
  isBare: false,
  locked: false,
  registrationState: "registered",
  directoryState: "present",
  adoptionState: "none",
  eligibleForAdoption: false,
} as const;

describe("worktree catalog schemas", () => {
  it("keeps removal planning and detach-only inputs free of client path authority", () => {
    expect(
      encodeGetRemovalPlanInput(
        decodeGetRemovalPlanInput({
          projectId: "project-1",
          threadId: "thread-1",
          path: "/client-controlled",
        }),
      ),
    ).toEqual({ projectId: "project-1", threadId: "thread-1" });
    expect(
      encodeRemoveFromCoddyStudioInput(
        decodeRemoveFromCoddyStudioInput({
          commandId: "detach-command-1",
          projectId: "project-1",
          threadId: "thread-1",
          expectedGeneration: 9,
          planToken: "client-token",
          path: "/client-controlled",
        }),
      ),
    ).toEqual({
      commandId: "detach-command-1",
      projectId: "project-1",
      threadId: "thread-1",
    });
  });

  it("requires every destructive removal choice and still strips client paths", () => {
    const input = {
      commandId: "remove-command-1",
      projectId: "project-1",
      threadId: "thread-1",
      mode: "delete-git-worktree",
      expectedGeneration: 9,
      planToken: "plan-token-1",
      forceDirty: false,
      confirmRepositoryWidePrune: false,
      path: "/client-controlled",
    } as const;

    expect(encodeRemoveInput(decodeRemoveInput(input))).toEqual({
      commandId: "remove-command-1",
      projectId: "project-1",
      threadId: "thread-1",
      mode: "delete-git-worktree",
      expectedGeneration: 9,
      planToken: "plan-token-1",
      forceDirty: false,
      confirmRepositoryWidePrune: false,
    });
    for (const field of [
      "mode",
      "expectedGeneration",
      "planToken",
      "forceDirty",
      "confirmRepositoryWidePrune",
    ] as const) {
      const incomplete = { ...input } as Record<string, unknown>;
      delete incomplete[field];
      expect(() => decodeRemoveInput(incomplete)).toThrow();
    }
  });

  it("encodes adoption identity, generation, and ordinary thread defaults without a path", () => {
    const decoded = decodeAdoptInput({
      commandId: "command-adopt-1",
      projectId: "project-1",
      worktreeKey: "worktree-1",
      expectedGeneration: 9,
      threadDefaults: {
        modelSelection: { instanceId: "codex", model: "gpt-5" },
        runtimeMode: "full-access",
        interactionMode: "plan",
      },
      path: "/client-controlled",
    });

    expect(encodeAdoptInput(decoded)).toEqual({
      commandId: "command-adopt-1",
      projectId: "project-1",
      worktreeKey: "worktree-1",
      expectedGeneration: 9,
      threadDefaults: {
        modelSelection: { instanceId: "codex", model: "gpt-5" },
        runtimeMode: "full-access",
        interactionMode: "plan",
      },
    });
  });

  it("decodes every adoption disposition", () => {
    expect(
      ["created", "existing", "restored"].map((disposition) =>
        decodeAdoptResult({ threadId: "thread-1", disposition }),
      ),
    ).toEqual([
      { threadId: "thread-1", disposition: "created" },
      { threadId: "thread-1", disposition: "existing" },
      { threadId: "thread-1", disposition: "restored" },
    ]);
  });

  it("preserves the current generation on stale and ineligible adoption failures", () => {
    expect(
      ["stale-generation", "ineligible"].map((reason) =>
        decodeAdoptionError({
          _tag: "WorktreeAdoptionError",
          reason,
          message: "Adoption must be revalidated.",
          currentGeneration: 12,
        }),
      ),
    ).toMatchObject([
      { reason: "stale-generation", currentGeneration: 12 },
      { reason: "ineligible", currentGeneration: 12 },
    ]);
  });

  it("decodes a command conflict without a server-resolved path", () => {
    expect(
      decodeAdoptionError({
        _tag: "WorktreeAdoptionError",
        reason: "command-conflict",
        message: "The command ID was already used with a different adoption payload.",
        currentGeneration: 12,
      }),
    ).toMatchObject({
      _tag: "WorktreeAdoptionError",
      reason: "command-conflict",
      message: "The command ID was already used with a different adoption payload.",
      currentGeneration: 12,
    });
  });

  it("decodes a discovery-policy command conflict", () => {
    expect(
      decodeCatalogError({
        _tag: "WorktreeCatalogError",
        reason: "command-conflict",
        message: "The command ID was already used by another worktree mutation.",
      }),
    ).toMatchObject({
      _tag: "WorktreeCatalogError",
      reason: "command-conflict",
    });
  });

  it("accepts project-scoped catalog reads without client-supplied paths", () => {
    expect(decodeCatalogInput({ projectId: "project-1" })).toEqual({ projectId: "project-1" });
    expect(decodeCatalogRefreshInput({ projectId: "project-1" })).toEqual({
      projectId: "project-1",
    });
    expect(() => decodeCatalogInput({ projectId: "" })).toThrow();
  });

  it("accepts only compatible catalog refresh reasons", () => {
    expect(decodeCatalogRefreshInput({ projectId: "project-1" })).toEqual({
      projectId: "project-1",
    });
    expect(decodeCatalogRefreshInput({ projectId: "project-1", reason: "focus" })).toEqual({
      projectId: "project-1",
      reason: "focus",
    });
    expect(decodeCatalogRefreshInput({ projectId: "project-1", reason: "explicit" })).toEqual({
      projectId: "project-1",
      reason: "explicit",
    });
    expect(() =>
      decodeCatalogRefreshInput({ projectId: "project-1", reason: "scheduled" }),
    ).toThrow();
  });

  it("accepts only server-compacted discovery policy update controls", () => {
    expect(
      decodeDiscoveryPolicyUpdateInput({
        commandId: "command-1",
        projectId: "project-1",
        visibility: "shown",
        acknowledgeGeneration: 7,
        dismissInitialPrompt: true,
      }),
    ).toEqual({
      commandId: "command-1",
      projectId: "project-1",
      visibility: "shown",
      acknowledgeGeneration: 7,
      dismissInitialPrompt: true,
    });
    expect(
      decodeDiscoveryPolicyUpdateInput({
        commandId: "command-1",
        projectId: "project-1",
        baselinePaths: ["/client-controlled"],
      }),
    ).toEqual({ commandId: "command-1", projectId: "project-1" });
  });

  it("round-trips every descriptor state and a degraded retained snapshot", () => {
    const descriptors = [
      baseDescriptor,
      {
        ...baseDescriptor,
        worktreeKey: "worktree-prunable",
        path: "/repo/missing",
        branch: null,
        head: null,
        isPrimary: false,
        isBare: true,
        locked: true,
        lockReason: "maintenance",
        registrationState: "prunable",
        directoryState: "missing",
        adoptionState: "active",
        adoptedThreadId: "thread-active",
        eligibleForAdoption: false,
      },
      {
        ...baseDescriptor,
        worktreeKey: "worktree-unknown",
        path: "/repo/feature",
        isPrimary: false,
        registrationState: "registered",
        directoryState: "unknown",
        adoptionState: "archived",
        adoptedThreadId: "thread-archived",
        eligibleForAdoption: true,
      },
    ] as const;
    const snapshot = {
      repositoryKey: "repository-1",
      generation: 3,
      authoritative: false,
      observedAt: "2026-08-09T00:00:00.000Z",
      scanStatus: {
        _tag: "degraded",
        reason: "git-failed",
        message: "git worktree list failed",
        failedAt: "2026-08-09T00:00:00.000Z",
        lastAuthoritativeAt: "2026-08-08T00:00:00.000Z",
      },
      worktrees: descriptors,
      adoptedWorkspaces: [],
    } as const;

    const decodedDescriptors = descriptors.map((descriptor) => decodeDescriptor(descriptor));
    const decodedSnapshot = decodeSnapshot(snapshot);

    expect(
      decodedDescriptors.map((descriptor) => decodeDescriptor(encodeDescriptor(descriptor))),
    ).toEqual(descriptors);
    expect(decodeSnapshot(encodeSnapshot(decodedSnapshot))).toEqual(snapshot);
  });

  it("round-trips active and archived server-supplied adopted workspace joins", () => {
    const joins = [
      {
        threadId: "thread-active",
        worktreeKey: "worktree-active",
        path: "/repo/active",
        branch: "feature/active",
        availability: "present",
        registrationState: "registered",
        locked: false,
      },
      {
        threadId: "thread-archived",
        worktreeKey: null,
        path: "/repo/archived",
        branch: null,
        availability: "missing-unregistered",
        registrationState: null,
        locked: true,
        lockReason: "retain for recovery",
      },
    ] as const;

    const decodedJoins = joins.map((join) => decodeAdoptedWorkspace(join));

    expect(
      decodedJoins.map((join) => decodeAdoptedWorkspace(encodeAdoptedWorkspace(join))),
    ).toEqual(joins);
  });

  it("rejects catalog and removal-plan arrays above their fixed bound", () => {
    expect(() =>
      decodeWorktreeDiscoveryPolicy({
        baselinePaths: Array.from({ length: 513 }, (_, index) => `/repo/worktree-${index}`),
      }),
    ).toThrow();
    expect(() =>
      decodeSnapshot({
        repositoryKey: "repository-1",
        generation: 1,
        authoritative: true,
        observedAt: "2026-08-09T00:00:00.000Z",
        scanStatus: { _tag: "ready" },
        worktrees: Array.from({ length: 513 }, () => baseDescriptor),
        adoptedWorkspaces: [],
      }),
    ).toThrow();
    expect(() =>
      decodeSnapshot({
        repositoryKey: "repository-1",
        generation: 1,
        authoritative: true,
        observedAt: "2026-08-09T00:00:00.000Z",
        scanStatus: { _tag: "ready" },
        worktrees: [],
        adoptedWorkspaces: Array.from({ length: 513 }, (_, index) => ({
          threadId: `thread-${index}`,
          worktreeKey: `worktree-${index}`,
          path: `/repo/worktree-${index}`,
          branch: null,
          availability: "present",
          registrationState: "registered",
          locked: false,
        })),
      }),
    ).toThrow();
    expect(() =>
      decodeRemovalPlan({
        planToken: "plan-1",
        generation: 1,
        availability: "present",
        registered: true,
        locked: false,
        trackedChangeCount: 0,
        untrackedFileCount: 0,
        pruneImpact: Array.from({ length: 513 }, () => ({
          path: "/repo/missing",
          pruneReason: "gitdir file points to non-existent location",
          locked: false,
        })),
      }),
    ).toThrow();
  });

  it("retains each exact prune reason in removal plans", () => {
    expect(
      decodeRemovalPlan({
        planToken: "plan-1",
        generation: 1,
        availability: "missing-registered",
        registered: true,
        locked: false,
        trackedChangeCount: 0,
        untrackedFileCount: 0,
        pruneImpact: [
          {
            path: "/repo/missing",
            pruneReason: "gitdir file points to non-existent location",
            locked: false,
          },
        ],
      }).pruneImpact,
    ).toEqual([
      {
        path: "/repo/missing",
        pruneReason: "gitdir file points to non-existent location",
        locked: false,
      },
    ]);
  });

  it("keeps nullable descriptor and availability fields distinct from omitted lock reasons", () => {
    const descriptor = decodeDescriptor({
      ...baseDescriptor,
      branch: null,
      head: null,
    });
    const adopted = decodeAdoptedWorkspace({
      threadId: "thread-1",
      worktreeKey: null,
      path: "/repo/missing",
      branch: null,
      availability: "missing-unregistered",
      registrationState: null,
      locked: false,
    });

    expect(descriptor.branch).toBeNull();
    expect(descriptor.head).toBeNull();
    expect(descriptor.lockReason).toBeUndefined();
    expect(adopted.worktreeKey).toBeNull();
    expect(adopted.branch).toBeNull();
    expect(adopted.registrationState).toBeNull();
    expect(adopted.lockReason).toBeUndefined();
  });

  it("round-trips every tagged error and removal result", () => {
    const catalogError = decodeCatalogError({
      _tag: "WorktreeCatalogError",
      reason: "repository-unavailable",
      message: "No repository.",
    });
    const adoptionError = decodeAdoptionError({
      _tag: "WorktreeAdoptionError",
      reason: "stale-generation",
      message: "Refresh before adopting.",
      currentGeneration: 2,
    });
    const removalError = decodeRemovalError({
      _tag: "WorktreeRemovalError",
      reason: "stale-plan",
      message: "Request a new plan.",
      currentGeneration: 3,
    });
    const workspaceUnavailableError = decodeWorkspaceUnavailableError({
      _tag: "WorkspaceUnavailableError",
      reason: "workspace-unavailable",
      message: "The checkout is missing.",
      threadId: "thread-1",
      path: "/repo/missing",
      availability: "missing-registered",
    });
    const operationError = decodeOperationError({
      _tag: "WorktreeOperationError",
      reason: "operation-capacity",
      message: "Retry after another worktree operation finishes.",
    });
    const workspaceIdentityError = decodeWorkspaceIdentityError({
      _tag: "WorkspaceIdentityError",
      reason: "workspace-identity-unavailable",
      message: "The physical workspace identity could not be verified.",
      path: "/repo/unverified",
    });
    const results = [
      {
        threadRemoved: true,
        gitOutcome: "not-requested",
        orphanCleanupPending: false,
      },
      {
        threadRemoved: true,
        gitOutcome: "failed",
        detail: "Git cleanup could not be verified.",
        orphanCleanupPending: true,
      },
    ] as const;

    expect(decodeCatalogError(encodeCatalogError(catalogError))._tag).toBe("WorktreeCatalogError");
    expect(decodeAdoptionError(encodeAdoptionError(adoptionError))._tag).toBe(
      "WorktreeAdoptionError",
    );
    expect(decodeRemovalError(encodeRemovalError(removalError))._tag).toBe("WorktreeRemovalError");
    expect(decodeOperationError(encodeOperationError(operationError))._tag).toBe(
      "WorktreeOperationError",
    );
    expect(
      decodeWorkspaceUnavailableError(encodeWorkspaceUnavailableError(workspaceUnavailableError))
        ._tag,
    ).toBe("WorkspaceUnavailableError");
    expect(
      decodeWorkspaceIdentityError(encodeWorkspaceIdentityError(workspaceIdentityError))._tag,
    ).toBe("WorkspaceIdentityError");
    expect(
      results.map((result) =>
        decodeRemovalResult(encodeRemovalResult(decodeRemovalResult(result))),
      ),
    ).toEqual(results);
  });

  it("decodes every removal outcome and actionable error reason", () => {
    expect(
      ["not-requested", "removed", "cleaned", "failed"].map((gitOutcome) =>
        decodeRemovalResult({
          threadRemoved: true,
          gitOutcome,
          orphanCleanupPending: false,
        }),
      ),
    ).toHaveLength(4);

    const reasons = [
      "command-conflict",
      "cleanup-capacity",
      "session-running",
      "ownership-conflict",
      "stale-plan",
      "dirty-confirmation-required",
      "protected-target",
      "locked",
      "replacement-conflict",
      "git-failed",
      "repository-mismatch",
    ] as const;
    expect(
      reasons.map((reason) =>
        decodeRemovalError({
          _tag: "WorktreeRemovalError",
          reason,
          message: "Removal was rejected.",
          currentGeneration: 12,
        }),
      ),
    ).toMatchObject(reasons.map((reason) => ({ reason, currentGeneration: 12 })));
  });
});
