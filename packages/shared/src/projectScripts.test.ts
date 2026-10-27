import { describe, expect, it } from "vite-plus/test";
import { projectScriptCwd, projectScriptRuntimeEnv, setupProjectScript } from "./projectScripts.ts";

const scripts = [
  {
    id: "setup",
    name: "Setup",
    command: "vp install",
    icon: "configure" as const,
    runOnWorktreeCreate: true,
  },
  {
    id: "test",
    name: "Test",
    command: "vp test",
    icon: "test" as const,
    runOnWorktreeCreate: false,
  },
];

describe("project script runtime helpers", () => {
  it("uses the worktree when present and the project root otherwise", () => {
    expect(projectScriptCwd({ project: { cwd: "/repo" }, worktreePath: "/worktree" })).toBe(
      "/worktree",
    );
    expect(projectScriptCwd({ project: { cwd: "/repo" }, worktreePath: null })).toBe("/repo");
    expect(projectScriptCwd({ project: { cwd: "/repo" } })).toBe("/repo");
  });

  it("treats empty and whitespace-only worktree paths as absent for cwd and environment", () => {
    for (const worktreePath of ["", " ", "\t\r\n"]) {
      const input = { project: { cwd: "/repo" }, worktreePath };

      expect(projectScriptCwd(input), JSON.stringify(worktreePath)).toBe("/repo");
      expect(projectScriptRuntimeEnv(input), JSON.stringify(worktreePath)).toEqual({
        CODDY_STUDIO_PROJECT_ROOT: "/repo",
      });
    }
  });

  it("preserves a non-empty worktree path exactly", () => {
    const worktreePath = " /worktree ";

    expect(projectScriptCwd({ project: { cwd: "/repo" }, worktreePath })).toBe(worktreePath);
    expect(projectScriptRuntimeEnv({ project: { cwd: "/repo" }, worktreePath })).toEqual({
      CODDY_STUDIO_PROJECT_ROOT: "/repo",
      CODDY_STUDIO_WORKTREE_PATH: worktreePath,
    });
  });

  it("builds the base environment without inventing a worktree path", () => {
    expect(projectScriptRuntimeEnv({ project: { cwd: "/repo" } })).toEqual({
      CODDY_STUDIO_PROJECT_ROOT: "/repo",
    });
    expect(projectScriptRuntimeEnv({ project: { cwd: "/repo" }, worktreePath: "" })).toEqual({
      CODDY_STUDIO_PROJECT_ROOT: "/repo",
    });
  });

  it("adds a worktree path and lets explicit environment values override defaults", () => {
    expect(
      projectScriptRuntimeEnv({
        project: { cwd: "/repo" },
        worktreePath: "/worktree",
        extraEnv: {
          CODDY_STUDIO_PROJECT_ROOT: "/custom",
          CODDY_STUDIO_WORKTREE_PATH: "/custom-worktree",
          CUSTOM_FLAG: "1",
        },
      }),
    ).toEqual({
      CODDY_STUDIO_PROJECT_ROOT: "/custom",
      CODDY_STUDIO_WORKTREE_PATH: "/custom-worktree",
      CUSTOM_FLAG: "1",
    });
  });

  it("returns the first setup script or null", () => {
    expect(setupProjectScript(scripts)).toBe(scripts[0]);
    expect(setupProjectScript(scripts.slice(1))).toBeNull();
    expect(setupProjectScript([])).toBeNull();
  });
});
