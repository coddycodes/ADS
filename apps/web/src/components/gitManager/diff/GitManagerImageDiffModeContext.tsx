import { projectKey } from "@coddy/client-runtime/state/entities";
import type { ScopedProjectRef } from "@coddy/contracts";
import { createContext, memo, type ReactNode, useCallback, useContext, useMemo } from "react";

import {
  DEFAULT_GIT_MANAGER_VIEW_STATE,
  type GitManagerImageDiffMode,
  useGitManagerStore,
} from "../../../gitManagerStore";
import { GitManagerImageDiff } from "./GitManagerImageDiff";

interface GitManagerImageDiffModeValue {
  readonly mode: GitManagerImageDiffMode;
  readonly onModeChange: (mode: GitManagerImageDiffMode) => void;
}

const noop = () => undefined;
const DEFAULT_IMAGE_DIFF_MODE_VALUE: GitManagerImageDiffModeValue = Object.freeze({
  mode: DEFAULT_GIT_MANAGER_VIEW_STATE.imageDiffMode,
  onModeChange: noop,
});
const GitManagerImageDiffModeContext = createContext<GitManagerImageDiffModeValue>(
  DEFAULT_IMAGE_DIFF_MODE_VALUE,
);

interface GitManagerImageDiffModeProviderProps {
  readonly projectRef: ScopedProjectRef;
  readonly children: ReactNode;
}

export const GitManagerImageDiffModeProvider = memo(function GitManagerImageDiffModeProvider({
  projectRef,
  children,
}: GitManagerImageDiffModeProviderProps) {
  const { environmentId, projectId } = projectRef;
  const storeKey = projectKey({ environmentId, projectId } as ScopedProjectRef);
  const selectMode = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      state.byProjectKey[storeKey]?.imageDiffMode ?? DEFAULT_GIT_MANAGER_VIEW_STATE.imageDiffMode,
    [storeKey],
  );
  const mode = useGitManagerStore(selectMode);
  const setImageDiffMode = useGitManagerStore((state) => state.setImageDiffMode);
  const onModeChange = useCallback(
    (nextMode: GitManagerImageDiffMode) =>
      setImageDiffMode({ environmentId, projectId } as ScopedProjectRef, nextMode),
    [environmentId, projectId, setImageDiffMode],
  );
  const value = useMemo(() => ({ mode, onModeChange }), [mode, onModeChange]);

  return (
    <GitManagerImageDiffModeContext.Provider value={value}>
      {children}
    </GitManagerImageDiffModeContext.Provider>
  );
});

export function useGitManagerImageDiffMode(): GitManagerImageDiffModeValue {
  return useContext(GitManagerImageDiffModeContext);
}

interface GitManagerStoredImageDiffProps {
  readonly before: string | null;
  readonly after: string | null;
}

export const GitManagerStoredImageDiff = memo(function GitManagerStoredImageDiff({
  before,
  after,
}: GitManagerStoredImageDiffProps) {
  const { mode, onModeChange } = useGitManagerImageDiffMode();
  return (
    <GitManagerImageDiff after={after} before={before} mode={mode} onModeChange={onModeChange} />
  );
});
