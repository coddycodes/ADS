import type { EnvironmentId } from "@coddy/contracts";

import { useFileTreeEntries } from "./projectFilesQueryState";

interface ProjectFilesPreloaderProps {
  readonly environmentId: EnvironmentId;
  readonly cwd: string;
}

/**
 * Holds the thread's Files list and its change subscription while the thread is open, so opening
 * Files paints at once with no read of its own. Changes made meanwhile only mark the list stale;
 * the panel re-reads it when it opens.
 */
export function ProjectFilesPreloader({ environmentId, cwd }: ProjectFilesPreloaderProps): null {
  useFileTreeEntries(environmentId, cwd, false);
  return null;
}
