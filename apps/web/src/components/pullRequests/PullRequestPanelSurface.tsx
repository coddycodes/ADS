import { resolveChangeRequestPresentationForKind } from "@bibcode/shared/sourceControl";
import type { ScopedProjectRef, ScopedThreadRef } from "@bibcode/contracts";
import { useNavigate } from "@tanstack/react-router";
import { useMemo, useState } from "react";
import { usePrimarySettings } from "../../hooks/useSettings";
import { usePullRequestsStore } from "../../pullRequestsStore";
import { type PullRequestRightPanelSurface, useRightPanelStore } from "../../rightPanelStore";
import { useServerConfigs } from "../../state/entities";
import { useEnvironmentConnectionState } from "../../state/environments";
import { Button } from "../ui/button";
import { Skeleton } from "../ui/skeleton";
import { PullRequestsDetailView } from "./detail/PullRequestsDetailView";
import {
  PullRequestsFileAnchorContext,
  type PullRequestsNavigation,
  PullRequestsNavigationContext,
} from "./detail/pullRequestsNavigation";
import {
  PULL_REQUESTS_DISABLED_IN_SETTINGS_MESSAGE,
  resolvePullRequestsAvailability,
  resolvePullRequestsMutationsDisabledReason,
} from "./pullRequestsAvailability";
import { PullRequestsContextRefresh } from "./pullRequestsContextRefresh";
import { PullRequestsUnavailableState } from "./PullRequestsUnavailableState";
import { usePullRequestsContextQuery } from "./usePullRequestsContextQuery";

export interface PullRequestPanelSurfaceProps {
  threadRef: ScopedThreadRef;
  projectRef: ScopedProjectRef | null;
  gitCwd: string | null;
  workspaceUnavailable: string | null;
  surface: PullRequestRightPanelSurface;
}

/** A request's detail as a thread's right-panel tab, against the thread's checkout. */
export function PullRequestPanelSurface(props: PullRequestPanelSurfaceProps) {
  // A definite width: the detail view lays out by its container's inline size.
  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <PullRequestPanelSurfaceContent {...props} />
    </div>
  );
}

function PullRequestPanelSurfaceContent({
  threadRef,
  projectRef,
  gitCwd,
  workspaceUnavailable,
  surface,
}: PullRequestPanelSurfaceProps) {
  const navigate = useNavigate();
  const { environmentId } = threadRef;
  const projectId = projectRef?.projectId ?? null;
  // The thread view rebuilds its project ref on every thread update; the detail view's
  // effects key on it.
  const stableProjectRef = useMemo(
    () => (projectId === null ? null : { environmentId, projectId }),
    [environmentId, projectId],
  );
  const connection = useEnvironmentConnectionState(environmentId);
  const serverConfig = useServerConfigs().get(environmentId) ?? null;
  const enabled = usePrimarySettings((settings) => settings.pullRequestsEnabled);
  const availability = resolvePullRequestsAvailability(connection.data, serverConfig, enabled);
  // ChatView derives gitCwd from the loaded project, so a cwd means the project is known.
  const usable = availability.kind === "ready" && gitCwd !== null && workspaceUnavailable === null;
  const { query, storedContext, rescan, recover } = usePullRequestsContextQuery({
    environmentId,
    cwd: gitCwd,
    enabled: usable,
  });
  const [fileAnchor, setFileAnchor] = useState<string | null>(null);
  const { id, number, provider } = surface;
  const navigation = useMemo<PullRequestsNavigation>(() => {
    const store = () => useRightPanelStore.getState();
    return {
      selectTab: (tab) => store().setPullRequestTab(threadRef, number, tab),
      showFile: (path) => {
        setFileAnchor(path);
        store().setPullRequestTab(threadRef, number, "files");
      },
      setFileAnchor,
      openRequest: (target) => store().openPullRequest(threadRef, { number: target, provider }),
      requestGone: () => store().closeSurface(threadRef, id),
      discardDeleted: () => store().closeSurface(threadRef, id),
      back: null,
    };
  }, [id, number, provider, threadRef]);
  const scope = useMemo(() => ({ environmentId, cwd: gitCwd ?? "" }), [environmentId, gitCwd]);

  if (availability.kind !== "ready") {
    return (
      <PullRequestsUnavailableState
        reason={
          availability.kind === "disabled_in_settings"
            ? PULL_REQUESTS_DISABLED_IN_SETTINGS_MESSAGE
            : availability.kind === "unsupported"
              ? "This environment does not support Pull Requests."
              : availability.reason
        }
        disabledInSettings={availability.kind === "disabled_in_settings"}
      />
    );
  }
  if (workspaceUnavailable !== null) {
    return <PullRequestsUnavailableState reason={workspaceUnavailable} />;
  }
  if (stableProjectRef === null || gitCwd === null) {
    return <PullRequestsUnavailableState reason="Waiting for project data." />;
  }
  if ((query.error !== null && !storedContext) || query.data?.status === "unavailable") {
    const unavailable = query.data?.status === "unavailable" ? query.data : null;
    return (
      <PullRequestsUnavailableState
        reason={query.error ?? unavailable!.message}
        authCommand={unavailable?.authCommand ?? null}
        installHint={unavailable?.installHint ?? null}
        onRescan={rescan}
        isPending={query.isPending}
      />
    );
  }
  if (query.data === null) {
    return (
      <div role="status" className="space-y-3 p-4">
        <span className="sr-only">
          Loading {resolveChangeRequestPresentationForKind(provider).longName}…
        </span>
        <Skeleton className="h-8 w-60" />
        <Skeleton className="h-14 w-full" />
      </div>
    );
  }
  const openFullPage = () => {
    // The full page reads its checkout from the project's saved choice. Carry the thread's,
    // so the page shows the same request against the same checkout; its Worktree picker
    // shows the switch and changes it back in one step, like Git Manager's link.
    usePullRequestsStore.getState().setCheckoutCwd(stableProjectRef, gitCwd);
    void navigate({
      to: "/project/$environmentId/$projectId/pull-requests/$number",
      params: { ...stableProjectRef, number: String(number) },
      search: { tab: surface.tab },
    });
  };
  // An element, not a component: the detail view calls headerActions while rendering.
  const openFullPageButton = (
    <Button size="sm" variant="link" onClick={openFullPage}>
      Open full page
    </Button>
  );
  return (
    <PullRequestsContextRefresh value={recover}>
      <PullRequestsNavigationContext value={navigation}>
        <PullRequestsFileAnchorContext value={fileAnchor}>
          <PullRequestsDetailView
            // A different repository or account at the same checkout path remounts the
            // detail, as on the full page: its queries are keyed by checkout and number.
            key={JSON.stringify([
              gitCwd,
              number,
              query.data.provider,
              query.data.host,
              query.data.repository,
              query.data.account.login,
            ])}
            scope={scope}
            projectRef={stableProjectRef}
            context={query.data}
            number={number}
            tab={surface.tab}
            mutationsDisabledReason={resolvePullRequestsMutationsDisabledReason(serverConfig)}
            headerActions={() => openFullPageButton}
          />
        </PullRequestsFileAnchorContext>
      </PullRequestsNavigationContext>
    </PullRequestsContextRefresh>
  );
}
