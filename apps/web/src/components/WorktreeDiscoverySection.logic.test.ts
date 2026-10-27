import {
  EnvironmentId,
  ProjectId,
  WorktreeKey,
  type VcsWorktreeDescriptor,
} from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  buildWorktreeDiscoveryGroups,
  formatDiscoveredWorktreeCount,
  formatWorktreeAddAllSummary,
  getDiscoveryVisibilityMenuLabel,
  getWorktreeCandidateLabel,
} from "./WorktreeDiscoverySection.logic";

function candidate(
  path: string,
  branch: string | null,
  head: string | null,
): VcsWorktreeDescriptor {
  return {
    worktreeKey: WorktreeKey.make(`key:${path}`),
    path,
    branch,
    head,
    isPrimary: false,
    isBare: false,
    locked: false,
    registrationState: "registered",
    directoryState: "present",
    adoptionState: "none",
    eligibleForAdoption: true,
  };
}

describe("WorktreeDiscoverySection presentation logic", () => {
  it("groups physical projects by environment before display-only parent directories", () => {
    const groups = buildWorktreeDiscoveryGroups([
      {
        environmentId: EnvironmentId.make("env-zulu"),
        environmentLabel: "Zulu",
        projectId: ProjectId.make("project-zulu"),
        candidates: [
          candidate("C:\\worktrees\\repo\\beta", "feature/beta", "bbbbbbbb"),
          candidate("C:\\worktrees\\repo\\alpha", "feature/alpha", "aaaaaaaa"),
        ],
      },
      {
        environmentId: EnvironmentId.make("env-alpha"),
        environmentLabel: "Alpha",
        projectId: ProjectId.make("project-alpha"),
        candidates: [
          candidate("/srv/other/detached", null, "1234567890abcdef"),
          candidate("/srv/repo/zeta", "zeta", "cccccccc"),
          candidate("/srv/repo/alpha", "alpha", "dddddddd"),
        ],
      },
    ]);

    expect(groups.map((group) => [group.environmentLabel, group.projectId])).toEqual([
      ["Alpha", ProjectId.make("project-alpha")],
      ["Zulu", ProjectId.make("project-zulu")],
    ]);
    expect(
      groups[0]?.parentGroups.map((group) => [
        group.parentDirectory,
        group.candidates.map((item) => [item.label, item.candidate.path]),
      ]),
    ).toEqual([
      ["/srv/other", [["Detached 1234567", "/srv/other/detached"]]],
      [
        "/srv/repo",
        [
          ["alpha", "/srv/repo/alpha"],
          ["zeta", "/srv/repo/zeta"],
        ],
      ],
    ]);
    expect(groups[1]?.parentGroups).toEqual([
      {
        parentDirectory: "C:\\worktrees\\repo",
        candidates: [
          {
            candidate: expect.objectContaining({ path: "C:\\worktrees\\repo\\alpha" }),
            discriminator: null,
            label: "feature/alpha",
          },
          {
            candidate: expect.objectContaining({ path: "C:\\worktrees\\repo\\beta" }),
            discriminator: null,
            label: "feature/beta",
          },
        ],
      },
    ]);
  });

  it("uses a locale-independent exact tie-break for folded display labels", () => {
    const groups = buildWorktreeDiscoveryGroups([
      {
        environmentId: EnvironmentId.make("env-lower"),
        environmentLabel: "alpha",
        projectId: ProjectId.make("project-lower"),
        candidates: [candidate("/repo/lower", "feature", "1111111")],
      },
      {
        environmentId: EnvironmentId.make("env-upper"),
        environmentLabel: "Alpha",
        projectId: ProjectId.make("project-upper"),
        candidates: [
          candidate("/repo/upper-lower", "feature", "2222222"),
          candidate("/repo/upper-upper", "Feature", "3333333"),
        ],
      },
    ]);

    expect(groups.map((group) => group.environmentLabel)).toEqual(["Alpha", "alpha"]);
    expect(groups[0]?.parentGroups[0]?.candidates.map((candidate) => candidate.label)).toEqual([
      "Feature",
      "feature",
    ]);
  });

  it("adds final-component discriminators only for exact duplicate labels in one parent", () => {
    const groups = buildWorktreeDiscoveryGroups([
      {
        environmentId: EnvironmentId.make("environment-main"),
        environmentLabel: "This device",
        projectId: ProjectId.make("project-main"),
        candidates: [
          candidate(
            "/Users/admin/conductor/workspaces/pathfinder-review",
            "hotfix/PFS-1817",
            "1111111",
          ),
          candidate("/Users/admin/orca/workspaces/alpha", "alpha", "2222222"),
          candidate(
            "/Users/admin/conductor/workspaces/pathfinder-hotfix",
            "hotfix/PFS-1817",
            "3333333",
          ),
        ],
      },
    ]);

    expect(groups[0]?.parentGroups).toMatchObject([
      {
        parentDirectory: "/Users/admin/conductor/workspaces",
        candidates: [
          { label: "hotfix/PFS-1817", discriminator: "pathfinder-hotfix" },
          { label: "hotfix/PFS-1817", discriminator: "pathfinder-review" },
        ],
      },
      {
        parentDirectory: "/Users/admin/orca/workspaces",
        candidates: [{ label: "alpha", discriminator: null }],
      },
    ]);
  });

  it("derives a duplicate discriminator from Windows separators without changing the path", () => {
    const reviewPath = "C:\\Users\\admin\\workspaces\\pathfinder-review";
    const groups = buildWorktreeDiscoveryGroups([
      {
        environmentId: EnvironmentId.make("environment-main"),
        environmentLabel: "This device",
        projectId: ProjectId.make("project-main"),
        candidates: [
          candidate("C:\\Users\\admin\\workspaces\\pathfinder-hotfix", "hotfix/PFS-1817", null),
          candidate(reviewPath, "hotfix/PFS-1817", null),
        ],
      },
    ]);

    expect(groups[0]?.parentGroups[0]?.candidates).toMatchObject([
      { discriminator: "pathfinder-hotfix" },
      { candidate: { path: reviewPath }, discriminator: "pathfinder-review" },
    ]);
  });

  it("uses the branch or a seven-character detached HEAD label without changing the path", () => {
    const branchCandidate = candidate("/Repo/Feature", "feature/keep-case", "abcdef012345");
    const detachedCandidate = candidate("C:\\Repo\\Detached", null, "ABCDEF012345");

    expect(getWorktreeCandidateLabel(branchCandidate)).toBe("feature/keep-case");
    expect(getWorktreeCandidateLabel(detachedCandidate)).toBe("Detached ABCDEF0");
    expect(detachedCandidate.path).toBe("C:\\Repo\\Detached");
  });

  it("formats singular and plural discovery counts", () => {
    expect(formatDiscoveredWorktreeCount(1)).toBe("1 discovered worktree");
    expect(formatDiscoveredWorktreeCount(3)).toBe("3 discovered worktrees");
  });

  it("reports complete, partial, and failed add-all outcomes", () => {
    expect(formatWorktreeAddAllSummary({ successCount: 3, failureCount: 0 })).toEqual({
      type: "success",
      title: "Added 3 discovered worktrees",
      description: "All discovered worktrees were added to Coddy Studio.",
    });
    expect(formatWorktreeAddAllSummary({ successCount: 2, failureCount: 1 })).toEqual({
      type: "warning",
      title: "Added 2 of 3 discovered worktrees",
      description: "1 worktree could not be added.",
    });
    expect(formatWorktreeAddAllSummary({ successCount: 0, failureCount: 2 })).toEqual({
      type: "error",
      title: "Could not add 2 discovered worktrees",
      description: "No discovered worktrees were added.",
    });
  });

  it("uses the inverse discovery visibility action in project menus", () => {
    expect(getDiscoveryVisibilityMenuLabel("hidden")).toBe("Show Hidden Worktrees");
    expect(getDiscoveryVisibilityMenuLabel("hidden", 0)).toBe("Show Hidden Worktrees (0)");
    expect(getDiscoveryVisibilityMenuLabel("hidden", 2)).toBe("Show Hidden Worktrees (2)");
    expect(getDiscoveryVisibilityMenuLabel("shown", 2)).toBe("Hide Discovered Worktrees");
  });
});
