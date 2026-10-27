import { EnvironmentId, ProviderInstanceId, type ServerConfig } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import { WORKSPACE_CARD_STATUS } from "../Sidebar.logic";
import {
  buildWorkspaceModelLabels,
  resolveWorkspaceCardBranchTooltip,
  resolveWorkspaceCardPreview,
  resolveWorkspaceModelLabel,
} from "./workspaceCard.logic";

const preview = {
  prompt: "Fix the export dates",
  tool: "Editing src/export/invoiceDates.ts",
  assistantMessage: "Renderer upgraded; export tests pass",
};
const session = { providerName: "claudeAgent" } as never;

describe("resolveWorkspaceCardPreview", () => {
  it("reports an unresolved delivery first", () => {
    expect(
      resolveWorkspaceCardPreview(
        { unresolvedDelivery: { state: "failed" }, conversationPreview: preview, session },
        WORKSPACE_CARD_STATUS.failed,
      ),
    ).toEqual({ text: "Delivery failed", tone: "destructive" });
    expect(
      resolveWorkspaceCardPreview(
        { unresolvedDelivery: { state: "uncertain" }, conversationPreview: preview, session },
        WORKSPACE_CARD_STATUS.working,
      ),
    ).toEqual({ text: "Delivery uncertain", tone: "warning" });
  });

  it("shows the tool while working, else the reply, else the provider", () => {
    expect(
      resolveWorkspaceCardPreview(
        { conversationPreview: preview, session },
        WORKSPACE_CARD_STATUS.working,
      ).text,
    ).toBe("Editing src/export/invoiceDates.ts");
    expect(
      resolveWorkspaceCardPreview(
        { conversationPreview: preview, session },
        WORKSPACE_CARD_STATUS.done,
      ).text,
    ).toBe("Renderer upgraded; export tests pass");
    expect(
      resolveWorkspaceCardPreview(
        { conversationPreview: null, session },
        WORKSPACE_CARD_STATUS.idle,
      ),
    ).toEqual({ text: "Claude", tone: null });
    expect(
      resolveWorkspaceCardPreview(
        { conversationPreview: null, session: null },
        WORKSPACE_CARD_STATUS.idle,
      ),
    ).toEqual({ text: null, tone: null });
  });
});

describe("workspace model labels", () => {
  const environmentId = EnvironmentId.make("env-main");
  const instanceId = ProviderInstanceId.make("claude");
  const serverConfigs = new Map([
    [
      environmentId,
      {
        providers: [
          {
            instanceId,
            models: [
              {
                slug: "claude-opus-5",
                name: "Claude Opus 5",
                shortName: "opus",
                isCustom: false,
                capabilities: null,
              },
              {
                slug: "claude-sonnet-5",
                name: "Claude Sonnet 5",
                isCustom: false,
                capabilities: null,
              },
              {
                slug: "sonnet",
                name: "Most efficient for everyday tasks",
                isCustom: false,
                capabilities: null,
              },
            ],
          },
        ],
      } as unknown as ServerConfig,
    ],
  ]);
  const labels = buildWorkspaceModelLabels(serverConfigs);

  it("uses the catalog short name, then the slug, never the descriptive name", () => {
    expect(
      resolveWorkspaceModelLabel(labels, {
        environmentId,
        modelSelection: { instanceId, model: "claude-opus-5" },
      }),
    ).toBe("opus");
    expect(
      resolveWorkspaceModelLabel(labels, {
        environmentId,
        modelSelection: { instanceId, model: "claude-sonnet-5" },
      }),
    ).toBe("claude-sonnet-5");

    expect(
      resolveWorkspaceModelLabel(labels, {
        environmentId,
        modelSelection: { instanceId, model: "gpt-5-codex" },
      }),
    ).toBe("gpt-5-codex");
  });

  it("shows the slug for a model with no short name and a descriptive name", () => {
    // Some catalogs report aliases this way: name "Most efficient for everyday tasks".
    expect(
      resolveWorkspaceModelLabel(labels, {
        environmentId,
        modelSelection: { instanceId, model: "sonnet" },
      }),
    ).toBe("sonnet");
  });

  it("tolerates configs that have not loaded their providers", () => {
    expect(
      buildWorkspaceModelLabels(new Map([[environmentId, {} as unknown as ServerConfig]])).size,
    ).toBe(0);
  });
});

describe("resolveWorkspaceCardBranchTooltip", () => {
  it("names the worktree or the checkout the card runs in", () => {
    expect(
      resolveWorkspaceCardBranchTooltip({
        branch: "fix-TRI-150",
        worktreePath: "/repos/wt/fix-TRI-150",
        checkoutPath: "/repos/customer-portal",
      }),
    ).toBe("Worktree: fix-TRI-150 (fix-TRI-150)");
    expect(
      resolveWorkspaceCardBranchTooltip({
        branch: "develop",
        worktreePath: null,
        checkoutPath: "/repos/customer-portal",
      }),
    ).toBe("Checkout: customer-portal (develop)");
    expect(
      resolveWorkspaceCardBranchTooltip({
        branch: "develop",
        worktreePath: null,
        checkoutPath: null,
      }),
    ).toBe("develop");
  });
});
