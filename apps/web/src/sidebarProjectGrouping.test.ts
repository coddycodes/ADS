import { EnvironmentId, ProjectId } from "@coddy/contracts";
import type { EnvironmentProject } from "@coddy/client-runtime/state/shell";
import { describe, expect, it } from "vite-plus/test";

import { buildSidebarProjectSnapshots } from "./sidebarProjectGrouping";

function project(
  id: string,
  environmentId: EnvironmentId,
  title: string,
  workspaceRoot: string,
): EnvironmentProject {
  return {
    id: ProjectId.make(id),
    title,
    workspaceRoot,
    repositoryIdentity: {
      canonicalKey: "repository:shared",
      locator: {
        source: "git-remote",
        remoteName: "origin",
        remoteUrl: "https://example.com/repo.git",
      },
      rootPath: workspaceRoot,
      displayName: "Repo",
      name: "Repo",
    },
    defaultModelSelection: null,
    scripts: [],
    worktreeDiscovery: {
      visibility: "hidden",
      initialPromptDismissedAt: null,
      baselinePaths: [],
    },
    createdAt: "2026-08-09T00:00:00.000Z",
    updatedAt: "2026-08-09T00:00:00.000Z",
    environmentId,
  };
}

describe("buildSidebarProjectSnapshots", () => {
  it("keeps grouped project members as deterministic physical environment children", () => {
    const localEnvironmentId = EnvironmentId.make("env-local");
    const zuluEnvironmentId = EnvironmentId.make("env-zulu");
    const alphaEnvironmentId = EnvironmentId.make("env-alpha");
    const projects = [
      project("project-zulu", zuluEnvironmentId, "Repo", "/zulu/repo"),
      project("project-local", localEnvironmentId, "Repo", "/local/repo"),
      project("project-alpha", alphaEnvironmentId, "Repo", "/alpha/repo"),
    ];

    const snapshots = buildSidebarProjectSnapshots({
      projects,
      settings: {
        sidebarProjectGroupingMode: "repository",
        sidebarProjectGroupingOverrides: {},
      },
      primaryEnvironmentId: localEnvironmentId,
      resolveEnvironmentLabel: (environmentId) =>
        new Map([
          [localEnvironmentId, "Local"],
          [zuluEnvironmentId, "Zulu"],
          [alphaEnvironmentId, "Alpha"],
        ]).get(environmentId) ?? null,
    });

    expect(snapshots).toHaveLength(1);
    expect(
      snapshots[0]?.memberProjects.map((member) => ({
        environmentId: member.environmentId,
        environmentLabel: member.environmentLabel,
        projectId: member.id,
        workspaceRoot: member.workspaceRoot,
      })),
    ).toEqual([
      {
        environmentId: alphaEnvironmentId,
        environmentLabel: "Alpha",
        projectId: ProjectId.make("project-alpha"),
        workspaceRoot: "/alpha/repo",
      },
      {
        environmentId: localEnvironmentId,
        environmentLabel: "Local",
        projectId: ProjectId.make("project-local"),
        workspaceRoot: "/local/repo",
      },
      {
        environmentId: zuluEnvironmentId,
        environmentLabel: "Zulu",
        projectId: ProjectId.make("project-zulu"),
        workspaceRoot: "/zulu/repo",
      },
    ]);
  });
});
