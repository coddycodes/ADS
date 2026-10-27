import type {
  GitManagerRefEntry,
  GitManagerRemoteTag,
  GitManagerRemoteTags,
} from "@coddy/contracts";

/** Section keys stored in the Git Manager view state as `collapsedTagSections`. */
export const LOCAL_TAG_SECTION = "local";

export function remoteTagSection(remote: string): string {
  return `remote:${remote}`;
}

export interface LocalTagRow {
  readonly name: string;
  readonly targetSha: string;
  readonly shortSha: string;
}

/** How a remote tag relates to the local checkout. */
export type RemoteTagPresence = "same" | "differs" | "not-fetched";

export interface RemoteTagRow {
  readonly name: string;
  readonly targetSha: string;
  readonly shortSha: string;
  readonly presence: RemoteTagPresence;
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

/** Newest-looking names first: version-like tags sort numerically, descending. */
function byNameDescending(
  left: { readonly name: string },
  right: { readonly name: string },
): number {
  return collator.compare(right.name, left.name);
}

function shortSha(sha: string): string {
  return sha.slice(0, 8);
}

export function buildLocalTagRows(
  tags: ReadonlyArray<Pick<GitManagerRefEntry, "name" | "tipSha">>,
): ReadonlyArray<LocalTagRow> {
  return tags
    .map((tag) => ({ name: tag.name, targetSha: tag.tipSha, shortSha: shortSha(tag.tipSha) }))
    .toSorted(byNameDescending);
}

export function buildRemoteTagRows(
  remoteTags: ReadonlyArray<GitManagerRemoteTag>,
  localTags: ReadonlyArray<Pick<GitManagerRefEntry, "name" | "tipSha">>,
): ReadonlyArray<RemoteTagRow> {
  const localByName = new Map(localTags.map((tag) => [tag.name, tag.tipSha]));
  return remoteTags
    .map((tag) => {
      const local = localByName.get(tag.name);
      const presence: RemoteTagPresence =
        local === undefined ? "not-fetched" : local === tag.targetSha ? "same" : "differs";
      return {
        name: tag.name,
        targetSha: tag.targetSha,
        shortSha: shortSha(tag.targetSha),
        presence,
      };
    })
    .toSorted(byNameDescending);
}

export function describeRemoteTagPresence(presence: RemoteTagPresence): string | null {
  switch (presence) {
    case "same":
      return null;
    case "differs":
      return "differs locally";
    case "not-fetched":
      return "not fetched";
  }
}

export interface RemoteTagsQuerySummary {
  readonly kind: "loading" | "error" | "unavailable" | "empty" | "ready";
  readonly message: string | null;
}

export function summarizeRemoteTagsQuery(input: {
  readonly remote: string;
  readonly result: GitManagerRemoteTags | null;
  readonly pending: boolean;
  readonly error: string | null;
}): RemoteTagsQuerySummary {
  if (input.result === null) {
    if (input.error !== null) return { kind: "error", message: input.error };
    return { kind: "loading", message: `Loading tags from ${input.remote}…` };
  }
  if (input.result.status === "unavailable") {
    return {
      kind: "unavailable",
      message: input.result.reason ?? `${input.remote} could not be reached.`,
    };
  }
  if (input.result.tags.length === 0) {
    return { kind: "empty", message: `No tags on ${input.remote}.` };
  }
  return { kind: "ready", message: input.pending ? "Refreshing…" : null };
}
