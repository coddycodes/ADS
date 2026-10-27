import { describe, expect, it } from "vite-plus/test";
import { ProviderInstanceId } from "@coddy/contracts";

import type { ComposerCommandItem } from "./composerCommandItems";
import { searchSlashCommandItems } from "./composerSlashCommandSearch";

describe("searchSlashCommandItems", () => {
  const claudeInstanceId = ProviderInstanceId.make("claudeAgent");

  it("moves exact provider command matches ahead of broader description matches", () => {
    const items = [
      {
        id: "provider-slash-command:claudeAgent:ui",
        type: "provider-command",
        group: "commands",
        providerInstanceId: claudeInstanceId,
        command: { name: "ui" },
        label: "/ui",
        description: "Explore, build, and refine UI.",
        replacement: "/ui ",
      },
      {
        id: "provider-slash-command:claudeAgent:frontend-design",
        type: "provider-command",
        group: "commands",
        providerInstanceId: claudeInstanceId,
        command: { name: "frontend-design" },
        label: "/frontend-design",
        description: "Create distinctive, production-grade frontend interfaces",
        replacement: "/frontend-design ",
      },
      {
        id: "slash-skill:claudeAgent:ui-review",
        type: "provider-skill",
        group: "skills",
        providerInstanceId: claudeInstanceId,
        skill: {
          name: "ui-review",
          path: "/skills/ui-review",
          enabled: true,
          invocation: "slash",
        },
        label: "/ui-review",
        description: "Review user interfaces",
        replacement: "/ui-review ",
      },
    ] satisfies Array<
      Extract<ComposerCommandItem, { type: "provider-command" | "provider-skill" }>
    >;

    expect(searchSlashCommandItems(items, "ui").map((item) => item.id)).toEqual([
      "provider-slash-command:claudeAgent:ui",
      "slash-skill:claudeAgent:ui-review",
    ]);
  });

  it("supports fuzzy provider command and slash-skill matches", () => {
    const items = [
      {
        id: "slash-skill:claudeAgent:gh-fix-ci",
        type: "provider-skill",
        group: "skills",
        providerInstanceId: claudeInstanceId,
        skill: {
          name: "gh-fix-ci",
          path: "/skills/gh-fix-ci",
          enabled: true,
          invocation: "slash",
        },
        label: "/gh-fix-ci",
        description: "Fix failing GitHub Actions",
        replacement: "/gh-fix-ci ",
      },
      {
        id: "provider-slash-command:claudeAgent:github",
        type: "provider-command",
        group: "commands",
        providerInstanceId: claudeInstanceId,
        command: { name: "github" },
        label: "/github",
        description: "General GitHub help",
        replacement: "/github ",
      },
    ] satisfies Array<
      Extract<ComposerCommandItem, { type: "provider-command" | "provider-skill" }>
    >;

    expect(searchSlashCommandItems(items, "gfc").map((item) => item.id)).toEqual([
      "slash-skill:claudeAgent:gh-fix-ci",
    ]);
  });

  it("matches provider commands through their descriptions", () => {
    const items = [
      {
        id: "provider-slash-command:claudeAgent:review",
        type: "provider-command",
        group: "commands",
        providerInstanceId: claudeInstanceId,
        command: { name: "review" },
        label: "/review",
        description: "Review staged changes",
        replacement: "/review ",
      },
    ] satisfies Array<
      Extract<ComposerCommandItem, { type: "provider-command" | "provider-skill" }>
    >;

    expect(searchSlashCommandItems(items, "staged").map((item) => item.id)).toEqual([
      "provider-slash-command:claudeAgent:review",
    ]);
  });

  it("sorts empty-query results deterministically instead of preserving inventory order", () => {
    const alpha = {
      id: "provider-slash-command:claudeAgent:alpha",
      type: "provider-command",
      group: "commands",
      providerInstanceId: claudeInstanceId,
      command: { name: "alpha" },
      label: "/alpha",
      description: "Alpha command",
      replacement: "/alpha ",
    } as const;
    const zebra = {
      id: "provider-slash-command:claudeAgent:zebra",
      type: "provider-command",
      group: "commands",
      providerInstanceId: claudeInstanceId,
      command: { name: "zebra" },
      label: "/zebra",
      description: "Zebra command",
      replacement: "/zebra ",
    } as const;

    expect(searchSlashCommandItems([zebra, alpha], "").map((item) => item.id)).toEqual([
      alpha.id,
      zebra.id,
    ]);
    expect(searchSlashCommandItems([alpha, zebra], "").map((item) => item.id)).toEqual([
      alpha.id,
      zebra.id,
    ]);
  });
});
