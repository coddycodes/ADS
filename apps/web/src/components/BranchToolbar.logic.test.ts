import { EnvironmentId } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";
import { resolveEnvironmentOptionLabel, resolveEffectiveEnvMode } from "./BranchToolbar.logic";

const localEnvironmentId = EnvironmentId.make("environment-local");
const remoteEnvironmentId = EnvironmentId.make("environment-remote");

describe("resolveEnvironmentOptionLabel", () => {
  it("prefers the primary environment's machine label", () => {
    expect(
      resolveEnvironmentOptionLabel({
        isPrimary: true,
        environmentId: localEnvironmentId,
        runtimeLabel: "Julius's Mac mini",
        savedLabel: "Local environment",
      }),
    ).toBe("Julius's Mac mini");
  });

  it("falls back to 'This device' for generic primary labels", () => {
    expect(
      resolveEnvironmentOptionLabel({
        isPrimary: true,
        environmentId: localEnvironmentId,
        runtimeLabel: "Local environment",
        savedLabel: "Local",
      }),
    ).toBe("This device");
  });

  it("keeps configured labels for non-primary environments", () => {
    expect(
      resolveEnvironmentOptionLabel({
        isPrimary: false,
        environmentId: remoteEnvironmentId,
        runtimeLabel: null,
        savedLabel: "Build box",
      }),
    ).toBe("Build box");
  });
});

describe("resolveEffectiveEnvMode", () => {
  it("treats draft threads already attached to a worktree as current-checkout mode", () => {
    expect(
      resolveEffectiveEnvMode({
        activeWorktreePath: "/repo/.coddy/studio/worktrees/feature-a",
        hasServerThread: false,
        draftThreadEnvMode: "worktree",
      }),
    ).toBe("local");
  });

  it("keeps explicit new-worktree mode for draft threads without a worktree path", () => {
    expect(
      resolveEffectiveEnvMode({
        activeWorktreePath: null,
        hasServerThread: false,
        draftThreadEnvMode: "worktree",
      }),
    ).toBe("worktree");
  });
});
