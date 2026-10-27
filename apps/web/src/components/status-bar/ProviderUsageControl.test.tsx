// @vitest-environment happy-dom

import type { StatusBarUsageMode } from "@coddy/contracts/settings";
import * as DateTime from "effect/DateTime";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { ProviderUsageViewModel, UsageWindowViewModel } from "./providerUsagePresentation";

vi.mock("../ui/popover", async () => {
  const React = await vi.importActual<typeof import("react")>("react");
  const MenuContext = React.createContext({
    open: false,
    onOpenChange: (_open: boolean) => {},
  });

  return {
    Popover: ({
      children,
      open = false,
      onOpenChange = () => {},
    }: {
      children: React.ReactNode;
      open?: boolean;
      onOpenChange?: (open: boolean) => void;
    }) => {
      const contextValue = React.useMemo(() => ({ open, onOpenChange }), [onOpenChange, open]);
      return <MenuContext.Provider value={contextValue}>{children}</MenuContext.Provider>;
    },
    PopoverTrigger: ({
      render,
      children,
      ...props
    }: {
      render?: React.ReactElement;
      children?: React.ReactNode;
    } & React.ComponentProps<"button">) => {
      const menu = React.useContext(MenuContext);
      const trigger = render ?? <button type="button">{children}</button>;
      return React.cloneElement(trigger, {
        ...props,
        "aria-expanded": menu.open,
        onClick: () => menu.onOpenChange(!menu.open),
      });
    },
    PopoverPopup: ({
      children,
      viewportClassName: _viewportClassName,
      ...props
    }: React.ComponentProps<"div"> & { viewportClassName?: string }) => {
      const menu = React.useContext(MenuContext);
      return menu.open ? <div {...props}>{children}</div> : null;
    },
  };
});

import { ProviderUsageControl } from "./ProviderUsageControl";

const mounted: Array<{ root: Root; container: HTMLDivElement }> = [];
const updatedAt = DateTime.makeUnsafe("2026-07-22T20:00:00.000Z");

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(async () => {
  for (const entry of mounted.splice(0)) {
    await act(async () => entry.root.unmount());
    entry.container.remove();
  }
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

function usageWindow(overrides: Partial<UsageWindowViewModel> = {}): UsageWindowViewModel {
  return {
    key: "session",
    label: "Session",
    windowLabel: "5h",
    consumedPercent: 40,
    displayedPercent: 60,
    fillPercent: 60,
    percentageLabel: "60% remaining",
    resetLabel: "Resets in 5m",
    resetsAt: null,
    resetDescription: null,
    barColorClass: "bg-emerald-500",
    ...overrides,
  };
}

function provider(overrides: Partial<ProviderUsageViewModel> = {}): ProviderUsageViewModel {
  const session = usageWindow();
  const weekly = usageWindow({
    key: "weekly",
    label: "Weekly",
    windowLabel: "wk",
    consumedPercent: 85,
    displayedPercent: 15,
    fillPercent: 15,
    percentageLabel: "15% remaining",
  });
  return {
    provider: "codex",
    status: "ok",
    windows: [session, weekly],
    detailedWindows: [session, weekly],
    compactWindows: [weekly],
    plan: null,
    credits: null,
    updatedAt,
    error: null,
    ...overrides,
  };
}

function props(
  overrides: Partial<React.ComponentProps<typeof ProviderUsageControl>> = {},
): React.ComponentProps<typeof ProviderUsageControl> {
  return {
    viewModel: provider(),
    statusBarUsageMode: "detailed" as StatusBarUsageMode,
    iconOnly: false,
    onOpenProviderSettings: vi.fn(),
    ...overrides,
  };
}

async function mount(
  input: React.ComponentProps<typeof ProviderUsageControl>,
): Promise<{ container: HTMLDivElement; rerender: (next: typeof input) => Promise<void> }> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  await act(async () => root.render(<ProviderUsageControl {...input} />));
  return {
    container,
    rerender: async (next) => {
      await act(async () => root.render(<ProviderUsageControl {...next} />));
    },
  };
}

function usageTrigger(container: HTMLElement, label = "Codex usage"): HTMLButtonElement {
  const trigger = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
  if (trigger === null) throw new Error(`${label} trigger was not rendered.`);
  return trigger;
}

describe("ProviderUsageControl", () => {
  it("renders one borderless provider trigger with detailed or compact meters", async () => {
    const mountedControl = await mount(props());
    const trigger = usageTrigger(mountedControl.container);

    expect(mountedControl.container.querySelectorAll('button[aria-label$=" usage"]')).toHaveLength(
      1,
    );
    expect(trigger.className).toContain("border-0");
    expect(trigger.querySelectorAll('[role="progressbar"]')).toHaveLength(2);
    expect(trigger.textContent).toContain("Session");
    expect(trigger.textContent).toContain("Weekly");

    await mountedControl.rerender(props({ statusBarUsageMode: "compact" }));
    const compactTrigger = usageTrigger(mountedControl.container);
    expect(compactTrigger.querySelectorAll('[role="progressbar"]')).toHaveLength(1);
    expect(compactTrigger.textContent).not.toContain("Session");
    expect(compactTrigger.textContent).toContain("Weekly");
  });

  it("keeps each narrow provider badge independently clickable and identifiable", async () => {
    const codex = await mount(props({ iconOnly: true }));
    const claude = await mount(
      props({ viewModel: provider({ provider: "claude" }), iconOnly: true }),
    );

    const codexTrigger = usageTrigger(codex.container);
    const claudeTrigger = usageTrigger(claude.container, "Claude usage");
    expect(codexTrigger.querySelector('[data-provider-icon="codex"]')).not.toBeNull();
    expect(claudeTrigger.querySelector('[data-provider-icon="claude"]')).not.toBeNull();
    expect(codexTrigger.querySelectorAll('[role="progressbar"]')).toHaveLength(0);
    expect(claudeTrigger.querySelectorAll('[role="progressbar"]')).toHaveLength(0);
  });

  it("opens its provider detail directly without a roster or Back step", async () => {
    const { container } = await mount(props());
    await act(async () => usageTrigger(container).click());

    expect(container.querySelector('[data-testid="provider-usage-detail"]')).not.toBeNull();
    expect(container.textContent).toContain("Codex");
    expect(container.querySelector('[data-testid="provider-usage-roster"]')).toBeNull();
    expect(container.querySelector('[aria-label="Back to provider usage"]')).toBeNull();
    expect(container.querySelector('[aria-label="Refresh provider usage"]')).toBeNull();
  });

  it("closes its popup before navigating to provider settings", async () => {
    const onOpenProviderSettings = vi.fn();
    const { container } = await mount(props({ onOpenProviderSettings }));
    await act(async () => usageTrigger(container).click());
    const settings = Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
      (button) => button.textContent?.trim() === "Provider settings",
    );
    await act(async () => settings?.click());

    expect(onOpenProviderSettings).toHaveBeenCalledOnce();
    expect(container.querySelector('[data-testid="provider-usage-detail"]')).toBeNull();
  });
});
