// @vitest-environment happy-dom

import type { ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { DEFAULT_UNIFIED_SETTINGS, type UnifiedSettings } from "@coddy/contracts/settings";

type Control = {
  readonly kind: "toggle-group" | "switch";
  readonly label: string;
  readonly props: Record<string, unknown>;
};

const h = vi.hoisted(() => ({
  settings: null as unknown,
  updateSettings: vi.fn(),
  controls: [] as Control[],
}));

vi.mock("../../hooks/useSettings", () => ({
  usePrimarySettings: () => h.settings,
  useUpdatePrimarySettings: () => h.updateSettings,
}));

vi.mock("./settingsLayout", () => ({
  SettingsSection: (props: { title: string; children: ReactNode }) => (
    <section data-section-title={props.title}>{props.children}</section>
  ),
  SettingsRow: (props: { title: ReactNode; description: ReactNode; control?: ReactNode }) => (
    <div>
      {props.title}
      {props.description}
      {props.control}
    </div>
  ),
}));

vi.mock("../ui/toggle-group", () => ({
  ToggleGroup: (
    props: { "aria-label"?: string; children: ReactNode } & Record<string, unknown>,
  ) => {
    h.controls.push({
      kind: "toggle-group",
      label: props["aria-label"] ?? "",
      props,
    });
    return <div>{props.children}</div>;
  },
  Toggle: (props: { children: ReactNode }) => <button type="button">{props.children}</button>,
}));

vi.mock("../ui/switch", () => ({
  Switch: (props: { "aria-label"?: string } & Record<string, unknown>) => {
    h.controls.push({ kind: "switch", label: props["aria-label"] ?? "", props });
    return <button type="button" aria-label={props["aria-label"]} />;
  },
}));

import { StatusBarSettingsSection } from "./StatusBarSettingsSection";

function render(settings: UnifiedSettings = DEFAULT_UNIFIED_SETTINGS): void {
  h.controls.length = 0;
  h.settings = settings;
  renderToStaticMarkup(<StatusBarSettingsSection />);
}

function control(kind: Control["kind"], label: string): Control {
  const found = h.controls.find((entry) => entry.kind === kind && entry.label === label);
  if (!found) throw new Error(`No ${kind} control labelled ${label}`);
  return found;
}

function invoke(control: Control, handler: string, ...args: unknown[]): void {
  const callback = control.props[handler];
  if (typeof callback !== "function") throw new Error(`No ${handler} handler`);
  (callback as (...values: unknown[]) => void)(...args);
}

beforeEach(() => {
  h.updateSettings.mockReset();
  h.controls.length = 0;
  h.settings = DEFAULT_UNIFIED_SETTINGS;
});

describe("StatusBarSettingsSection", () => {
  it("updates the two controlled display modes and ignores empty selections", () => {
    render();

    const percentage = control("toggle-group", "Usage percentage");
    const footerDetail = control("toggle-group", "Footer detail");

    expect(percentage.props.value).toEqual(["remaining"]);
    expect(footerDetail.props.value).toEqual(["detailed"]);

    invoke(percentage, "onValueChange", ["used"]);
    invoke(footerDetail, "onValueChange", ["compact"]);
    invoke(percentage, "onValueChange", []);
    invoke(footerDetail, "onValueChange", []);

    expect(h.updateSettings).toHaveBeenNthCalledWith(1, { usagePercentageDisplay: "used" });
    expect(h.updateSettings).toHaveBeenNthCalledWith(2, { statusBarUsageMode: "compact" });
    expect(h.updateSettings).toHaveBeenCalledTimes(2);
  });

  it("updates indicator visibility in canonical order regardless of interaction order", () => {
    render({
      ...DEFAULT_UNIFIED_SETTINGS,
      statusBarItems: ["codex", "resource-usage", "claude"],
    });

    const claude = control("switch", "Show Claude usage");
    const codex = control("switch", "Show Codex usage");
    const resource = control("switch", "Show Resource Manager");

    expect(claude.props.checked).toBe(true);
    expect(codex.props.checked).toBe(true);
    expect(resource.props.checked).toBe(true);

    const toggle = (label: string, checked: boolean) => {
      invoke(control("switch", label), "onCheckedChange", checked);
      const patch = h.updateSettings.mock.calls.at(-1)?.[0] as Partial<UnifiedSettings> | undefined;
      if (!patch) throw new Error("Expected status bar settings update");
      render({ ...(h.settings as UnifiedSettings), ...patch });
    };

    toggle("Show Resource Manager", false);
    toggle("Show Claude usage", false);
    toggle("Show Codex usage", false);
    toggle("Show Codex usage", true);
    toggle("Show Resource Manager", true);
    toggle("Show Claude usage", true);

    expect(h.updateSettings).toHaveBeenNthCalledWith(1, {
      statusBarItems: ["claude", "codex"],
    });
    expect(h.updateSettings).toHaveBeenNthCalledWith(2, { statusBarItems: ["codex"] });
    expect(h.updateSettings).toHaveBeenNthCalledWith(3, { statusBarItems: [] });
    expect(h.updateSettings).toHaveBeenNthCalledWith(4, { statusBarItems: ["codex"] });
    expect(h.updateSettings).toHaveBeenNthCalledWith(5, {
      statusBarItems: ["codex", "resource-usage"],
    });
    expect(h.updateSettings).toHaveBeenNthCalledWith(6, {
      statusBarItems: ["claude", "codex", "resource-usage"],
    });
  });
});
