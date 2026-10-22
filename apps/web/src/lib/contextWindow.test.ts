import { describe, expect, it } from "vite-plus/test";
import { EventId, type OrchestrationThreadActivity, TurnId } from "@bibcode/contracts";

import {
  deriveLatestContextWindowSnapshot,
  formatContextWindowTokens,
  formatProviderDisplayName,
} from "./contextWindow";

function makeActivity(id: string, kind: string, payload: unknown): OrchestrationThreadActivity {
  return {
    id: EventId.make(id),
    tone: "info",
    kind,
    summary: kind,
    payload,
    turnId: TurnId.make("turn-1"),
    createdAt: "2026-03-23T00:00:00.000Z",
  };
}

describe("contextWindow", () => {
  it("derives the latest valid context window snapshot", () => {
    const snapshot = deriveLatestContextWindowSnapshot([
      makeActivity("activity-1", "context-window.updated", {
        usedTokens: 1000,
      }),
      makeActivity("activity-2", "tool.started", {}),
      makeActivity("activity-3", "context-window.updated", {
        usedTokens: 14_000,
        maxTokens: 258_000,
        compactsAutomatically: true,
        autoCompactTokens: 245_000,
      }),
    ]);

    expect(snapshot).not.toBeNull();
    expect(snapshot?.usedTokens).toBe(14_000);
    expect(snapshot?.totalProcessedTokens).toBeNull();
    expect(snapshot?.maxTokens).toBe(258_000);
    expect(snapshot?.compactsAutomatically).toBe(true);
    expect(snapshot?.autoCompactTokens).toBe(245_000);
  });

  it("ignores malformed payloads", () => {
    const snapshot = deriveLatestContextWindowSnapshot([
      makeActivity("activity-1", "context-window.updated", {}),
    ]);

    expect(snapshot).toBeNull();
  });

  it("keeps valid zero-usage snapshots", () => {
    const snapshot = deriveLatestContextWindowSnapshot([
      makeActivity("activity-1", "context-window.updated", {
        usedTokens: 0,
        maxTokens: 100_000,
      }),
    ]);

    expect(snapshot).toMatchObject({
      usedTokens: 0,
      maxTokens: 100_000,
      remainingTokens: 100_000,
      usedPercentage: 0,
      remainingPercentage: 100,
    });
  });

  it("formats compact token counts", () => {
    expect(formatContextWindowTokens(999)).toBe("999");
    expect(formatContextWindowTokens(1400)).toBe("1.4k");
    expect(formatContextWindowTokens(14_000)).toBe("14k");
    expect(formatContextWindowTokens(258_000)).toBe("258k");
  });

  it("includes total processed tokens when available", () => {
    const snapshot = deriveLatestContextWindowSnapshot([
      makeActivity("activity-1", "context-window.updated", {
        usedTokens: 81_659,
        totalProcessedTokens: 748_126,
        maxTokens: 258_400,
        lastUsedTokens: 81_659,
      }),
    ]);

    expect(snapshot?.usedTokens).toBe(81_659);
    expect(snapshot?.totalProcessedTokens).toBe(748_126);
  });

  it("formats known, missing, and unknown provider names", () => {
    expect(formatProviderDisplayName(null)).toBe("This agent");
    expect(formatProviderDisplayName("claudeAgent")).toBe("Claude");
    expect(formatProviderDisplayName("claude")).toBe("Claude");
    expect(formatProviderDisplayName("codex")).toBe("Codex");
    expect(formatProviderDisplayName("cursor")).toBe("Cursor");
    expect(formatProviderDisplayName("opencode")).toBe("OpenCode");
    expect(formatProviderDisplayName("customAgent")).toBe("Custom");
    expect(formatProviderDisplayName("Agent")).toBe("Agent");
  });

  it("skips absent, unrelated, primitive, and negative usage activities", () => {
    expect(
      deriveLatestContextWindowSnapshot([
        undefined,
        makeActivity("wrong", "tool.started", {}),
        makeActivity("primitive", "context-window.updated", "invalid"),
        makeActivity("negative", "context-window.updated", { usedTokens: -1 }),
      ] as never),
    ).toBeNull();
  });

  it("keeps usage when capacity and optional fields are invalid", () => {
    expect(
      deriveLatestContextWindowSnapshot([
        makeActivity("invalid-optional", "context-window.updated", {
          usedTokens: 5,
          maxTokens: 0,
          totalProcessedTokens: -1,
          inputTokens: Number.NaN,
          compactsAutomatically: "yes",
        }),
      ]),
    ).toMatchObject({
      usedTokens: 5,
      maxTokens: null,
      totalProcessedTokens: null,
      remainingTokens: null,
      usedPercentage: null,
      remainingPercentage: null,
      inputTokens: null,
      compactsAutomatically: false,
    });
    expect(
      deriveLatestContextWindowSnapshot([
        makeActivity("activity", "context-window.updated", { usedTokens: 5 }),
      ]),
    ).toMatchObject({
      maxTokens: null,
      remainingTokens: null,
      usedPercentage: null,
      remainingPercentage: null,
    });
  });

  it("keeps the latest valid snapshot when a newer usage activity is malformed", () => {
    expect(
      deriveLatestContextWindowSnapshot([
        makeActivity("valid", "context-window.updated", { usedTokens: 5, maxTokens: 100 }),
        makeActivity("malformed", "context-window.updated", { usedTokens: -1 }),
      ]),
    ).toMatchObject({ usedTokens: 5, maxTokens: 100 });
  });

  it("formats null, non-finite, million, and rounded token boundaries", () => {
    expect(formatContextWindowTokens(null)).toBe("0");
    expect(formatContextWindowTokens(Number.POSITIVE_INFINITY)).toBe("0");
    expect(formatContextWindowTokens(9_999)).toBe("10k");
    expect(formatContextWindowTokens(1_000_000)).toBe("1m");
    expect(formatContextWindowTokens(1_250_000)).toBe("1.3m");
  });
});
