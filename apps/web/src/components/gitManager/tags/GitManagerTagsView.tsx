import type { EnvironmentId, GitManagerRefEntry } from "@coddy/contracts";
import {
  ChevronRightIcon,
  CloudUploadIcon,
  RefreshCwIcon,
  TagIcon,
  Trash2Icon,
} from "lucide-react";
import { memo, useCallback, useMemo, type ReactNode } from "react";

import { Button } from "~/components/ui/button";
import { Collapsible, CollapsiblePanel, CollapsibleTrigger } from "~/components/ui/collapsible";
import { cn } from "~/lib/utils";

import { gitManagerEnvironment } from "../../../state/gitManager";
import { useEnvironmentQuery } from "../../../state/query";
import {
  LOCAL_TAG_SECTION,
  buildLocalTagRows,
  buildRemoteTagRows,
  describeRemoteTagPresence,
  remoteTagSection,
  summarizeRemoteTagsQuery,
} from "./GitManagerTagsView.logic";

export type GitManagerTagRowAction = "push" | "delete";

export interface GitManagerTagsViewProps {
  readonly scope: { readonly environmentId: EnvironmentId; readonly cwd: string };
  /** Local tags from the refs snapshot. */
  readonly tags: ReadonlyArray<GitManagerRefEntry>;
  readonly remotes: ReadonlyArray<string>;
  readonly tagDisabledReason: string | null;
  readonly collapsedSections: ReadonlyArray<string>;
  readonly onSectionCollapsedChange: (section: string, collapsed: boolean) => void;
  readonly onTagAction: (action: GitManagerTagRowAction, tag: string) => void;
}

const ROW_CLASS =
  "group/tag-row flex h-8 items-center gap-2 rounded-md px-2 text-xs hover:bg-accent/50";

function TagSection({
  section,
  title,
  count,
  collapsed,
  trailing,
  onCollapsedChange,
  children,
}: {
  readonly section: string;
  readonly title: string;
  readonly count: number | null;
  readonly collapsed: boolean;
  readonly trailing?: ReactNode;
  readonly onCollapsedChange: (section: string, collapsed: boolean) => void;
  readonly children: ReactNode;
}) {
  const handleOpenChange = useCallback(
    (open: boolean) => onCollapsedChange(section, !open),
    [onCollapsedChange, section],
  );
  return (
    <Collapsible open={!collapsed} onOpenChange={handleOpenChange}>
      <div className="flex items-center gap-1">
        <CollapsibleTrigger
          aria-label={`${collapsed ? "Expand" : "Collapse"} ${title}`}
          className="flex min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs font-semibold hover:bg-accent/50"
          data-section={section}
        >
          <ChevronRightIcon
            aria-hidden="true"
            className={cn("size-3.5 shrink-0 transition-transform", !collapsed && "rotate-90")}
          />
          <span className="truncate">{title}</span>
          {count === null ? null : (
            <span className="rounded bg-muted px-1.5 font-mono text-xs font-normal text-muted-foreground">
              {count}
            </span>
          )}
        </CollapsibleTrigger>
        {trailing}
      </div>
      <CollapsiblePanel>
        <div className="pb-2 pl-3">{children}</div>
      </CollapsiblePanel>
    </Collapsible>
  );
}

function LocalTagRows({
  tags,
  canPush,
  tagDisabledReason,
  onTagAction,
}: {
  readonly tags: ReadonlyArray<GitManagerRefEntry>;
  readonly canPush: boolean;
  readonly tagDisabledReason: string | null;
  readonly onTagAction: GitManagerTagsViewProps["onTagAction"];
}) {
  const rows = useMemo(() => buildLocalTagRows(tags), [tags]);
  if (rows.length === 0) {
    return (
      <p className="px-2 py-1.5 text-xs text-muted-foreground">
        No local tags. Create one from a commit in History.
      </p>
    );
  }
  return (
    <ul aria-label="Local tags" className="space-y-0.5">
      {rows.map((row) => (
        <li key={row.name} className={ROW_CLASS} data-testid={`git-manager-local-tag-${row.name}`}>
          <TagIcon aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" />
          <span className="min-w-0 flex-1 truncate font-mono">{row.name}</span>
          <span className="font-mono text-xs text-muted-foreground">{row.shortSha}</span>
          <span className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity focus-within:opacity-100 group-hover/tag-row:opacity-100">
            {canPush ? (
              <Button
                aria-label={`Push tag ${row.name}`}
                disabled={tagDisabledReason !== null}
                size="xs"
                title={tagDisabledReason ?? `Push ${row.name} to a remote`}
                variant="ghost"
                onClick={() => onTagAction("push", row.name)}
              >
                <CloudUploadIcon aria-hidden="true" />
              </Button>
            ) : null}
            <Button
              aria-label={`Delete tag ${row.name}`}
              disabled={tagDisabledReason !== null}
              size="xs"
              title={tagDisabledReason ?? `Delete local tag ${row.name}`}
              variant="ghost"
              onClick={() => onTagAction("delete", row.name)}
            >
              <Trash2Icon aria-hidden="true" />
            </Button>
          </span>
        </li>
      ))}
    </ul>
  );
}

