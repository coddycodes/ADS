import type { PullRequestsCommits as Commits } from "@coddy/contracts";
import { CopyIcon } from "lucide-react";
import { memo, useState } from "react";
import { writeTextToClipboard } from "../../../hooks/useCopyToClipboard";
import { formatRelativeTimeLabel } from "../../../timestampFormat";
import { Button } from "../../ui/button";
import { PullRequestsActor } from "../shared/PullRequestsActor";
import { PullRequestsExternalLink } from "../shared/PullRequestsMarkdown";
const CommitRow = memo(function CommitRow({ commit }: { commit: Commits["commits"][number] }) {
  const [copyStatus, setCopyStatus] = useState<string | null>(null);
  return (
    <li className="space-y-2 border-b border-border p-4 last:border-b-0">
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <PullRequestsExternalLink href={commit.url}>
          {commit.subject || commit.shortSha}
        </PullRequestsExternalLink>
        <code className="font-mono text-xs">{commit.shortSha}</code>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={`Copy commit ${commit.shortSha}`}
          title="Copy commit SHA"
          onClick={() => {
            void writeTextToClipboard(commit.sha, "commit SHA").then(
              () => setCopyStatus("SHA copied"),
              () => setCopyStatus("Copy failed. Open the commit and copy its SHA."),
            );
          }}
        >
          <CopyIcon aria-hidden="true" />
        </Button>
      </div>
      <div className="flex items-center gap-2 text-xs">
        <PullRequestsActor actor={commit.author} />
        <time
          dateTime={commit.authoredAt}
          title={commit.authoredAt}
          className="text-muted-foreground"
        >
          {formatRelativeTimeLabel(commit.authoredAt)}
        </time>
      </div>
      {copyStatus ? (
        <p role="status" className="text-xs text-muted-foreground">
          {copyStatus}
        </p>
      ) : null}
    </li>
  );
});
export const PullRequestsCommits = memo(function PullRequestsCommits({
  commits,
}: {
  commits: Commits;
}) {
  return (
    <div className="min-h-0 flex-1 overflow-auto" data-text-surface="background">
      {commits.commits.length === 0 ? (
        <p className="p-4 text-sm text-muted-foreground">No commits reported</p>
      ) : (
        <ul>
          {commits.commits.map((commit) => (
            <CommitRow key={commit.sha} commit={commit} />
          ))}
        </ul>
      )}
    </div>
  );
});
