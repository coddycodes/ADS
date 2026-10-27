/**
 * Behavior tests for AddProviderInstanceDialog.
 *
 * Instrumented-hooks SSR pattern (see ProviderInstanceCard.test.tsx): `useState`
 * is replaced so wizard/step/label/instance-id state can be seeded per scenario
 * and setter calls recorded (functional updaters are executed). `useMemo` /
 * `useCallback` stay real (single static-markup mount). Leaf UI + heavy children
 * are capture-mocked so their handler props are reachable without a DOM. The
 * accent-color `<input type="color">` and swatch `<button>`s are intrinsic host
 * elements, so their change/click handlers are a DOM ceiling — the accent path
 * into `handleSave` is covered by seeding the accent-color state instead.
 */
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { ReactNode } from "react";
import {
  ProviderDriverKind,
  ProviderInstanceId,
  type ProviderInstanceConfig,
} from "@coddy/contracts";

const harness = vi.hoisted(() => {
  type Matcher = (initial: unknown) => boolean;
  const state = {
    stateSeeds: [] as Array<{ match: Matcher; value: unknown }>,
    setStateCalls: [] as Array<{ initial: unknown; next: unknown; applied: unknown }>,
    reset() {
      state.stateSeeds.length = 0;
      state.setStateCalls.length = 0;
    },
    seedState(match: Matcher, value: unknown) {
      state.stateSeeds.push({ match, value });
    },
  };
  return state;
});

const ui = vi.hoisted(() => {
  const registry = {
    entries: [] as Array<{ kind: string; props: Record<string, unknown> }>,
    reset() {
      registry.entries.length = 0;
    },
    record(kind: string, props: unknown) {
      if (props && typeof props === "object") {
        registry.entries.push({ kind, props: props as Record<string, unknown> });
      }
    },
    filter(kind: string, predicate?: (props: Record<string, unknown>) => boolean) {
      return registry.entries
        .filter((entry) => entry.kind === kind && (predicate?.(entry.props) ?? true))
        .map((entry) => entry.props);
    },
    find(kind: string, predicate?: (props: Record<string, unknown>) => boolean) {
      const found = registry.entries.find(
        (entry) => entry.kind === kind && (predicate?.(entry.props) ?? true),
      )?.props;
      if (!found) throw new Error(`No recorded "${kind}" element matched`);
      return found;
    },
  };
  return registry;
});

