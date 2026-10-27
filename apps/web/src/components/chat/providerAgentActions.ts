import type { DefaultAgentSelection, ServerProvider, ServerSettings } from "@coddy/contracts";

import {
  applyProviderInstanceSettings,
  deriveProviderInstanceEntries,
  type ProviderInstanceEntry,
} from "~/providerInstances";
import { buildPanelMenuModel } from "./ChatHeaderPanelMenu.logic";
import {
  resolveProviderTerminalAction,
  type ProviderTerminalActionItem,
} from "./providerTerminalActions";

export type ProviderAgentSettings = Pick<
  ServerSettings,
  "providerInstances" | "providers" | "providerSessionDefaults"
>;

export type ProviderAgentAction =
  | {
      readonly kind: "chat";
      readonly value: `chat:${string}`;
      readonly selection: DefaultAgentSelection;
      readonly entry: ProviderInstanceEntry;
      readonly label: string;
      readonly disabled: boolean;
      readonly disabledReason?: string;
    }
  | {
      readonly kind: "terminal";
      readonly value: `terminal:${string}`;
      readonly selection: DefaultAgentSelection;
      readonly entry: ProviderInstanceEntry;
      readonly label: string;
      readonly disabled: boolean;
      readonly disabledReason?: string;
      readonly terminalAction: ProviderTerminalActionItem;
    };

export function buildProviderAgentActions(
  providerStatuses: ReadonlyArray<ServerProvider>,
  settings: ProviderAgentSettings,
): ReadonlyArray<ProviderAgentAction> {
  const providerItems = buildPanelMenuModel(
    applyProviderInstanceSettings(deriveProviderInstanceEntries(providerStatuses), settings),
  );
  const chats = providerItems.map<ProviderAgentAction>((item) => ({
    kind: "chat",
    value: `chat:${item.entry.instanceId}`,
    selection: { kind: "chat", instanceId: item.entry.instanceId },
    entry: item.entry,
    label: item.entry.displayName,
    disabled: item.disabled,
    ...(item.disabledReason ? { disabledReason: item.disabledReason } : {}),
  }));
  const terminals = providerItems.flatMap<ProviderAgentAction>((item) => {
    const terminalAction = resolveProviderTerminalAction(item.entry, settings);
    if (terminalAction === null) return [];
    const disabled = item.disabled || terminalAction.command === null;
    const disabledReason = item.disabledReason ?? terminalAction.disabledReason;
    return [
      {
        kind: "terminal",
        value: `terminal:${item.entry.instanceId}`,
        selection: { kind: "terminal", instanceId: item.entry.instanceId },
        entry: item.entry,
        label: terminalAction.label,
        disabled,
        ...(disabledReason ? { disabledReason } : {}),
        terminalAction,
      },
    ];
  });
  return [...chats, ...terminals];
}

export function isProviderAgentActionSelectable(action: ProviderAgentAction): boolean {
  return !action.disabled;
}

export function resolveEffectiveProviderAgentAction(
  actions: ReadonlyArray<ProviderAgentAction>,
  selection: DefaultAgentSelection,
): ProviderAgentAction | null {
  const saved = actions.find(
    (action) =>
      action.kind === selection.kind &&
      action.entry.instanceId === selection.instanceId &&
      isProviderAgentActionSelectable(action),
  );
  return (
    saved ??
    actions.find((action) => action.kind === "chat" && isProviderAgentActionSelectable(action)) ??
    null
  );
}
