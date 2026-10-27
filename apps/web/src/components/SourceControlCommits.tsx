import type { ScopedThreadRef, VcsCommit } from "@coddy/contracts";
import { ChevronDownIcon } from "lucide-react";
import { useEffect, useEffectEvent, useState } from "react";

import { cn } from "~/lib/utils";
import { useEnvironmentQuery } from "~/state/query";
import { vcsEnvironment } from "~/state/vcs";

import { formatCommitTimestamp } from "./SourceControlCommits.logic";

export function SourceControlCommitRow({ commit, nowMs }: { commit: VcsCommit; nowMs: number }) {
  return (
    <div className="flex items-center gap-2 px-2 py-1 text-xs">
      <span className="shrink-0 font-mono text-xs text-muted-foreground">{commit.shortSha}</span>
      <span className="min-w-0 flex-1 truncate">{commit.subject}</span>
      <span className="shrink-0 text-xs text-muted-foreground">{commit.authorName}</span>
      <span className="shrink-0 text-xs text-muted-foreground">
        {formatCommitTimestamp(commit.authoredAtMs, nowMs)}
      </span>
    </div>
  );
}

const COMMITS_PAGE_SIZE = 30;

export function SourceControlCommits({
  threadRef,
  gitCwd,
  nowMs,
  reloadToken,
}: {
  threadRef: ScopedThreadRef;
  gitCwd: string | null;
  nowMs: number;
  reloadToken?: number;
}) {
  const [expanded, setExpanded] = useState(false);
  // Pagination: `cursor` drives the currently-fetched page; pages already
  // fetched are accumulated in `previousPages` so "Load more" appends.
  const [cursor, setCursor] = useState<number | null>(null);
  const [previousPages, setPreviousPages] = useState<readonly VcsCommit[]>([]);
  // Deferred first load: only subscribe once expanded.
  const query = useEnvironmentQuery(
    expanded && gitCwd !== null
      ? vcsEnvironment.listCommits({
          environmentId: threadRef.environmentId,
          input: { cwd: gitCwd, limit: COMMITS_PAGE_SIZE, ...(cursor !== null ? { cursor } : {}) },
        })
      : null,
  );

  // Refetch from the first page whenever a commit is made in the panel
  // (reloadToken bumps) or the section is (re)expanded.
  const resetPages = () => {
    setPreviousPages([]);
    setCursor(null);
  };
  const [seenReloadToken, setSeenReloadToken] = useState(reloadToken);
  if (seenReloadToken !== reloadToken) {
    setSeenReloadToken(reloadToken);
    if (expanded) resetPages();
  }
  const toggleExpanded = () => {
    if (!expanded) resetPages();
    setExpanded((value) => !value);
  };
  const revalidateCommits = useEffectEvent(() => query.revalidate());
  const reloadKey = expanded ? reloadToken : null;
  useEffect(() => {
    if (reloadKey !== null) revalidateCommits();
  }, [reloadKey]);

  const currentPage = query.data?.commits ?? [];
  const commits = [...previousPages, ...currentPage];
  const nextCursor = query.data?.nextCursor ?? null;

  const loadMore = () => {
    if (query.data === null || query.data.nextCursor === null) return;
    setPreviousPages((prev) => [...prev, ...query.data!.commits]);
    setCursor(query.data.nextCursor);
  };

  return (
    <div className="border-t border-border/60">
      <button
        type="button"
        onClick={toggleExpanded}
        className="flex w-full items-center gap-1 px-2 py-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground"
      >
        <ChevronDownIcon
          className={cn("size-3.5 transition-transform", !expanded && "-rotate-90")}
        />
        Commits
      </button>
      {expanded ? (
        query.error !== null && commits.length === 0 ? (
          <p className="px-2 py-3 text-xs text-destructive">Couldn't load commits</p>
        ) : query.isPending && commits.length === 0 ? (
          <p className="px-2 py-3 text-xs text-muted-foreground">Loading commits…</p>
        ) : commits.length === 0 ? (
          <p className="px-2 py-3 text-xs text-muted-foreground">No commits yet</p>
        ) : (
          <div className="max-h-48 overflow-auto pb-1">
            {commits.map((commit) => (
              <SourceControlCommitRow key={commit.sha} commit={commit} nowMs={nowMs} />
            ))}
            {nextCursor !== null ? (
              <button
                type="button"
                onClick={loadMore}
                className="w-full px-2 py-1.5 text-left text-xs font-medium text-muted-foreground hover:text-foreground"
              >
                Load more
              </button>
            ) : null}
          </div>
        )
      ) : null}
    </div>
  );
}
