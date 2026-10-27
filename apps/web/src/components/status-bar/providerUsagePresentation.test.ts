import type { ServerProviderUsageSnapshot } from "@coddy/contracts";
import * as DateTime from "effect/DateTime";
import { describe, expect, it } from "vite-plus/test";

import { buildProviderUsageViewModel } from "./providerUsagePresentation";

const now = DateTime.makeUnsafe("2026-07-07T18:00:00.000Z");
const updatedAt = DateTime.makeUnsafe("2026-07-07T17:59:00.000Z");

function providerSnapshot(
  patch: Partial<ServerProviderUsageSnapshot> = {},
): ServerProviderUsageSnapshot {
  return {
    provider: "codex",
    status: "ok",
    session: {
      usedPercent: 20,
      windowMinutes: 300,
      resetsAt: DateTime.makeUnsafe("2026-07-07T18:05:00.000Z"),
      resetDescription: "Resets later today",
    },
    weekly: {
      usedPercent: 80,
      windowMinutes: 10080,
      resetsAt: null,
      resetDescription: null,
    },
    fableWeekly: {
      usedPercent: 65,
      windowMinutes: 10080,
      resetsAt: null,
      resetDescription: null,
    },
    planType: null,
    rateLimitResetCredits: null,
    updatedAt,
    error: null,
    metadata: {},
    ...patch,
  };
}

describe("providerUsagePresentation", () => {
  it("clamps consumed values before deriving remaining display and fill", () => {
    const vm = buildProviderUsageViewModel(
      providerSnapshot({
        session: { usedPercent: -5, windowMinutes: 300, resetsAt: null, resetDescription: null },
        weekly: {
          usedPercent: 150,
          windowMinutes: 10080,
          resetsAt: null,
          resetDescription: null,
        },
        fableWeekly: {
          usedPercent: Number.NaN,
          windowMinutes: 10080,
          resetsAt: null,
          resetDescription: null,
        },
      }),
      { now },
    );

    expect(vm.windows.map((window) => window.consumedPercent)).toEqual([0, 100, 0]);
    expect(vm.windows.map((window) => window.displayedPercent)).toEqual([100, 0, 100]);
    expect(vm.windows.map((window) => window.fillPercent)).toEqual([100, 0, 100]);
    expect(vm.windows.map((window) => window.percentageLabel)).toEqual([
      "100% remaining",
      "0% remaining",
      "100% remaining",
    ]);
    expect(vm.compactWindows.map((window) => window.key)).toEqual(["weekly"]);
  });

  it("changes only labels and fills for Used display", () => {
    const snapshot = providerSnapshot({
      session: { usedPercent: 25, windowMinutes: 300, resetsAt: null, resetDescription: null },
      weekly: { usedPercent: 80, windowMinutes: 10080, resetsAt: null, resetDescription: null },
      fableWeekly: null,
    });

    const remaining = buildProviderUsageViewModel(snapshot, {
      now,
      percentageDisplay: "remaining",
    });
    const used = buildProviderUsageViewModel(snapshot, { now, percentageDisplay: "used" });

    expect(remaining.windows.map((window) => window.consumedPercent)).toEqual([25, 80]);
    expect(used.windows.map((window) => window.consumedPercent)).toEqual([25, 80]);
    expect(remaining.compactWindows.map((window) => window.key)).toEqual(["weekly"]);
    expect(used.compactWindows.map((window) => window.key)).toEqual(["weekly"]);
    expect(remaining.windows.map((window) => [window.fillPercent, window.percentageLabel])).toEqual(
      [
        [75, "75% remaining"],
        [20, "20% remaining"],
      ],
    );
    expect(used.windows.map((window) => [window.fillPercent, window.percentageLabel])).toEqual([
      [25, "25% used"],
      [80, "80% used"],
    ]);
  });

  it("keeps detailed windows in Session, Weekly, Fable order and makes compact output single-window", () => {
    const vm = buildProviderUsageViewModel(providerSnapshot(), { now });

    expect(vm.windows.map((window) => [window.key, window.label])).toEqual([
      ["session", "Session"],
      ["weekly", "Weekly"],
      ["fable", "Fable"],
    ]);
    expect(vm.detailedWindows.map((window) => window.key)).toEqual(["session", "weekly", "fable"]);
    expect(vm.compactWindows.map((window) => window.key)).toEqual(["weekly"]);
  });

  it("handles missing windows without fabricated compact output", () => {
    const vm = buildProviderUsageViewModel(
      providerSnapshot({ session: null, weekly: null, fableWeekly: null }),
      { now },
    );

    expect(vm.windows).toEqual([]);
    expect(vm.compactWindows).toEqual([]);
  });

  it("uses deterministic reset countdowns and presentation-only plan title casing", () => {
    const snapshot = providerSnapshot({ planType: "  chatgpt plus  " });
    const vm = buildProviderUsageViewModel(snapshot, { now });

    expect(vm.windows[0]?.resetLabel).toBe("Resets in 5m");
    expect(vm.plan?.label).toBe("Chatgpt Plus");
    expect(vm.plan?.value).toBe("  chatgpt plus  ");
    expect(snapshot.planType).toBe("  chatgpt plus  ");
  });
});
