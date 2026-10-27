/**
 * Behavior tests for the ProviderInstanceCard component (the pure
 * `deriveProviderModelsForDisplay` helper is covered separately in
 * ProviderInstanceCard.test.ts).
 *
 * Instrumented-hooks SSR pattern (see FilePreviewPanel.test.tsx): useState is
 * replaced so the nested environment-variable editor's state can be seeded and
 * its setter calls recorded; leaf UI + heavy child components are capture-mocked
 * so their handler props can be invoked directly without a DOM.
 */
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { ReactNode } from "react";
import {
  ProviderDriverKind,
  ProviderInstanceId,
  type ProviderInstanceConfig,
  type ProviderSessionDefault,
  type ServerProvider,
} from "@coddy/contracts";
import type { DriverOption } from "./providerDriverMeta";

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
    byLabel(kind: string, label: string) {
      return registry.find(kind, (props) => props["aria-label"] === label);
    },
  };
  return registry;
});

const testState = vi.hoisted(() => ({
  copyShouldFail: false,
  copies: [] as Array<{ value: string; ctx: unknown }>,
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
  const useEffect = () => {};
  const useRef = (initial?: unknown) => ({ current: initial ?? null });
  return {
    ...actual,
    useState: useState as typeof actual.useState,
    useEffect: useEffect as typeof actual.useEffect,
    useRef: useRef as typeof actual.useRef,
  };
});

vi.mock("../../hooks/useCopyToClipboard", () => ({
  useCopyToClipboard: (opts?: {
    onCopy?: (ctx: unknown) => void;
    onError?: (error: Error, ctx: unknown) => void;
  }) => ({
    isCopied: false,
    copyToClipboard: (value: string, ctx: unknown) => {
      testState.copies.push({ value, ctx });
      if (testState.copyShouldFail) {
        opts?.onError?.(new Error("copy failed"), ctx);
      } else {
        opts?.onCopy?.(ctx);
      }
    },
  }),
}));

vi.mock("../ui/toast", () => ({
  stackedThreadToast: (options: Record<string, unknown>) => ({ stacked: true, ...options }),
  toastManager: { add: (toast: unknown) => testState.toasts.push(toast) },
}));

vi.mock("../ui/badge", () => ({
  Badge: (props: Record<string, unknown>) => {
    ui.record("Badge", props);
    return <div data-badge>{props.children as ReactNode}</div>;
  },
}));

vi.mock("../ui/button", () => ({
  Button: (props: Record<string, unknown>) => {
    ui.record("Button", props);
    return (
      <button type="button" aria-label={props["aria-label"] as string | undefined}>
        {props.children as ReactNode}
      </button>
    );
  },
}));

vi.mock("../ui/checkbox", () => ({
  Checkbox: (props: Record<string, unknown>) => {
    ui.record("Checkbox", props);
    return <input type="checkbox" aria-label={props["aria-label"] as string | undefined} />;
  },
}));

vi.mock("../ui/collapsible", () => ({
  Collapsible: ({ children }: { children?: ReactNode }) => <div data-collapsible>{children}</div>,
  CollapsibleContent: ({ children }: { children?: ReactNode }) => (
    <div data-collapsible-content>{children}</div>
  ),
}));

vi.mock("../ui/draft-input", () => ({
  DraftInput: (props: Record<string, unknown>) => {
    ui.record("DraftInput", props);
    return <input aria-label={props["aria-label"] as string | undefined} />;
  },
}));

vi.mock("../ui/popover", () => ({
  Popover: ({ children }: { children?: ReactNode }) => <div data-popover>{children}</div>,
  PopoverTrigger: ({ render }: { render?: ReactNode }) => <span>{render}</span>,
  PopoverPopup: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
}));

vi.mock("../ui/scroll-area", () => ({
  ScrollArea: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
}));

vi.mock("../ui/switch", () => ({
  Switch: (props: Record<string, unknown>) => {
    ui.record("Switch", props);
    return <span data-switch aria-label={props["aria-label"] as string | undefined} />;
  },
}));

vi.mock("../ui/table", () => ({
  Table: ({ children }: { children?: ReactNode }) => <table>{children}</table>,
  TableBody: ({ children }: { children?: ReactNode }) => <tbody>{children}</tbody>,
  TableCell: ({ children }: { children?: ReactNode }) => <td>{children}</td>,
  TableHead: ({ children }: { children?: ReactNode }) => <th>{children}</th>,
  TableHeader: ({ children }: { children?: ReactNode }) => <thead>{children}</thead>,
  TableRow: ({ children }: { children?: ReactNode }) => <tr>{children}</tr>,
}));

vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: ReactNode }) => <>{children}</>,
  TooltipTrigger: ({ render, children }: { render?: ReactNode; children?: ReactNode }) => (
    <span>
      {render}
      {children}
    </span>
  ),
  TooltipPopup: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
}));

