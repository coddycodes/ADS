import type { PullRequestsVocabularyInput } from "@coddy/contracts";
import { useEffect, useMemo, useState } from "react";
import { pullRequestsEnvironment } from "../../../state/pullRequests";
import { usePullRequestsQuery } from "../shared/usePullRequestsQuery";
import type { PullRequestsScope } from "../usePullRequestsAction";

/** Reads the first page on mount; only a truncated vocabulary searches the host. */
export function usePullRequestsVocabulary(
  scope: PullRequestsScope,
  kind: PullRequestsVocabularyInput["kind"],
) {
  const [search, setSearch] = useState("");
  const [remoteSearch, setRemoteSearch] = useState<string | null>(null);
  const [wasTruncated, setWasTruncated] = useState(false);
  const query = usePullRequestsQuery(
    useMemo(
      () =>
        pullRequestsEnvironment.getVocabulary({
          environmentId: scope.environmentId,
          input: { cwd: scope.cwd, kind, query: remoteSearch },
        }),
      [scope.environmentId, scope.cwd, kind, remoteSearch],
    ),
  );
  // Keep the initial truncated decision when a narrower response fits in one page.
  if (query.data?.truncated && !wasTruncated) setWasTruncated(true);
  const needsSearch = wasTruncated || Boolean(query.data?.truncated);
  const desiredQuery = search.trim() || null;
  useEffect(() => {
    if (!needsSearch || desiredQuery === remoteSearch) return;
    const timer = setTimeout(() => setRemoteSearch(desiredQuery), 300);
    return () => clearTimeout(timer);
  }, [needsSearch, desiredQuery, remoteSearch]);
  const searching = query.isPending || (needsSearch && desiredQuery !== remoteSearch);
  const entries = useMemo(
    () =>
      query.data?.entries.filter((entry) =>
        `${entry.label} ${entry.description ?? ""}`
          .toLocaleLowerCase()
          .includes(search.toLocaleLowerCase()),
      ) ?? [],
    [query.data, search],
  );
  return { ...query, entries, search, onSearch: setSearch, searching };
}
