import type { EnvironmentId, ServerConfig } from "@coddy/contracts";

import type { SidebarThreadSummary } from "../../types";
import { formatWorktreePathForDisplay } from "../../worktreeCleanup";
import { getTriggerDisplayModelName } from "../chat/providerIconUtils";
import type { WorkspaceCardStatus } from "../Sidebar.logic";
import { resolveAgentProvider, resolveConversationPreviewLine } from "./agentsSection.logic";

export interface WorkspaceCardPreview {
  readonly text: string | null;
  readonly tone: "destructive" | "warning" | null;
}

/**
 * Card line 3's text, first match wins: an unresolved delivery; the tool while
 * working, else the latest assistant message, else the prompt; the provider label.
 */
export function resolveWorkspaceCardPreview(
  thread: Pick<SidebarThreadSummary, "unresolvedDelivery" | "conversationPreview" | "session">,
  status: WorkspaceCardStatus,
): WorkspaceCardPreview {
  const delivery = thread.unresolvedDelivery ?? null;
  if (delivery?.state === "failed") {
    return { text: "Delivery failed", tone: "destructive" };
  }
  if (delivery?.state === "uncertain") {
    return { text: "Delivery uncertain", tone: "warning" };
  }
  const line = resolveConversationPreviewLine(
    status.kind === "working",
    thread.conversationPreview,
  );
  if (line !== null) {
    return { text: line, tone: null };
  }
  return { text: resolveAgentProvider(thread.session?.providerName).label, tone: null };
}

export function workspaceModelLabelKey(
  environmentId: string,
  instanceId: string,
  slug: string,
): string {
  return `${environmentId}\u0000${instanceId}\u0000${slug}`;
}

/**
 * One lookup per project list, built from the loaded server configs, so each
 * card resolves its model's catalog short name in O(1).
 */
export function buildWorkspaceModelLabels(
  serverConfigs: ReadonlyMap<EnvironmentId, Pick<ServerConfig, "providers">>,
): ReadonlyMap<string, string> {
  const labels = new Map<string, string>();
  for (const [environmentId, config] of serverConfigs) {
    for (const provider of config.providers ?? []) {
      for (const model of provider.models ?? []) {
        // Only short names: a model without one falls back to its slug in
        // `resolveWorkspaceModelLabel`, never to its (often descriptive) name.
        if (!model.shortName) continue;
        labels.set(
          workspaceModelLabelKey(environmentId, provider.instanceId, model.slug),
          getTriggerDisplayModelName(model),
        );
      }
    }
  }
  return labels;
}

/** Line 3's model: the catalog short name, else the slug. */
export function resolveWorkspaceModelLabel(
  labels: ReadonlyMap<string, string>,
  thread: Pick<SidebarThreadSummary, "environmentId" | "modelSelection">,
): string {
  return (
    labels.get(
      workspaceModelLabelKey(
        thread.environmentId,
        thread.modelSelection.instanceId,
        thread.modelSelection.model,
      ),
    ) ?? thread.modelSelection.model
  );
}

/** Line 2's branch tooltip names where the card runs, as the old worktree label did. */
export function resolveWorkspaceCardBranchTooltip(input: {
  readonly branch: string;
  readonly worktreePath: string | null;
  readonly checkoutPath: string | null;
}): string {
  if (input.worktreePath !== null) {
    return `Worktree: ${formatWorktreePathForDisplay(input.worktreePath)} (${input.branch})`;
  }
  if (input.checkoutPath !== null) {
    return `Checkout: ${formatWorktreePathForDisplay(input.checkoutPath)} (${input.branch})`;
  }
  return input.branch;
}
