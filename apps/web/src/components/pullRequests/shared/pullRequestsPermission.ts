import type { PullRequestsPermission } from "@coddy/contracts";

/** Release placeholder only; a host denial and its wording always take precedence. */
export function readOnlyPermission(
  permission: PullRequestsPermission,
  reason = "This action arrives in a later build",
): PullRequestsPermission {
  return permission.allowed ? { allowed: false, reason } : permission;
}
