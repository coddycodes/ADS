import type {
  PullRequestsChecks,
  PullRequestsDetail,
  PullRequestsFile,
  PullRequestsHostCapabilities,
  PullRequestsMergeReadiness,
  PullRequestsProviderKind,
  PullRequestsTimelineItem,
} from "@coddy/contracts";
type Vocabulary = PullRequestsHostCapabilities["vocabulary"];
export function headerSentence(
  detail: PullRequestsDetail,
  provider: PullRequestsProviderKind,
): string {
  return provider === "gitlab"
    ? `${detail.author.login} requested to merge ${detail.headBranch} into ${detail.baseBranch}`
    : `${detail.author.login} wants to merge ${detail.commitCount} commit${detail.commitCount === 1 ? "" : "s"} into ${detail.baseBranch} from ${detail.headBranch}`;
}
const READINESS_TONES = {
  mergeable: "success",
  checks_pending: "warning",
  checks_failing: "danger",
  review_required: "warning",
  changes_requested: "danger",
  conflicts: "danger",
  behind: "warning",
  blocked: "warning",
  draft: "neutral",
  merged: "success",
  closed: "neutral",
  unknown: "neutral",
} as const;
/** GitLab's approval line beside the overview Approve control. */
export function approvalStatusNote(
  required: { readonly approved: number; readonly required: number } | null,
): string {
  if (required === null || required.required <= 0) return "Approval is optional";
  return `${required.approved} of ${required.required} approvals`;
}
export function readinessPresentation(
  readiness: PullRequestsMergeReadiness,
  _vocabulary: Vocabulary,
): { tone: "success" | "warning" | "danger" | "neutral"; title: string; lines: readonly string[] } {
  // Readiness wording is host-authored, including counts and reviewer names.
  const title = readiness.summary.trim();
  return {
    tone: READINESS_TONES[readiness.status],
    title: readiness.summary,
    lines: readiness.details.filter((line) => line.trim() !== title),
  };
}
function timelineTime(item: PullRequestsTimelineItem) {
  const value =
    item.kind === "review"
      ? item.submittedAt
      : item.kind === "thread"
        ? item.comments[0]?.createdAt
        : item.createdAt;
  const time = value === undefined ? 0 : Date.parse(value);
  return Number.isFinite(time) ? time : 0;
}
export function groupTimeline(items: readonly PullRequestsTimelineItem[]) {
  return items.toSorted((a, b) => timelineTime(a) - timelineTime(b));
}
const CHECKS_LABELS = {
  none: "No checks reported",
  success: "All checks passed",
  pending: "Checks pending",
  failure: "Checks failed",
  neutral: "Checks completed",
};
export function checksSummaryLabel(checks: PullRequestsChecks): string {
  return CHECKS_LABELS[checks.summary];
}
const EVENT_LABELS: Record<string, string> = {
  labeled: "labeled",
  unlabeled: "removed label",
  assigned: "assigned",
  unassigned: "unassigned",
  review_requested: "requested review from",
  ready_for_review: "marked ready for review",
  converted_to_draft: "converted to draft",
  head_ref_force_pushed: "force-pushed the head branch",
  commits_added: "added commits",
  closed: "closed",
  reopened: "reopened",
  merged: "merged",
  milestoned: "set milestone",
  demilestoned: "removed milestone",
  renamed: "changed the title",
  locked: "locked the conversation",
  unlocked: "unlocked the conversation",
};
export function timelineEventText(
  item: Extract<PullRequestsTimelineItem, { kind: "event" }>,
): string {
  // GitLab system notes may already supply the complete host sentence. IDs stay opaque.
  if (
    item.detail &&
    /^(?:assigned to |requested review |closed\b|reopened\b|marked this merge request |merged\b|changed title |added \d+ commits?\b)/.test(
      item.detail,
    )
  )
    return item.detail;
  const label = Object.hasOwn(EVENT_LABELS, item.event) ? EVENT_LABELS[item.event] : undefined;
  return label
    ? `${label}${item.detail ? ` ${item.detail}` : ""}`
    : `updated the request${item.detail ? `: ${item.detail}` : ""}`;
}
export type FileTreeRow =
  | { kind: "folder"; path: string; depth: number }
  | { kind: "file"; path: string; depth: number; file: PullRequestsFile };
export function fileTree(files: readonly PullRequestsFile[]): FileTreeRow[] {
  interface Folder {
    folders: Map<string, Folder>;
    files: PullRequestsFile[];
  }
  const root: Folder = { folders: new Map(), files: [] };
  for (const file of files) {
    const parts = file.path.split("/");
    let folder = root;
    for (const part of parts.slice(0, -1)) {
      let child = folder.folders.get(part);
      if (!child) {
        child = { folders: new Map(), files: [] };
        folder.folders.set(part, child);
      }
      folder = child;
    }
    folder.files.push(file);
  }
  const rows: FileTreeRow[] = [];
  const visit = (folder: Folder, prefix: string, depth: number) => {
    for (const [name, child] of [...folder.folders].sort(([a], [b]) => a.localeCompare(b))) {
      const path = prefix ? `${prefix}/${name}` : name;
      rows.push({ kind: "folder", path, depth });
      visit(child, path, depth + 1);
    }
    for (const file of folder.files.toSorted((a, b) => a.path.localeCompare(b.path)))
      rows.push({ kind: "file", path: file.path, depth, file });
  };
  visit(root, "", 0);
  return rows;
}
/** Convert whitespace-only pairs to context without inventing full file contents or renumbering lines. */
export function patchWithoutWhitespaceChanges(patch: string): string {
  const lines = patch.split("\n");
  const output: string[] = [];
  let oldRemaining = 0;
  let newRemaining = 0;
  for (let index = 0; index < lines.length;) {
    const line = lines[index]!;
    const header = /^@@ -\d+(?:,(\d+))? \+\d+(?:,(\d+))? @@/.exec(line);
    if (header) {
      oldRemaining = Number(header[1] ?? 1);
      newRemaining = Number(header[2] ?? 1);
      output.push(line);
      index++;
      continue;
    }
    if ((oldRemaining > 0 && line.startsWith("-")) || (newRemaining > 0 && line.startsWith("+"))) {
      const block: string[] = [];
      const before: string[] = [];
      const after: string[] = [];
      while (index < lines.length) {
        const current = lines[index]!;
        if (current.startsWith("-") && oldRemaining > 0) {
          before.push(current.slice(1));
          oldRemaining--;
        } else if (current.startsWith("+") && newRemaining > 0) {
          after.push(current.slice(1));
          newRemaining--;
        } else if (!current.startsWith("\\")) break;
        block.push(current);
        index++;
      }
      if (before.length === after.length && !block.some((entry) => entry.startsWith("\\"))) {
        for (let offset = 0; offset < before.length; offset++) {
          const old = before[offset]!;
          const next = after[offset]!;
          if (old.replace(/\s/g, "") === next.replace(/\s/g, "")) output.push(` ${next}`);
          else output.push(`-${old}`, `+${next}`);
        }
      } else for (const entry of block) output.push(entry);
      continue;
    }
    if (line.startsWith(" ") && oldRemaining > 0 && newRemaining > 0) {
      oldRemaining--;
      newRemaining--;
    }
    output.push(line);
    index++;
  }
  return output.join("\n");
}
