import type { StatusBarItem } from "@coddy/contracts/settings";
import { usePrimarySettings, useUpdatePrimarySettings } from "../../hooks/useSettings";
import { Switch } from "../ui/switch";
import { Toggle, ToggleGroup } from "../ui/toggle-group";
import { SettingsRow, SettingsSection } from "./settingsLayout";

const STATUS_BAR_ITEM_ORDER = [
  "claude",
  "codex",
  "resource-usage",
] as const satisfies readonly StatusBarItem[];

export function setStatusBarItemVisible(
  items: readonly StatusBarItem[],
  item: StatusBarItem,
  visible: boolean,
): StatusBarItem[] {
  const visibleItems = new Set(items);
  if (visible) {
    visibleItems.add(item);
  } else {
    visibleItems.delete(item);
  }
  return STATUS_BAR_ITEM_ORDER.filter((statusBarItem) => visibleItems.has(statusBarItem));
}

export function StatusBarSettingsSection() {
  const settings = usePrimarySettings();
  const updateSettings = useUpdatePrimarySettings();

  return (
    <SettingsSection title="Status bar">
      <SettingsRow
        title="Usage percentage"
        description="Choose whether usage shows what is used or what remains."
        control={
          <ToggleGroup
            aria-label="Usage percentage"
            variant="outline"
            size="xs"
            value={[settings.usagePercentageDisplay]}
            onValueChange={(value) => {
              const next = value[0];
              if (next === "used" || next === "remaining") {
                updateSettings({ usagePercentageDisplay: next });
              }
            }}
          >
            <Toggle value="used">Used</Toggle>
            <Toggle value="remaining">Remaining</Toggle>
          </ToggleGroup>
        }
      />
      <SettingsRow
        title="Footer detail"
        description="Choose how much usage detail appears in the status bar."
        control={
          <ToggleGroup
            aria-label="Footer detail"
            variant="outline"
            size="xs"
            value={[settings.statusBarUsageMode]}
            onValueChange={(value) => {
              const next = value[0];
              if (next === "detailed" || next === "compact") {
                updateSettings({ statusBarUsageMode: next });
              }
            }}
          >
            <Toggle value="detailed">Detailed</Toggle>
            <Toggle value="compact">Compact</Toggle>
          </ToggleGroup>
        }
      />
      <SettingsRow
        title="Show Claude usage"
        description="Show Claude usage in the status bar."
        control={
          <Switch
            checked={settings.statusBarItems.includes("claude")}
            onCheckedChange={(checked) =>
              updateSettings({
                statusBarItems: setStatusBarItemVisible(
                  settings.statusBarItems,
                  "claude",
                  Boolean(checked),
                ),
              })
            }
            aria-label="Show Claude usage"
          />
        }
      />
      <SettingsRow
        title="Show Codex usage"
        description="Show Codex usage in the status bar."
        control={
          <Switch
            checked={settings.statusBarItems.includes("codex")}
            onCheckedChange={(checked) =>
              updateSettings({
                statusBarItems: setStatusBarItemVisible(
                  settings.statusBarItems,
                  "codex",
                  Boolean(checked),
                ),
              })
            }
            aria-label="Show Codex usage"
          />
        }
      />
      <SettingsRow
        title="Show Resource Manager"
        description="Show Resource Manager usage in the status bar."
        control={
          <Switch
            checked={settings.statusBarItems.includes("resource-usage")}
            onCheckedChange={(checked) =>
              updateSettings({
                statusBarItems: setStatusBarItemVisible(
                  settings.statusBarItems,
                  "resource-usage",
                  Boolean(checked),
                ),
              })
            }
            aria-label="Show Resource Manager"
          />
        }
      />
    </SettingsSection>
  );
}
