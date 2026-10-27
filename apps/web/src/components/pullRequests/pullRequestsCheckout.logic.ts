import type {
  PullRequestsCheckoutInput,
  PullRequestsCheckoutResult,
  VcsWorktreeDescriptor,
} from "@coddy/contracts";
export type CheckoutWorktree = Pick<
  VcsWorktreeDescriptor,
  "path" | "branch" | "isBare" | "directoryState"
>;
export type CheckoutTarget = PullRequestsCheckoutInput["target"] & {
  label: string;
  branch?: string | null;
};
export function checkoutTargets(
  worktrees: readonly CheckoutWorktree[],
  selectedCwd: string,
): CheckoutTarget[] {
  const targets: CheckoutTarget[] = [
    { kind: "checkout", cwd: selectedCwd, label: "Current checkout" },
  ];
  const seen = new Set([selectedCwd]);
  for (const worktree of worktrees) {
    if (seen.has(worktree.path) || worktree.isBare || worktree.directoryState !== "present")
      continue;
    seen.add(worktree.path);
    targets.push({
      kind: "checkout",
      cwd: worktree.path,
      label: "Another worktree…",
      branch: worktree.branch,
    });
  }
  targets.push({ kind: "worktree", branchName: null, label: "New worktree…" });
  return targets;
}
export function resultMessage(result: PullRequestsCheckoutResult): string {
  switch (result.kind) {
    case "checked_out":
      return `Checked out ${result.branch} in ${result.cwd}`;
    case "worktree_created":
      return `Created worktree ${result.cwd} on ${result.branch}`;
    case "blocked":
      return result.reason.message;
  }
}
export function occupiedCheckout(
  result: PullRequestsCheckoutResult,
  worktrees: readonly CheckoutWorktree[],
  branch: string,
): string | null {
  if (result.kind !== "blocked" || result.reason.code !== "worktree-checked-out") return null;
  return (
    worktrees.find((w) => w.branch === branch && !w.isBare && w.directoryState === "present")
      ?.path ?? null
  );
}
