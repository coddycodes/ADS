import type { ScopedProjectRef } from "@bibcode/contracts";
import { useLocation, useNavigate } from "@tanstack/react-router";
import { createContext, type ReactNode, useContext, useMemo } from "react";

import type { PullRequestsDetailTab } from "../../../pullRequestsStore";

/** Where a request's detail view lives: the full page (router) or a thread's right panel. */
export interface PullRequestsNavigation {
  /** The file the Changes tab scrolls to, or null. */
  readonly fileAnchor: string | null;
  /** The user picked another detail tab. */
  selectTab(tab: PullRequestsDetailTab): void;
  /** A conversation link to a file: the Changes tab, scrolled to that file. */
  showFile(path: string): void;
  /** The file tree selected a file: replace the anchor in place. */
  setFileAnchor(path: string): void;
  /** An action created another request (a revert request). */
  openRequest(number: number): void;
  /** The request was deleted while its view was shown. */
  requestGone(): void;
  /**
   * The request was deleted, whether or not its view is still shown: a panel closes the
   * deleted request's tab even after the user switched tabs. Null on the full page, which
   * never navigates once the user has left.
   */
  readonly discardDeleted: (() => void) | null;
  /** "Back to …", or null where there is no list to go back to. */
  readonly back: (() => void) | null;
}

export const PullRequestsNavigationContext = createContext<PullRequestsNavigation | null>(null);

export function usePullRequestsNavigation(): PullRequestsNavigation {
  const navigation = useContext(PullRequestsNavigationContext);
  if (navigation === null) {
    throw new Error("Pull request views need a PullRequestsNavigationContext provider.");
  }
  return navigation;
}

export function pathFromHash(hash: string): string | null {
  const value = hash.replace(/^#/, "");
  if (!value.startsWith("file=")) return null;
  try {
    return decodeURIComponent(value.slice(5));
  } catch {
    return null;
  }
}

const DETAIL_ROUTE = "/project/$environmentId/$projectId/pull-requests/$number";
const LIST_ROUTE = "/project/$environmentId/$projectId/pull-requests";

/** The full page: every navigation is the URL it was before the panel existed. */
export function PullRequestsRouteNavigation({
  projectRef,
  number,
  children,
}: {
  projectRef: ScopedProjectRef;
  number: number;
  children: ReactNode;
}) {
  const navigate = useNavigate();
  const { hash } = useLocation();
  const fileAnchor = pathFromHash(hash);
  const navigation = useMemo<PullRequestsNavigation>(() => {
    const params = { ...projectRef, number: String(number) };
    const fileHash = (path: string) => `file=${encodeURIComponent(path)}`;
    const toList = () => void navigate({ to: LIST_ROUTE, params: projectRef });
    return {
      fileAnchor,
      selectTab: (tab) =>
        void navigate({ to: DETAIL_ROUTE, params, search: { tab }, hash: "", replace: true }),
      showFile: (path) =>
        void navigate({ to: DETAIL_ROUTE, params, search: { tab: "files" }, hash: fileHash(path) }),
      setFileAnchor: (path) =>
        void navigate({
          to: DETAIL_ROUTE,
          params,
          search: { tab: "files" },
          hash: fileHash(path),
          replace: true,
        }),
      openRequest: (target) =>
        void navigate({
          to: DETAIL_ROUTE,
          params: { ...projectRef, number: String(target) },
          search: { tab: "conversation" },
          hash: "",
        }),
      requestGone: toList,
      discardDeleted: null,
      back: toList,
    };
  }, [fileAnchor, navigate, number, projectRef]);
  return (
    <PullRequestsNavigationContext value={navigation}>{children}</PullRequestsNavigationContext>
  );
}