function RemoteTagsSection({
  scope,
  remote,
  localTags,
  collapsed,
  onCollapsedChange,
}: {
  readonly scope: GitManagerTagsViewProps["scope"];
  readonly remote: string;
  readonly localTags: ReadonlyArray<GitManagerRefEntry>;
  readonly collapsed: boolean;
  readonly onCollapsedChange: GitManagerTagsViewProps["onSectionCollapsedChange"];
}) {
  const { environmentId, cwd } = scope;
  // Each remote is one bounded `ls-remote`; the query runs when the section
  // mounts (the tab is open) and again only on an explicit refresh.
  const queryAtom = useMemo(
    () => gitManagerEnvironment.getRemoteTags({ environmentId, input: { cwd, remote } }),
    [cwd, environmentId, remote],
  );
  const query = useEnvironmentQuery(queryAtom);
  const result = query.data ?? null;
  const summary = summarizeRemoteTagsQuery({
    remote,
    result,
    pending: query.isPending,
    error: query.error,
  });
  const rows = useMemo(
    () => (result === null ? [] : buildRemoteTagRows(result.tags, localTags)),
    [localTags, result],
  );
  const refresh = query.refresh;
  const retry = useCallback(() => refresh(), [refresh]);
  return (
    <TagSection
      collapsed={collapsed}
      count={result?.status === "available" ? result.tags.length : null}
      section={remoteTagSection(remote)}
      title={`Remote ${remote}`}
      trailing={
        <Button
          aria-label={`Refresh tags from ${remote}`}
          disabled={query.isPending}
          size="xs"
          title={`Refresh tags from ${remote}`}
          variant="ghost"
          onClick={retry}
        >
          <RefreshCwIcon aria-hidden="true" className={cn(query.isPending && "animate-spin")} />
        </Button>
      }
      onCollapsedChange={onCollapsedChange}
    >
      {summary.kind === "ready" ? (
        <ul aria-label={`Tags on ${remote}`} className="space-y-0.5">
          {rows.map((row) => {
            const presence = describeRemoteTagPresence(row.presence);
            return (
              <li
                key={row.name}
                className={ROW_CLASS}
                data-presence={row.presence}
                data-testid={`git-manager-remote-tag-${remote}-${row.name}`}
              >
                <TagIcon aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" />
                <span className="min-w-0 flex-1 truncate font-mono">{row.name}</span>
                {presence === null ? null : (
                  <span className="rounded bg-muted px-1.5 text-xs text-muted-foreground">
                    {presence}
                  </span>
                )}
                <span className="font-mono text-xs text-muted-foreground">{row.shortSha}</span>
              </li>
            );
          })}
        </ul>
      ) : (
        <p
          aria-live="polite"
          className={cn(
            "px-2 py-1.5 text-xs",
            summary.kind === "error" || summary.kind === "unavailable"
              ? "text-destructive"
              : "text-muted-foreground",
          )}
        >
          {summary.message}
          {summary.kind === "error" || summary.kind === "unavailable" ? (
            <Button className="ml-2" size="xs" variant="outline" onClick={retry}>
              Retry
            </Button>
          ) : null}
        </p>
      )}
      {summary.kind === "ready" && summary.message !== null ? (
        <p className="px-2 py-1 text-xs text-muted-foreground">{summary.message}</p>
      ) : null}
    </TagSection>
  );
}

/**
 * Tags tab: local tags first, then one collapsible section per remote listing
 * what that remote advertises, each marked against the local set.
 */
export const GitManagerTagsView = memo(function GitManagerTagsView({
  scope,
  tags,
  remotes,
  tagDisabledReason,
  collapsedSections,
  onSectionCollapsedChange,
  onTagAction,
}: GitManagerTagsViewProps) {
  const collapsed = useMemo(() => new Set(collapsedSections), [collapsedSections]);
  return (
    <section
      aria-label="Tags"
      className="flex min-h-0 flex-1 flex-col gap-2 overflow-auto"
      data-testid="git-manager-tags"
    >
      <TagSection
        collapsed={collapsed.has(LOCAL_TAG_SECTION)}
        count={tags.length}
        section={LOCAL_TAG_SECTION}
        title="Local"
        onCollapsedChange={onSectionCollapsedChange}
      >
        <LocalTagRows
          canPush={remotes.length > 0}
          tagDisabledReason={tagDisabledReason}
          tags={tags}
          onTagAction={onTagAction}
        />
      </TagSection>
      {remotes.length === 0 ? (
        <p className="px-2 text-xs text-muted-foreground">
          No remote configured, so there are no remote tags to show.
        </p>
      ) : (
        remotes.map((remote) => (
          <RemoteTagsSection
            key={remote}
            collapsed={collapsed.has(remoteTagSection(remote))}
            localTags={tags}
            remote={remote}
            scope={scope}
            onCollapsedChange={onSectionCollapsedChange}
          />
        ))
      )}
    </section>
  );
});