vi.mock("./ProviderSettingsForm", () => ({
  ProviderSettingsForm: (props: Record<string, unknown>) => {
    ui.record("ProviderSettingsForm", props);
    return <div data-provider-settings-form />;
  },
}));

vi.mock("./ProviderModelsSection", () => ({
  ProviderModelsSection: (props: Record<string, unknown>) => {
    ui.record("ProviderModelsSection", props);
    return <div data-provider-models-section />;
  },
}));

vi.mock("./ProviderSessionDefaultsControls", () => ({
  ProviderSessionDefaultsControls: (props: Record<string, unknown>) => {
    ui.record("ProviderSessionDefaultsControls", props);
    return <div data-provider-session-defaults />;
  },
}));

vi.mock("./ProviderAccentColorPicker", () => ({
  ProviderAccentColorPicker: (props: Record<string, unknown>) => {
    ui.record("ProviderAccentColorPicker", props);
    return <div data-accent-picker />;
  },
}));

vi.mock("./RedactedSensitiveText", () => ({
  RedactedSensitiveText: (props: Record<string, unknown>) => (
    <span data-redacted>{props.value as string}</span>
  ),
}));

vi.mock("../chat/ProviderInstanceIcon", () => ({
  ProviderInstanceIcon: (props: Record<string, unknown>) => {
    ui.record("ProviderInstanceIcon", props);
    return <span data-provider-icon />;
  },
}));

import { ProviderInstanceCard } from "./ProviderInstanceCard";

const INSTANCE_ID = ProviderInstanceId.make("codex_work");
const CODEX = ProviderDriverKind.make("codex");
const NOW = "2026-07-06T00:00:00.000Z";

type Props = Parameters<typeof ProviderInstanceCard>[0];

const DummyIcon = (props: Record<string, unknown>) => (
  <span data-dummy-icon className={props.className as string} />
);

function driverOption(overrides: Partial<DriverOption> = {}): DriverOption {
  return {
    value: CODEX,
    label: "Codex",
    icon: DummyIcon as unknown as DriverOption["icon"],
    settingsSchema: {} as unknown as DriverOption["settingsSchema"],
    ...overrides,
  };
}

function instanceConfig(overrides: Partial<ProviderInstanceConfig> = {}): ProviderInstanceConfig {
  return { driver: CODEX, ...overrides } as ProviderInstanceConfig;
}

function liveProvider(overrides: Partial<ServerProvider> = {}): ServerProvider {
  return {
    instanceId: INSTANCE_ID,
    driver: CODEX,
    enabled: true,
    installed: true,
    version: null,
    status: "ready",
    auth: { status: "unauthenticated" },
    checkedAt: NOW,
    models: [],
    slashCommands: [],
    skills: [],
    ...overrides,
  } as ServerProvider;
}

function baseProps(overrides: Partial<Props> = {}): Props {
  return {
    instanceId: INSTANCE_ID,
    instance: instanceConfig(),
    driverOption: driverOption(),
    liveProvider: undefined,
    isExpanded: true,
    onExpandedChange: vi.fn(),
    onUpdate: vi.fn(),
    hiddenModels: [],
    favoriteModels: [],
    modelOrder: [],
    onHiddenModelsChange: vi.fn(),
    onFavoriteModelsChange: vi.fn(),
    onModelOrderChange: vi.fn(),
    ...overrides,
  };
}

