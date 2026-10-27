import {
  insertRankedSearchResult,
  normalizeSearchQuery,
  scoreQueryMatch,
} from "@coddy/shared/searchRanking";

export interface ComposerCommandSearchCandidate<T> {
  readonly item: T;
  readonly name: string;
  readonly description: string;
  readonly tieBreaker: string;
}

function scoreComposerCommandCandidate(
  candidate: ComposerCommandSearchCandidate<unknown>,
  query: string,
): number | null {
  const primaryValue = candidate.name.trim().toLowerCase();
  const description = candidate.description.trim().toLowerCase();
  const scores = [
    scoreQueryMatch({
      value: primaryValue,
      query,
      exactBase: 0,
      prefixBase: 2,
      boundaryBase: 4,
      includesBase: 6,
      fuzzyBase: 100,
      boundaryMarkers: ["-", "_", "/"],
    }),
    scoreQueryMatch({
      value: description,
      query,
      exactBase: 20,
      prefixBase: 22,
      boundaryBase: 24,
      includesBase: 26,
    }),
  ].filter((score): score is number => score !== null);

  return scores.length > 0 ? Math.min(...scores) : null;
}

export function searchComposerCommandCandidates<T>(
  candidates: ReadonlyArray<ComposerCommandSearchCandidate<T>>,
  query: string,
  options?: {
    readonly trimLeadingPattern?: RegExp;
  },
): T[] {
  const normalizedQuery = normalizeSearchQuery(query, options);
  const ranked: Array<{
    item: T;
    score: number;
    tieBreaker: string;
  }> = [];

  for (const candidate of candidates) {
    const score = normalizedQuery ? scoreComposerCommandCandidate(candidate, normalizedQuery) : 0;
    if (score === null) {
      continue;
    }

    insertRankedSearchResult(
      ranked,
      {
        item: candidate.item,
        score,
        tieBreaker: `${candidate.name.trim().toLowerCase()}\u0000${candidate.tieBreaker}`,
      },
      Number.POSITIVE_INFINITY,
    );
  }

  return ranked.map((entry) => entry.item);
}