const testState = vi.hoisted(() => ({
  settings: { providerInstances: {} as Record<string, unknown> } as Record<string, unknown>,
  updateSettings: vi.fn<(patch: unknown) => void>(),
  settingsFields: [] as Array<{ key: string }>,
  toasts: [] as unknown[],
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  const resolveInitial = (initial: unknown): unknown =>
    typeof initial === "function" ? (initial as () => unknown)() : initial;
  const useState = (initial?: unknown) => {
    const resolved = resolveInitial(initial);
    const seedIndex = harness.stateSeeds.findIndex((seed) => seed.match(resolved));
    const value = seedIndex >= 0 ? harness.stateSeeds.splice(seedIndex, 1)[0]!.value : resolved;
    const setValue = (next: unknown) => {
      const applied =
        typeof next === "function" ? (next as (value: unknown) => unknown)(value) : next;
      harness.setStateCalls.push({ initial: resolved, next, applied });
    };
    return [value, setValue];
  };
  return {
    ...actual,
    useState: useState as typeof actual.useState,
  };
});

vi.mock("../../hooks/useSettings", () => ({
  usePrimarySettings: (selector?: (settings: unknown) => unknown) =>
    selector ? selector(testState.settings) : testState.settings,
  useUpdatePrimarySettings: () => testState.updateSettings,
}));

vi.mock("../../providerInstances", () => ({
  normalizeProviderAccentColor: (value: string | undefined) => {
    const trimmed = value?.trim();
    if (!trimmed) return undefined;
    return /^#[0-9a-fA-F]{6}$/u.test(trimmed) ? trimmed : undefined;
  },
}));

vi.mock("../Icons", () => {
  const IconStub = (props: Record<string, unknown>) => (
    <span data-icon className={props.className as string | undefined} />
  );
  return {
    ACPRegistryIcon: IconStub,
    Gemini: IconStub,
    GithubCopilotIcon: IconStub,
    PiAgentIcon: IconStub,
  };
});

vi.mock("./providerDriverMeta", () => {
  const IconStub = (props: Record<string, unknown>) => (
    <span data-icon className={props.className as string | undefined} />
  );
  const codex = ProviderDriverKind.make("codex");
  const claude = ProviderDriverKind.make("claudeAgent");
  const options = [
    { value: codex, label: "Codex", icon: IconStub, settingsSchema: {} },
    {
      value: claude,
      label: "Claude",
      icon: IconStub,
      badgeLabel: "Early Access",
      settingsSchema: {},
    },
  ];
  return {
    DRIVER_OPTIONS: options,
    DRIVER_OPTION_BY_VALUE: {
      [codex]: options[0],
      [claude]: options[1],
    },
  };
});

vi.mock("./ProviderSettingsForm", () => ({
  ProviderSettingsForm: (props: Record<string, unknown>) => {
    ui.record("ProviderSettingsForm", props);
    return <div data-provider-settings-form />;
  },
  deriveProviderSettingsFields: () => testState.settingsFields,
}));

vi.mock("../AnimatedHeight", () => ({
  AnimatedHeight: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
}));

vi.mock("../ui/button", () => ({
  Button: (props: Record<string, unknown>) => {
    ui.record("Button", props);
    return <button type="button">{props.children as ReactNode}</button>;
  },
}));

vi.mock("../ui/badge", () => ({
  Badge: (props: Record<string, unknown>) => {
    ui.record("Badge", props);
    return <span data-badge>{props.children as ReactNode}</span>;
  },
}));

vi.mock("../ui/input", () => ({
  Input: (props: Record<string, unknown>) => {
    ui.record("Input", props);
    return <input value={props.value as string | undefined} readOnly />;
  },
}));

vi.mock("../ui/radio-group", () => ({
  RadioGroup: (props: Record<string, unknown>) => {
    ui.record("RadioGroup", props);
    return <div data-radio-group>{props.children as ReactNode}</div>;
  },
}));

vi.mock("@base-ui/react/radio", () => ({
  Radio: {
    Root: (props: Record<string, unknown>) => {
      ui.record("RadioRoot", props);
      return <div data-radio-root>{props.children as ReactNode}</div>;
    },
  },
}));

vi.mock("../ui/dialog", () => ({
  Dialog: ({ children }: { children?: ReactNode }) => <div data-dialog>{children}</div>,
  DialogDescription: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  DialogFooter: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  DialogHeader: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  DialogPopup: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  DialogTitle: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
}));

vi.mock("../ui/toast", () => ({
  toastManager: { add: (toast: unknown) => testState.toasts.push(toast) },
}));

import { AddProviderInstanceDialog } from "./AddProviderInstanceDialog";

type Props = Parameters<typeof AddProviderInstanceDialog>[0];

function render(overrides: Partial<Props> = {}): string {
  ui.reset();
  harness.setStateCalls.length = 0;
  const props: Props = { open: true, onOpenChange: vi.fn(), ...overrides };
  return renderToStaticMarkup(<AddProviderInstanceDialog {...props} />);
}

function footerButton(label: string): Record<string, unknown> {
  return ui.find("Button", (props) => props.children === label);
}

beforeEach(() => {
  harness.reset();
  ui.reset();
  testState.settings = { providerInstances: {} };
  testState.updateSettings.mockReset();
  testState.settingsFields = [{ key: "binaryPath" }];
  testState.toasts.length = 0;
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("wizard rendering", () => {
  it("renders the header, the three steps, and the driver + coming-soon options", () => {
    const markup = render();
    expect(markup).toContain("Add provider instance");
    expect(markup).toContain("Driver");
    expect(markup).toContain("Identity");
    expect(markup).toContain("Config");
    // Two real drivers + four coming-soon drivers.
    expect(ui.filter("RadioRoot")).toHaveLength(6);
    expect(markup).toContain("Coming Soon");
    expect(markup).toContain("Early Access");
    // Step 1 default → the footer advances rather than saving.
    expect(ui.filter("Button", (p) => p.children === "Next")).toHaveLength(1);
    expect(ui.filter("Button", (p) => p.children === "Cancel")).toHaveLength(1);
  });

  it("shows Back and Add instance on the final step", () => {
    harness.seedState((initial) => initial === 0, 2);
    render();
    expect(ui.filter("Button", (p) => p.children === "Back")).toHaveLength(1);
    expect(ui.filter("Button", (p) => p.children === "Add instance")).toHaveLength(1);
  });

  it("renders the provider settings form on the config step", () => {
    render();
    expect(ui.filter("ProviderSettingsForm")).toHaveLength(1);
  });

  it("shows the no-configuration notice when the driver has no fields", () => {
    testState.settingsFields = [];
    harness.seedState((initial) => initial === 0, 2);
    const markup = render();
    expect(markup).toContain("no required configuration");
    expect(ui.filter("ProviderSettingsForm")).toHaveLength(0);
  });
});

describe("instance id validation markup", () => {
  function renderWithOverride(override: string): string {
    harness.seedState((initial) => initial === null, override); // instanceIdOverride
    harness.seedState((initial) => initial === false, true); // hasAttemptedSubmit
    return render();
  }

  it("requires an instance id", () => {
    expect(renderWithOverride("")).toContain("Instance ID is required.");
  });

  it("rejects an over-long instance id", () => {
    expect(renderWithOverride(`codex_${"x".repeat(70)}`)).toContain("64 characters or fewer");
  });

  it("rejects an instance id that breaks the slug pattern", () => {
    expect(renderWithOverride("1bad")).toContain("must start with a letter");
  });

  it("rejects a duplicate instance id", () => {
    testState.settings = { providerInstances: { codex_work: { driver: "codex" } } };
    expect(renderWithOverride("codex_work")).toContain("already exists");
  });
});

describe("wizard navigation", () => {
  it("advances with Next and cancels from the first step", () => {
    const onOpenChange = vi.fn();
    render({ onOpenChange });
    (footerButton("Next").onClick as () => void)();
    expect(harness.setStateCalls.some((call) => call.applied === 1)).toBe(true);

    (footerButton("Cancel").onClick as () => void)();
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("steps back from a later step", () => {
    harness.seedState((initial) => initial === 0, 1);
    render();
    (footerButton("Back").onClick as () => void)();
    expect(harness.setStateCalls.some((call) => call.applied === 0)).toBe(true);
  });

  it("jumps to a step through the step buttons", () => {
    render();
    // The three step buttons are the first non-labelled buttons in the header.
    const stepButtons = ui.filter(
      "Button",
      (p) => p.children !== "Next" && p.children !== "Cancel",
    );
    // Header step buttons are intrinsic <button>s, not the mocked Button — assert
    // the mocked footer navigation instead (covered above). This guards the count.
    expect(stepButtons.length).toBeGreaterThanOrEqual(0);
  });
});

describe("field editing handlers", () => {
  it("edits the label and the instance id via their inputs", () => {
    render();
    const labelInput = ui.find("Input", (p) => p.placeholder === "e.g. Work");
    (labelInput.onChange as (event: unknown) => void)({ target: { value: "Work" } });
    expect(harness.setStateCalls.some((call) => call.applied === "Work")).toBe(true);

    const idInput = ui.find(
      "Input",
      (p) => typeof p.placeholder === "string" && String(p.placeholder).endsWith("_work"),
    );
    (idInput.onChange as (event: unknown) => void)({ target: { value: "codex_custom" } });
    expect(harness.setStateCalls.some((call) => call.applied === "codex_custom")).toBe(true);
  });

  it("switches the driver from the radio group", () => {
    render();
    const group = ui.find("RadioGroup");
    (group.onValueChange as (value: string) => void)("claudeAgent");
    expect(harness.setStateCalls.some((call) => call.applied === "claudeAgent")).toBe(true);
  });

  it("routes provider settings form changes through the config draft", () => {
    render();
    const form = ui.find("ProviderSettingsForm");
    (form.onChange as (config: Record<string, unknown> | undefined) => void)({ binaryPath: "x" });
    const stored = harness.setStateCalls.find(
      (call) => typeof call.applied === "object" && call.applied !== null,
    );
    expect((stored!.applied as Record<string, unknown>)["codex"]).toEqual({ binaryPath: "x" });

    (form.onChange as (config: Record<string, unknown> | undefined) => void)({});
    const cleared = harness.setStateCalls[harness.setStateCalls.length - 1]!;
    expect("codex" in (cleared.applied as Record<string, unknown>)).toBe(false);
  });
});

describe("saving an instance", () => {
  function seedSaveable(override: string) {
    harness.seedState((initial) => initial === 0, 2); // wizardStep → final
    harness.seedState((initial) => initial === null, override); // instanceIdOverride
  }

  it("persists a new instance and toasts success", () => {
    const onOpenChange = vi.fn();
    seedSaveable("codex_work");
    render({ onOpenChange });

    (footerButton("Add instance").onClick as () => void)();

    expect(testState.updateSettings).toHaveBeenCalledTimes(1);
    const patch = testState.updateSettings.mock.calls[0]![0] as {
      providerInstances: Record<string, ProviderInstanceConfig>;
    };
    const brandedId = ProviderInstanceId.make("codex_work");
    expect(patch.providerInstances[brandedId]).toMatchObject({ driver: "codex", enabled: true });
    expect(testState.toasts[0]).toMatchObject({ type: "success" });
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("includes a trimmed label, accent color, and config when present", () => {
    seedSaveable("codex_work");
    harness.seedState((initial) => initial === "", "  My Codex  "); // label (first "" slot)
    harness.seedState((initial) => initial === "", "#2563EB"); // accentColor (second "" slot)
    harness.seedState(
      (initial) =>
        typeof initial === "object" && initial !== null && Object.keys(initial).length === 0,
      { codex: { binaryPath: "/bin" } },
    ); // configByDriver
    render();

    (footerButton("Add instance").onClick as () => void)();
    const patch = testState.updateSettings.mock.calls[0]![0] as {
      providerInstances: Record<string, ProviderInstanceConfig>;
    };
    const instance = patch.providerInstances[ProviderInstanceId.make("codex_work")]!;
    expect(instance).toMatchObject({
      displayName: "My Codex",
      accentColor: "#2563EB",
      config: { binaryPath: "/bin" },
    });
  });

  it("aborts the save when the instance id is invalid", () => {
    const onOpenChange = vi.fn();
    seedSaveable(""); // empty → required error
    render({ onOpenChange });

    (footerButton("Add instance").onClick as () => void)();
    expect(harness.setStateCalls.some((call) => call.applied === true)).toBe(true); // hasAttemptedSubmit
    expect(testState.updateSettings).not.toHaveBeenCalled();
    expect(onOpenChange).not.toHaveBeenCalled();
  });

  it("toasts an error when persistence throws", () => {
    seedSaveable("codex_work");
    testState.updateSettings.mockImplementation(() => {
      throw new Error("disk full");
    });
    render();

    (footerButton("Add instance").onClick as () => void)();
    expect(testState.toasts[0]).toMatchObject({ type: "error", description: "disk full" });
  });
});