function render(props: Props): string {
  ui.reset();
  harness.setStateCalls.length = 0;
  return renderToStaticMarkup(<ProviderInstanceCard {...props} />);
}

beforeEach(() => {
  harness.reset();
  ui.reset();
  testState.copyShouldFail = false;
  testState.copies.length = 0;
  testState.toasts.length = 0;
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("header + status", () => {
  it("renders the display name, instance code, and driver icon", () => {
    const markup = render(baseProps());
    expect(markup).toContain("Codex");
    // instanceId differs from driver → the code chip is shown
    expect(markup).toContain("codex_work");
    expect(ui.filter("ProviderInstanceIcon")).toHaveLength(1);
  });

  it("prefers an explicit display name and hides the code chip when it matches the driver", () => {
    const markup = render(
      baseProps({
        instanceId: ProviderInstanceId.make("codex"),
        instance: instanceConfig({ displayName: "My Codex" }),
      }),
    );
    expect(markup).toContain("My Codex");
  });

  it("shows the early-access badge from the driver option", () => {
    render(baseProps({ driverOption: driverOption({ badgeLabel: "Early Access" }) }));
    expect(ui.filter("Badge")).toHaveLength(1);
    expect(ui.find("Badge").children).toBe("Early Access");
  });

  it("falls back to the driver-option icon when the driver kind is invalid", () => {
    const markup = render(
      baseProps({
        instance: instanceConfig({
          driver: "1-not-a-slug" as unknown as ProviderInstanceConfig["driver"],
        }),
        driverOption: driverOption(),
      }),
    );
    // Not the ProviderInstanceIcon (driverKind is null) but the fallback icon.
    expect(ui.filter("ProviderInstanceIcon")).toHaveLength(0);
    expect(markup).toContain("data-dummy-icon");
  });

  it("falls back to a status dot when there is no driver kind or icon", () => {
    const markup = render(
      baseProps({
        instance: instanceConfig({
          driver: "1-not-a-slug" as unknown as ProviderInstanceConfig["driver"],
        }),
        driverOption: undefined,
      }),
    );
    expect(ui.filter("ProviderInstanceIcon")).toHaveLength(0);
    expect(markup).not.toContain("data-dummy-icon");
    // The "not shipped" notice replaces the models section for unknown drivers.
    expect(markup).toContain("not");
    expect(markup).toContain("shipped with the current build");
  });

  it("derives the status dot from the enabled flag when the server has not reported", () => {
    // enabled (default) → warning dot
    render(baseProps());
    expect(ui.find("ProviderInstanceIcon").statusDotClassName).toBe("bg-warning");
    // explicitly disabled → disabled dot
    render(baseProps({ instance: instanceConfig({ enabled: false }) }));
    expect(ui.find("ProviderInstanceIcon").statusDotClassName).toBe("bg-amber-400");
    // server status wins when present
    render(baseProps({ liveProvider: liveProvider({ status: "error" }) }));
    expect(ui.find("ProviderInstanceIcon").statusDotClassName).toBe("bg-destructive");
  });

  it("renders the authenticated-as row with a redacted email and detail", () => {
    const markup = render(
      baseProps({
        liveProvider: liveProvider({
          auth: { status: "authenticated", email: "dev@example.com", label: "Pro" },
        }),
      }),
    );
    expect(markup).toContain("Authenticated as");
    expect(markup).toContain("dev@example.com");
    expect(markup).toContain("Pro");
  });

  it("renders the summary row with the account email when not authenticated", () => {
    const markup = render(
      baseProps({
        liveProvider: liveProvider({
          auth: { status: "unauthenticated", email: "pending@example.com" },
          message: "Sign in required",
        }),
      }),
    );
    expect(markup).toContain("Not authenticated");
    expect(markup).toContain("pending@example.com");
    expect(markup).toContain("Sign in required");
  });

  it("renders a checking summary when there is no live provider", () => {
    const markup = render(baseProps({ liveProvider: undefined }));
    expect(markup).toContain("Checking provider status");
  });

  it("renders the version label when present", () => {
    expect(render(baseProps({ liveProvider: liveProvider({ version: "1.2.3" }) }))).toContain(
      "v1.2.3",
    );
  });

  it("renders session defaults after status and before the collapsible content", () => {
    const sessionDefaults: ProviderSessionDefault = { model: "model-alpha" };
    const markup = render(
      baseProps({
        sessionDefaults,
        onSessionDefaultsChange: vi.fn(),
      }),
    );

    const statusIndex = markup.indexOf("Checking provider status");
    const defaultsIndex = markup.indexOf("data-provider-session-defaults");
    const collapsibleIndex = markup.indexOf("data-collapsible");
    const collapsibleContentIndex = markup.indexOf("data-collapsible-content");

    expect(statusIndex).toBeGreaterThanOrEqual(0);
    expect(defaultsIndex).toBeGreaterThan(statusIndex);
    expect(collapsibleIndex).toBeGreaterThan(defaultsIndex);
    expect(collapsibleContentIndex).toBeGreaterThan(defaultsIndex);
    expect(ui.find("ProviderSessionDefaultsControls")).toMatchObject({
      driver: CODEX,
      models: [],
      value: sessionDefaults,
      disabled: false,
    });
  });

  it("omits session defaults when no change callback is provided", () => {
    const markup = render(baseProps({ sessionDefaults: { model: "model-alpha" } }));

    expect(markup).not.toContain("data-provider-session-defaults");
    expect(ui.filter("ProviderSessionDefaultsControls")).toHaveLength(0);
  });

  it("renders as an independent rounded provider panel", () => {
    const markup = render(baseProps());

    expect(markup).toContain(
      'class="relative overflow-visible rounded-2xl border bg-card text-card-foreground shadow-sm/4"',
    );
  });

  it("disables session defaults when the provider instance is disabled", () => {
    render(
      baseProps({
        instance: instanceConfig({ enabled: false }),
        onSessionDefaultsChange: vi.fn(),
      }),
    );

    expect(ui.find("ProviderSessionDefaultsControls").disabled).toBe(true);
  });
});

describe("version advisory", () => {
  function advisoryProvider(overrides = {}) {
    return liveProvider({
      version: "1.0.0",
      versionAdvisory: {
        status: "behind_latest",
        currentVersion: "1.0.0",
        latestVersion: "2.0.0",
        updateCommand: "npm i -g codex@latest",
        canUpdate: true,
        checkedAt: NOW,
        message: null,
      },
      ...overrides,
    });
  }

  it("shows the update popover with a run-update button and copyable command", () => {
    const onRunUpdate = vi.fn();
    const markup = render(baseProps({ liveProvider: advisoryProvider(), onRunUpdate }));
    expect(markup).toContain("Update available");
    expect(markup).toContain("npm i -g codex@latest");

    const runButton = ui.find(
      "Button",
      (p) => p.children != null && String((p.children as unknown[])?.[1] ?? "") === "Update now",
    );
    (runButton.onClick as () => void)();
    expect(onRunUpdate).toHaveBeenCalledTimes(1);
  });

  it("shows update-check failures without update or copy actions", () => {
    const markup = render(
      baseProps({
        liveProvider: advisoryProvider({
          versionAdvisory: {
            status: "unknown",
            currentVersion: "1.0.0",
            latestVersion: null,
            updateCommand: "npm i -g codex@latest",
            canUpdate: true,
            checkedAt: NOW,
            message: "Could not check for provider updates.",
          },
        }),
        onRunUpdate: vi.fn(),
      }),
    );

    expect(markup).toContain("Update check failed");
    expect(markup).toContain("Could not check for provider updates.");
    expect(markup).not.toContain("Update now");
    expect(
      ui.filter("Button", (props) => props["aria-label"] === "Copy update command"),
    ).toHaveLength(0);
  });

  it("shows unchanged update diagnostics and rechecks the provider version", () => {
    const onRecheck = vi.fn();
    const markup = render({
      ...baseProps({
        liveProvider: advisoryProvider({
          updateState: {
            status: "unchanged",
            startedAt: NOW,
            finishedAt: NOW,
            message:
              "Update completed, but C:\\Users\\mauro\\.local\\bin\\claude.exe still reports 2.1.225; expected 2.1.226.",
            output: "Successfully updated from 2.1.225 to version 2.1.226",
          },
        }),
      }),
      onRecheck,
    } as Props);

    expect(markup).toContain("C:\\Users\\mauro\\.local\\bin\\claude.exe");
    expect(markup).toContain("still reports 2.1.225; expected 2.1.226");
    expect(markup).toContain("Successfully updated from 2.1.225 to version 2.1.226");

    (ui.byLabel("Button", "Recheck provider version").onClick as () => void)();
    expect(onRecheck).toHaveBeenCalledTimes(1);
  });

  it("shows the updating state and omits the run button when no handler is given", () => {
    const markup = render(
      baseProps({ liveProvider: advisoryProvider(), onRunUpdate: vi.fn(), isUpdating: true }),
    );
    expect(markup).toContain("Updating");

    // Without onRunUpdate the button and manual divider are dropped but the
    // command is still copyable.
    render(baseProps({ liveProvider: advisoryProvider() }));
    expect(ui.filter("Button", (p) => p["aria-label"] === "Copy update command")).toHaveLength(1);
  });

  it("copies the update command and toasts success", () => {
    render(baseProps({ liveProvider: advisoryProvider(), onRunUpdate: vi.fn() }));
    (ui.byLabel("Button", "Copy update command").onClick as () => void)();
    expect(testState.copies[0]!.value).toBe("npm i -g codex@latest");
    expect(testState.toasts).toHaveLength(1);
    expect(testState.toasts[0]).toMatchObject({ type: "success" });
  });

  it("toasts a stacked error when copying fails", () => {
    testState.copyShouldFail = true;
    render(baseProps({ liveProvider: advisoryProvider(), onRunUpdate: vi.fn() }));
    (ui.byLabel("Button", "Copy update command").onClick as () => void)();
    expect(testState.toasts[0]).toMatchObject({ stacked: true, type: "error" });
  });
});

describe("top-level controls", () => {
  it("toggles expansion and enabled state", () => {
    const onExpandedChange = vi.fn();
    const onUpdate = vi.fn();
    render(baseProps({ isExpanded: false, onExpandedChange, onUpdate }));

    (ui.byLabel("Button", "Toggle Codex details").onClick as () => void)();
    expect(onExpandedChange).toHaveBeenCalledWith(true);

    (ui.byLabel("Switch", "Enable Codex").onCheckedChange as (checked: boolean) => void)(false);
    expect(onUpdate).toHaveBeenCalledWith(expect.objectContaining({ enabled: false }));
  });

  it("renders the delete button only when onDelete is provided", () => {
    const onDelete = vi.fn();
    render(baseProps({ onDelete }));
    const del = ui.byLabel("Button", `Delete provider instance ${INSTANCE_ID}`);
    (del.onClick as () => void)();
    expect(onDelete).toHaveBeenCalledTimes(1);

    render(baseProps({ onDelete: undefined }));
    expect(
      ui.filter("Button", (p) => p["aria-label"] === `Delete provider instance ${INSTANCE_ID}`),
    ).toHaveLength(0);
  });

  it("renders a header action when provided", () => {
    const markup = render(baseProps({ headerAction: <span data-header-action /> }));
    expect(markup).toContain("data-header-action");
  });
});

describe("expanded editors", () => {
  it("commits and clears the display name via the draft input", () => {
    const onUpdate = vi.fn();
    render(baseProps({ instance: instanceConfig({ displayName: "Old" }), onUpdate }));
    const nameInput = ui.find(
      "DraftInput",
      (p) => typeof p.id === "string" && (p.id as string).endsWith("-display-name"),
    );
    (nameInput.onCommit as (value: string) => void)("  New Name  ");
    expect(onUpdate).toHaveBeenCalledWith(expect.objectContaining({ displayName: "New Name" }));

    onUpdate.mockReset();
    (nameInput.onCommit as (value: string) => void)("   ");
    // Clearing removes the displayName key entirely.
    const cleared = onUpdate.mock.calls[0]![0] as ProviderInstanceConfig;
    expect("displayName" in cleared).toBe(false);
  });

  it("commits the accent color, dropping it when normalization fails", () => {
    const onUpdate = vi.fn();
    render(baseProps({ onUpdate }));
    const picker = ui.find("ProviderAccentColorPicker");
    (picker.onCommit as (value: string) => void)("#2563eb");
    expect(onUpdate).toHaveBeenCalledWith(expect.objectContaining({ accentColor: "#2563eb" }));

    onUpdate.mockReset();
    (picker.onCommit as (value: string) => void)("bogus");
    const cleared = onUpdate.mock.calls[0]![0] as ProviderInstanceConfig;
    expect("accentColor" in cleared).toBe(false);
  });

  it("routes provider settings form changes through updateConfig", () => {
    const onUpdate = vi.fn();
    render(baseProps({ onUpdate }));
    const form = ui.find("ProviderSettingsForm");
    (form.onChange as (config: Record<string, unknown> | undefined) => void)({ apiKey: "x" });
    expect(onUpdate).toHaveBeenCalledWith(expect.objectContaining({ config: { apiKey: "x" } }));

    onUpdate.mockReset();
    (form.onChange as (config: Record<string, unknown> | undefined) => void)(undefined);
    const cleared = onUpdate.mock.calls[0]![0] as ProviderInstanceConfig;
    expect("config" in cleared).toBe(false);
  });

  it("reads custom models from config and writes them back", () => {
    const onUpdate = vi.fn();
    render(
      baseProps({
        instance: instanceConfig({ config: { customModels: ["a", 1, "b"] } }),
        onUpdate,
      }),
    );
    const section = ui.find("ProviderModelsSection");
    // Non-string entries are filtered out when reading.
    expect(section.customModels).toEqual(["a", "b"]);

    (section.onChange as (next: ReadonlyArray<string>) => void)(["a", "b", "c"]);
    expect(onUpdate).toHaveBeenCalledWith(
      expect.objectContaining({ config: { customModels: ["a", "b", "c"] } }),
    );
  });

  it("treats a non-array or missing config customModels as empty", () => {
    render(baseProps({ instance: instanceConfig({ config: { customModels: "nope" } }) }));
    expect(ui.find("ProviderModelsSection").customModels).toEqual([]);

    render(baseProps({ instance: instanceConfig({ config: null }) }));
    expect(ui.find("ProviderModelsSection").customModels).toEqual([]);
  });

  it("writes custom models onto a fresh config blob when none exists", () => {
    const onUpdate = vi.fn();
    render(baseProps({ instance: instanceConfig(), onUpdate }));
    (ui.find("ProviderModelsSection").onChange as (next: ReadonlyArray<string>) => void)(["m"]);
    expect(onUpdate).toHaveBeenCalledWith(
      expect.objectContaining({ config: { customModels: ["m"] } }),
    );
  });

  it("shows the unknown-driver notice instead of the models section", () => {
    const markup = render(baseProps({ driverOption: undefined }));
    expect(ui.filter("ProviderModelsSection")).toHaveLength(0);
    expect(ui.filter("ProviderSettingsForm")).toHaveLength(0);
    expect(markup).toContain("cannot be");
  });
});

describe("environment variables", () => {
  function envInstance(vars: ReadonlyArray<Record<string, unknown>>) {
    return instanceConfig({ environment: vars as ProviderInstanceConfig["environment"] });
  }

  it("renders the empty-state hint with no variables", () => {
    const markup = render(baseProps());
    expect(markup).toContain("Add variables to pass API keys");
  });

  it("renders existing variables, redacting stored secrets", () => {
    const markup = render(
      baseProps({
        instance: envInstance([
          { name: "API_KEY", value: "", sensitive: true, valueRedacted: true },
        ]),
      }),
    );
    expect(markup).toContain("API_KEY");
    const valueInput = ui.find(
      "DraftInput",
      (p) => p["aria-label"] === "Environment variable value 1",
    );
    expect(valueInput.type).toBe("password");
    expect(String(valueInput.placeholder)).toContain("Stored secret");
  });

  it("adds a new empty draft row", () => {
    render(baseProps());
    const addButton = ui.find(
      "Button",
      (p) => Array.isArray(p.children) && (p.children as unknown[]).includes("Add"),
    );
    (addButton.onClick as () => void)();
    const appended = harness.setStateCalls.find((c) => Array.isArray(c.applied))!;
    expect((appended.applied as unknown[]).length).toBe(1);
  });

  it("updates a variable name and publishes the cleaned list", () => {
    const onUpdate = vi.fn();
    render(
      baseProps({
        instance: envInstance([{ name: "OLD", value: "v", sensitive: true }]),
        onUpdate,
      }),
    );
    const nameInput = ui.find(
      "DraftInput",
      (p) => p["aria-label"] === "Environment variable name 1",
    );
    (nameInput.onCommit as (value: string) => void)("NEW_NAME");
    expect(onUpdate).toHaveBeenCalledWith(
      expect.objectContaining({
        environment: [expect.objectContaining({ name: "NEW_NAME", value: "v", sensitive: true })],
      }),
    );
  });

  it("does not publish while a variable name is invalid but non-empty", () => {
    const onUpdate = vi.fn();
    render(
      baseProps({ instance: envInstance([{ name: "OK", value: "v", sensitive: true }]), onUpdate }),
    );
    const nameInput = ui.find(
      "DraftInput",
      (p) => p["aria-label"] === "Environment variable name 1",
    );
    (nameInput.onCommit as (value: string) => void)("1bad");
    expect(onUpdate).not.toHaveBeenCalled();
  });

  it("skips empty invalid rows but still publishes the valid ones", () => {
    const onUpdate = vi.fn();
    harness.seedState(
      (initial) => Array.isArray(initial),
      [
        { id: "0:OK", name: "OK", value: "v", sensitive: true },
        { id: "draft-x", name: "", value: "", sensitive: true },
      ],
    );
    render(
      baseProps({ instance: envInstance([{ name: "OK", value: "v", sensitive: true }]), onUpdate }),
    );
    const valueInput = ui.find(
      "DraftInput",
      (p) => p["aria-label"] === "Environment variable value 1",
    );
    (valueInput.onCommit as (value: string) => void)("v2");
    expect(onUpdate).toHaveBeenCalledWith(
      expect.objectContaining({
        environment: [expect.objectContaining({ name: "OK", value: "v2" })],
      }),
    );
  });

  it("toggles the sensitive checkbox", () => {
    const onUpdate = vi.fn();
    render(
      baseProps({ instance: envInstance([{ name: "OK", value: "v", sensitive: true }]), onUpdate }),
    );
    const checkbox = ui.find("Checkbox");
    (checkbox.onCheckedChange as (checked: boolean) => void)(false);
    expect(onUpdate).toHaveBeenCalledWith(
      expect.objectContaining({ environment: [expect.objectContaining({ sensitive: false })] }),
    );
  });

  it("removes a variable, publishing the empty list", () => {
    const onUpdate = vi.fn();
    render(
      baseProps({ instance: envInstance([{ name: "OK", value: "v", sensitive: true }]), onUpdate }),
    );
    const removeButton = ui.byLabel("Button", "Remove environment variable OK");
    (removeButton.onClick as () => void)();
    // The card drops the environment key entirely when it becomes empty.
    const published = onUpdate.mock.calls[0]![0] as ProviderInstanceConfig;
    expect("environment" in published).toBe(false);
  });
});
