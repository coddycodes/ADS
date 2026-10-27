import { EnvironmentId, ProjectId } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import { derivePhysicalProjectKey, projectDraftFallbackKeys } from "./logicalProject";

const canonicalKey = "github.com/a/b";
const project = {
  id: ProjectId.make("project-1"),
  environmentId: EnvironmentId.make("env-a"),
  workspaceRoot: "/work/repo/packages/web",
};
const withIdentity = {
  ...project,
  repositoryIdentity: {
    canonicalKey,
    rootPath: "/work/repo",
    locator: {
      source: "git-remote" as const,
      remoteName: "origin",
      remoteUrl: "https://github.com/a/b.git",
    },
  },
};
const physicalKey = derivePhysicalProjectKey(project);
const repositoryPathKey = `${canonicalKey}::packages/web`;

describe("projectDraftFallbackKeys", () => {
  it("returns only the physical key for a project without repository identity", () => {
    expect(projectDraftFallbackKeys(project, "some-group")).toEqual([physicalKey]);
    expect(projectDraftFallbackKeys({ ...project, repositoryIdentity: null }, "g")).toEqual([
      physicalKey,
    ]);
  });

  it("covers every grouping mode's key and drops the one in use", () => {
    expect(projectDraftFallbackKeys(withIdentity, "other")).toEqual([
      physicalKey,
      canonicalKey,
      repositoryPathKey,
    ]);
    expect(projectDraftFallbackKeys(withIdentity, canonicalKey)).toEqual([
      physicalKey,
      repositoryPathKey,
    ]);
    expect(projectDraftFallbackKeys(withIdentity, physicalKey)).toEqual([
      canonicalKey,
      repositoryPathKey,
    ]);
  });
});
