import { createWorktreeEnvironmentAtoms } from "@coddy/client-runtime/state/worktrees";
import type { ScopedProjectRef } from "@coddy/contracts";
import { useEffect } from "react";

import { connectionAtomRuntime } from "../connection/runtime";
import { useAtomCommand } from "./use-atom-command";

export const worktreeEnvironment = createWorktreeEnvironmentAtoms(connectionAtomRuntime);

/**
 * Refreshes only the physical projects whose catalog atoms are currently in use by the caller.
 * The environment command scheduler coalesces focus and visibility requests for the same project.
 */
export function useWorktreeCatalogFocusRefresh(
  subscribedProjects: ReadonlyArray<ScopedProjectRef>,
): void {
  const refresh = useAtomCommand(worktreeEnvironment.refresh, {
    reportFailure: false,
  });

  useEffect(() => {
    if (
      subscribedProjects.length === 0 ||
      typeof window === "undefined" ||
      typeof document === "undefined"
    ) {
      return;
    }
    const uniqueProjects = new Map(
      subscribedProjects.map((project) => [
        JSON.stringify([project.environmentId, project.projectId]),
        project,
      ]),
    );
    const refreshSubscribedProjects = () => {
      for (const project of uniqueProjects.values()) {
        void refresh({
          environmentId: project.environmentId,
          input: { projectId: project.projectId, reason: "focus" },
        });
      }
    };
    const handleVisibilityChange = () => {
      if (document.visibilityState === "visible") {
        refreshSubscribedProjects();
      }
    };

    window.addEventListener("focus", refreshSubscribedProjects);
    document.addEventListener("visibilitychange", handleVisibilityChange);

    return () => {
      window.removeEventListener("focus", refreshSubscribedProjects);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
    };
  }, [refresh, subscribedProjects]);
}
