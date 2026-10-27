import type { ProjectScript } from "@coddy/contracts";

interface ProjectScriptLocationInput {
  project: {
    cwd: string;
  };
  worktreePath?: string | null;
}

interface ProjectScriptRuntimeEnvInput extends ProjectScriptLocationInput {
  extraEnv?: Record<string, string>;
}

const nonEmptyWorktreePath = (input: ProjectScriptLocationInput): string | undefined =>
  input.worktreePath?.trim() ? input.worktreePath : undefined;

export function projectScriptCwd(input: ProjectScriptLocationInput): string {
  return nonEmptyWorktreePath(input) ?? input.project.cwd;
}

export function projectScriptRuntimeEnv(
  input: ProjectScriptRuntimeEnvInput,
): Record<string, string> {
  const env: Record<string, string> = {
    CODDY_STUDIO_PROJECT_ROOT: input.project.cwd,
  };
  const worktreePath = nonEmptyWorktreePath(input);
  if (worktreePath !== undefined) {
    env.CODDY_STUDIO_WORKTREE_PATH = worktreePath;
  }
  if (input.extraEnv) {
    return { ...env, ...input.extraEnv };
  }
  return env;
}

export function setupProjectScript(scripts: readonly ProjectScript[]): ProjectScript | null {
  return scripts.find((script) => script.runOnWorktreeCreate) ?? null;
}
