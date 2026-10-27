import type { GitManagerCoAuthor } from "@coddy/contracts";

const IDEAL_SUMMARY_LENGTH = 50;

interface CommitEnabledInput {
  readonly summary: string;
  readonly includedCount: number;
  readonly allowEmpty: boolean;
  readonly isAmending: boolean;
  readonly isBusy: boolean;
}

export function isCommitEnabled({
  summary,
  includedCount,
  allowEmpty,
  isAmending,
  isBusy,
}: CommitEnabledInput): boolean {
  if (isBusy) return false;
  const hasCommitContent = includedCount > 0 || allowEmpty || isAmending;
  const hasSummary = summary.trim().length > 0 || includedCount === 1 || allowEmpty;
  return hasCommitContent && hasSummary;
}

export function buildPlaceholderSummary(paths: ReadonlyArray<string>): string {
  if (paths.length !== 1) return "";
  const path = paths[0]!;
  const separator = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return `Update ${path.slice(separator + 1)}`;
}

export function formatCoAuthorTrailers(coAuthors: ReadonlyArray<GitManagerCoAuthor>): string {
  const seenEmails = new Set<string>();
  const trailers: string[] = [];
  for (const coAuthor of coAuthors) {
    const emailKey = coAuthor.email.toLowerCase();
    if (seenEmails.has(emailKey)) continue;
    seenEmails.add(emailKey);
    trailers.push(`Co-Authored-By: ${coAuthor.name} <${coAuthor.email}>`);
  }
  return trailers.join("\n");
}

interface BuildCommitMessageInput {
  readonly summary: string;
  readonly description: string;
  readonly coAuthors: ReadonlyArray<GitManagerCoAuthor>;
}

export function buildCommitMessage({
  summary,
  description,
  coAuthors,
}: BuildCommitMessageInput): string {
  const trailers = formatCoAuthorTrailers(coAuthors);
  return [summary, description, trailers].filter((part) => part.length > 0).join("\n\n");
}

export function isSummaryOverIdealLength(summary: string): boolean {
  return summary.length > IDEAL_SUMMARY_LENGTH;
}
