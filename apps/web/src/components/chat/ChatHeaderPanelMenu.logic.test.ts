import { ProviderDriverKind, ProviderInstanceId } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import type { ProviderInstanceEntry } from "~/providerInstances";
import { buildPanelMenuModel } from "./ChatHeaderPanelMenu.logic";

function makeEntry(input: {
  instanceId: string;
  driverKind?: string;
  displayName?: string;
  enabled?: boolean;
  isAvailable?: boolean;
}): ProviderInstanceEntry {
  return {
    instanceId: ProviderInstanceId.make(input.instanceId),
    driverKind: ProviderDriverKind.make(input.driverKind ?? "codex"),
    displayName: input.displayName ?? input.instanceId,
    accentColor: undefined,
    continuationGroupKey: undefined,
    enabled: input.enabled ?? true,
    installed: true,
    status: "ready",
    isDefault: true,
    isAvailable: input.isAvailable ?? true,
    snapshot: {} as ProviderInstanceEntry["snapshot"],
    models: [],
  };
}

describe("buildPanelMenuModel", () => {
  it("keeps only settings-enabled instances and preserves order", () => {
    const model = buildPanelMenuModel([
      makeEntry({ instanceId: "codex", displayName: "Codex" }),
      makeEntry({ instanceId: "disabled", enabled: false }),
      makeEntry({ instanceId: "claude", displayName: "Claude" }),
    ]);
    expect(model.map((item) => item.entry.instanceId)).toEqual([
      ProviderInstanceId.make("codex"),
      ProviderInstanceId.make("claude"),
    ]);
  });

  it("keeps enabled but unavailable providers visible and disables their action", () => {
    const [ready, notReady] = buildPanelMenuModel([
      makeEntry({ instanceId: "codex" }),
      makeEntry({ instanceId: "claude", isAvailable: false }),
    ]);
    expect(ready?.disabled).toBe(false);
    expect(ready?.disabledReason).toBeUndefined();
    expect(notReady?.disabled).toBe(true);
    expect(notReady?.disabledReason).toBe(
      "This provider isn't ready yet — check its connection in Settings.",
    );
  });

  it("hides Grok even when an enabled inventory snapshot is present", () => {
    const model = buildPanelMenuModel([
      makeEntry({ instanceId: "codex", displayName: "Codex" }),
      makeEntry({ instanceId: "grok", driverKind: "grok", displayName: "Grok" }),
      makeEntry({ instanceId: "opencode", driverKind: "opencode", displayName: "OpenCode" }),
    ]);

    expect(model.map((item) => item.entry.displayName)).toEqual(["Codex", "OpenCode"]);
  });
});
