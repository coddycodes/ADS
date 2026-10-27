export function getBulkThreadDeletionConfirmation(
  threadCount: number,
  worktreeCount: number,
): string {
  return [
    `Delete ${threadCount} thread${threadCount === 1 ? "" : "s"}?`,
    `This permanently clears conversation history for ${threadCount === 1 ? "this thread" : "these threads"}.`,
    ...(worktreeCount > 0
      ? [
          `${worktreeCount} worktree-backed thread${worktreeCount === 1 ? "" : "s"} will be removed from Coddy Studio only. Git worktrees and files are left untouched.`,
        ]
      : []),
  ].join("\n");
}

export function formatWorktreePathForDisplay(worktreePath: string): string {
  const trimmed = worktreePath.trim();
  if (!trimmed) {
    return worktreePath;
  }

  const normalized = trimmed.replace(/\\/g, "/").replace(/\/+$/, "");
  const parts = normalized.split("/");
  const lastPart = parts[parts.length - 1]?.trim() ?? "";
  return lastPart.length > 0 ? lastPart : trimmed;
}
