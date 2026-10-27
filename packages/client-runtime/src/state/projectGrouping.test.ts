import { EnvironmentId, ProjectId } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  deriveLogicalProjectKey,
  deriveLogicalProjectKeyFromRef,
  deriveLogicalProjectKeyFromSettings,
  derivePhysicalProjectKey,
  derivePhysicalProjectKeyFromPath,
  deriveProjectGroupLabel,
  deriveProjectGroupingOverrideKey,
  getProjectOrderKey,
  resolveProjectGroupingMode,
  selectProjectGroupingSettings,
} from "./projectGrouping.ts";

const environmentId = EnvironmentId.make("environment-1");
const projectId = ProjectId.make("project-1");

function project(overrides: Record<string, unknown> = {}) {
  return {
    environmentId,
    id: projectId,
    title: "Workspace title",
    workspaceRoot: "/repo/packages/web",
    repositoryIdentity: {
      canonicalKey: "github:owner/repo",
      rootPath: "/repo",
      displayName: "Owner / Repo",
      name: "repo",
    },
    ...overrides,
  } as never;
}

describe("project grouping", () => {
  it("selects grouping settings and resolves physical-key overrides", () => {
    const settings = selectProjectGroupingSettings({
      sidebarProjectGroupingMode: "repository",
      sidebarProjectGroupingOverrides: {
        "environment-1:/repo/packages/web": "separate",
      },
    } as never);

    expect(settings).toEqual({
      sidebarProjectGroupingMode: "repository",
      sidebarProjectGroupingOverrides: {
        "environment-1:/repo/packages/web": "separate",
      },
    });
    expect(resolveProjectGroupingMode(project(), settings)).toBe("separate");
    expect(
      resolveProjectGroupingMode(project({ workspaceRoot: "/repo/packages/api" }), settings),
    ).toBe("repository");
  });

  it("normalizes physical keys for ordering, grouping, and direct paths", () => {
    const windowsProject = project({ workspaceRoot: "C:/Repo/WEB/" });
    expect(derivePhysicalProjectKey(windowsProject)).toBe("environment-1:c:\\repo\\web");
    expect(deriveProjectGroupingOverrideKey(windowsProject)).toBe("environment-1:c:\\repo\\web");
    expect(getProjectOrderKey(windowsProject)).toBe("environment-1:c:\\repo\\web");
    expect(derivePhysicalProjectKeyFromPath("environment-2", "/repo/web/")).toBe(
      "environment-2:/repo/web",
    );
  });

  it("groups repositories together or separates them by repository-relative path", () => {
    expect(deriveLogicalProjectKey(project())).toBe("github:owner/repo");
    expect(deriveLogicalProjectKey(project(), { groupingMode: "repository" })).toBe(
      "github:owner/repo",
    );
    expect(deriveLogicalProjectKey(project(), { groupingMode: "repository_path" })).toBe(
      "github:owner/repo::packages/web",
    );
    expect(
      deriveLogicalProjectKey(
        project({
          workspaceRoot: "C:\\Repo\\packages\\web",
          repositoryIdentity: {
            canonicalKey: "github:owner/repo",
            rootPath: "C:\\Repo",
          },
        }),
        { groupingMode: "repository_path" },
      ),
    ).toBe("github:owner/repo::packages/web");
    expect(
      deriveLogicalProjectKey(project({ workspaceRoot: "/repo" }), {
        groupingMode: "repository_path",
      }),
    ).toBe("github:owner/repo");
    expect(deriveLogicalProjectKey(project(), { groupingMode: "separate" })).toBe(
      "environment-1:/repo/packages/web",
    );
  });

  it("falls back to canonical or physical keys when repository paths are unusable", () => {
    expect(
      deriveLogicalProjectKey(
        project({ repositoryIdentity: { canonicalKey: "github:owner/repo" } }),
        { groupingMode: "repository_path" },
      ),
    ).toBe("github:owner/repo");
    expect(
      deriveLogicalProjectKey(
        project({ repositoryIdentity: { canonicalKey: "github:owner/repo", rootPath: " " } }),
        { groupingMode: "repository_path" },
      ),
    ).toBe("github:owner/repo");
    expect(
      deriveLogicalProjectKey(
        project({
          workspaceRoot: "/elsewhere/web",
          repositoryIdentity: { canonicalKey: "github:owner/repo", rootPath: "/repo" },
        }),
        { groupingMode: "repository_path" },
      ),
    ).toBe("github:owner/repo");
    expect(deriveLogicalProjectKey(project({ repositoryIdentity: null }))).toBe(
      "environment-1:/repo/packages/web",
    );
  });

  it("derives grouping from settings and falls back to scoped refs without a project", () => {
    const settings = {
      sidebarProjectGroupingMode: "repository_path" as const,
      sidebarProjectGroupingOverrides: {},
    };
    expect(deriveLogicalProjectKeyFromSettings(project(), settings)).toBe(
      "github:owner/repo::packages/web",
    );
    expect(
      deriveLogicalProjectKeyFromRef({ environmentId, projectId }, project(), {
        groupingMode: "repository",
      }),
    ).toBe("github:owner/repo");
    expect(deriveLogicalProjectKeyFromRef({ environmentId, projectId }, null)).toBe(
      "environment-1:project-1",
    );
    expect(deriveLogicalProjectKeyFromRef({ environmentId, projectId }, undefined)).toBe(
      "environment-1:project-1",
    );
  });

  it("prefers one unique display name, then repository name, then title", () => {
    expect(
      deriveProjectGroupLabel({
        representative: project(),
        members: [
          project(),
          project({ repositoryIdentity: { displayName: " Owner / Repo ", name: "repo" } }),
          project({ repositoryIdentity: { displayName: "", name: "repo" } }),
          project({ repositoryIdentity: { displayName: null, name: "repo" } }),
        ],
      }),
    ).toBe("Owner / Repo");

    expect(
      deriveProjectGroupLabel({
        representative: project(),
        members: [
          project({ repositoryIdentity: { displayName: "First", name: "repo" } }),
          project({ repositoryIdentity: { displayName: "Second", name: "repo" } }),
        ],
      }),
    ).toBe("repo");

    expect(
      deriveProjectGroupLabel({
        representative: project(),
        members: [
          project({ repositoryIdentity: { displayName: "First", name: "one" } }),
          project({ repositoryIdentity: { displayName: "Second", name: "two" } }),
        ],
      }),
    ).toBe("Workspace title");
  });
});
