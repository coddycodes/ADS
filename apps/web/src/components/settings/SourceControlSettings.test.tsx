/**
 * Behavior tests for SourceControlSettingsPanel and its row/summary helpers.
 *
 * Instrumented-hooks SSR pattern (see ProviderInstanceCard.test.tsx): `useState`
 * is replaced so the per-row expand state can be seeded and setter calls
 * recorded. Discovery data is fed through a mocked `useEnvironmentQuery`; leaf UI
 * (Collapsible/NumberField/Switch/Tooltip/settings-layout) is capture-mocked so
 * the scan/reset/fetch-interval handlers are reachable without a DOM.
 */
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { ReactNode } from "react";
import * as Duration from "effect/Duration";
import * as Option from "effect/Option";
import { EnvironmentId } from "@coddy/contracts";
import type {
  SourceControlDiscoveryResult,
  SourceControlProviderAuth,
  SourceControlProviderDiscoveryItem,
  VcsDiscoveryItem,
} from "@coddy/contracts";
import { DEFAULT_UNIFIED_SETTINGS } from "@coddy/contracts/settings";

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
  environmentId: null as unknown,
  queriedAtom: undefined as unknown,
  discovery: {
    data: null as SourceControlDiscoveryResult | null,
    error: null as string | null,
    isPending: false,
    refresh: vi.fn(),
  },
  settings: {} as Record<string, unknown>,
  updateSettings: vi.fn<(patch: unknown) => void>(),
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
  return { ...actual, useState: useState as typeof actual.useState };
});

vi.mock("../../hooks/useSettings", () => ({
  usePrimarySettings: (selector?: (settings: unknown) => unknown) =>
    selector ? selector(testState.settings) : testState.settings,
  useUpdatePrimarySettings: () => testState.updateSettings,
}));

vi.mock("../../state/environments", () => ({
  usePrimaryEnvironment: () =>
    testState.environmentId ? { environmentId: testState.environmentId } : null,
}));

vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (atom: unknown) => {
    testState.queriedAtom = atom;
    return testState.discovery;
  },
}));

vi.mock("../../state/sourceControl", () => ({
  sourceControlEnvironment: {
    discovery: (args: unknown) => ({ __discovery: args }),
  },
}));

vi.mock("../Icons", () => {
  const Stub = (props: Record<string, unknown>) => (
    <span data-icon className={props.className as string | undefined} />
  );
  return {
    AzureDevOpsIcon: Stub,
    BitbucketIcon: Stub,
    GitHubIcon: Stub,
    GitIcon: Stub,
    GitLabIcon: Stub,
    JujutsuIcon: Stub,
  };
});

vi.mock("../ui/badge", () => ({
  Badge: (props: Record<string, unknown>) => {
    ui.record("Badge", props);
    return <span data-badge>{props.children as ReactNode}</span>;
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

vi.mock("../ui/collapsible", () => ({
  Collapsible: (props: Record<string, unknown>) => {
    ui.record("Collapsible", props);
    return <div data-collapsible>{props.children as ReactNode}</div>;
  },
  CollapsibleContent: ({ children }: { children?: ReactNode }) => (
    <div data-collapsible-content>{children}</div>
  ),
}));

vi.mock("../ui/empty", () => ({
  Empty: ({ children }: { children?: ReactNode }) => <div data-empty>{children}</div>,
  EmptyContent: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  EmptyDescription: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  EmptyHeader: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  EmptyMedia: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  EmptyTitle: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
}));

vi.mock("../ui/skeleton", () => ({
  Skeleton: (props: Record<string, unknown>) => (
    <span data-skeleton className={props.className as string | undefined} />
  ),
}));

vi.mock("../ui/number-field", () => ({
  NumberField: (props: Record<string, unknown>) => {
    ui.record("NumberField", props);
    return <div data-number-field>{props.children as ReactNode}</div>;
  },
  NumberFieldDecrement: (props: Record<string, unknown>) => (
    <button type="button" aria-label={props["aria-label"] as string | undefined} />
  ),
  NumberFieldGroup: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  NumberFieldIncrement: (props: Record<string, unknown>) => (
    <button type="button" aria-label={props["aria-label"] as string | undefined} />
  ),
  NumberFieldInput: (props: Record<string, unknown>) => (
    <input aria-label={props["aria-label"] as string | undefined} readOnly />
  ),
}));

vi.mock("../ui/switch", () => ({
  Switch: (props: Record<string, unknown>) => {
    ui.record("Switch", props);
    return <span data-switch aria-label={props["aria-label"] as string | undefined} />;
  },
}));

vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: ReactNode }) => <>{children}</>,
  TooltipPopup: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  TooltipTrigger: ({ render }: { render?: ReactNode }) => <span>{render}</span>,
}));

vi.mock("./RedactedSensitiveText", () => ({
  RedactedSensitiveText: (props: Record<string, unknown>) => {
    ui.record("RedactedSensitiveText", props);
    return <span data-redacted>{props.value as string}</span>;
  },
}));

vi.mock("./settingsLayout", () => ({
  SettingsPageContainer: ({ children }: { children?: ReactNode }) => (
    <div data-settings-page>{children}</div>
  ),
  SettingsSection: (props: Record<string, unknown>) => {
    ui.record("SettingsSection", props);
    return (
      <section data-settings-section>
        <div data-title>{props.title as ReactNode}</div>
        <div data-header-action>{props.headerAction as ReactNode}</div>
        {props.children as ReactNode}
      </section>
    );
  },
  SettingsRow: (props: Record<string, unknown>) => (
    <div>
      {props.title as ReactNode}
      {props.description as ReactNode}
      {props.control as ReactNode}
    </div>
  ),
  SettingResetButton: (props: Record<string, unknown>) => {
    ui.record("SettingResetButton", props);
    return <button type="button" data-reset />;
  },
}));

import { SourceControlSettingsPanel } from "./SourceControlSettings";

const ENV = EnvironmentId.make("environment-1");

function vcsItem(overrides: Partial<VcsDiscoveryItem> = {}): VcsDiscoveryItem {
  return {
    kind: "git",
    implemented: true,
    label: "Git",
    status: "available",
    version: Option.some("2.44.0"),
    installHint: "Install Git from git-scm.com.",
    detail: Option.none(),
    ...overrides,
  } as VcsDiscoveryItem;
}

function providerAuth(
  overrides: Partial<SourceControlProviderAuth> = {},
): SourceControlProviderAuth {
  return {
    status: "authenticated",
    account: Option.some("octocat"),
    host: Option.none(),
    detail: Option.none(),
    ...overrides,
  } as SourceControlProviderAuth;
}

function providerItem(
  overrides: Partial<SourceControlProviderDiscoveryItem> = {},
): SourceControlProviderDiscoveryItem {
  return {
    kind: "github",
    label: "GitHub",
    status: "available",
    executable: "gh",
    version: Option.some("2.60.0"),
    installHint: "Install the GitHub CLI.",
    detail: Option.none(),
    auth: providerAuth(),
    ...overrides,
  } as SourceControlProviderDiscoveryItem;
}

function result(
  overrides: Partial<SourceControlDiscoveryResult> = {},
): SourceControlDiscoveryResult {
  return {
    versionControlSystems: [],
    sourceControlProviders: [],
    ...overrides,
  } as SourceControlDiscoveryResult;
}

function render(): string {
  ui.reset();
  harness.setStateCalls.length = 0;
  return renderToStaticMarkup(<SourceControlSettingsPanel />);
}

beforeEach(() => {
  harness.reset();
  ui.reset();
  testState.environmentId = ENV;
  testState.discovery = { data: null, error: null, isPending: false, refresh: vi.fn() };
  testState.settings = {
    pullRequestsEnabled: true,
    automaticGitFetchInterval: Duration.seconds(120),
  };
  testState.updateSettings.mockReset();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("panel-level states", () => {
  it("renders skeleton sections during the initial scan", () => {
    testState.discovery = { data: null, error: null, isPending: true, refresh: vi.fn() };
    const markup = render();
    expect(markup).toContain("data-skeleton");
    expect(markup).toContain("Version Control");
    expect(markup).toContain("Source Control Providers");
  });

  it("renders the empty state and scans on click", () => {
    testState.discovery = { data: result(), error: null, isPending: false, refresh: vi.fn() };
    const markup = render();
    expect(markup).toContain("Nothing detected yet");

    const scan = ui.find(
      "Button",
      (p) => p["aria-label"] === undefined && p.children !== undefined,
    );
    (scan.onClick as () => void)();
    expect(testState.discovery.refresh).toHaveBeenCalledTimes(1);
  });

  it("renders the empty error state", () => {
    testState.discovery = {
      data: result(),
      error: "network down",
      isPending: false,
      refresh: vi.fn(),
    };
    const markup = render();
    expect(markup).toContain("Could not scan the server environment");
    expect(markup).toContain("network down");
  });

  it("returns no discovery atom and empty state when there is no primary environment", () => {
    testState.environmentId = null;
    testState.discovery = { data: result(), error: null, isPending: false, refresh: vi.fn() };
    const markup = render();
    expect(markup).toContain("Nothing detected yet");
    expect(testState.queriedAtom).toBeNull();
  });

  it("asks the server to record the hosts this explicit scan lists", () => {
    render();
    expect(testState.queriedAtom).toEqual({
      __discovery: { environmentId: ENV, input: { recordHosts: true } },
    });
  });

  it("rescans from the section header scan button", () => {
    testState.discovery = {
      data: result({ versionControlSystems: [vcsItem()] }),
      error: null,
      isPending: false,
      refresh: vi.fn(),
    };
    render();
    const headerScan = ui.find("Button", (p) => p["aria-label"] === "Rescan server environment");
    (headerScan.onClick as () => void)();
    expect(testState.discovery.refresh).toHaveBeenCalledTimes(1);
  });
});

describe("version control rows", () => {
  it("renders a git row with its version and the git fetch interval settings", () => {
    testState.discovery = {
      data: result({ versionControlSystems: [vcsItem()] }),
      error: null,
      isPending: false,
      refresh: vi.fn(),
    };
    const markup = render();
    expect(markup).toContain("Git");
    expect(markup).toContain("2.44.0");
    expect(markup).toContain("Fetch interval");
    // Availability switch is enabled for an implemented, available git.
    expect(ui.find("Switch").checked).toBe(true);
  });

  it("marks an unimplemented VCS as coming soon", () => {
    testState.discovery = {
      data: result({
        versionControlSystems: [vcsItem({ kind: "jj", label: "Jujutsu", implemented: false })],
      }),
      error: null,
      isPending: false,
      refresh: vi.fn(),
    };
    const markup = render();
    expect(markup).toContain("Coming Soon");
    expect(markup).toContain("coming soon.");
  });

  it("reports a VCS that is not available on the server", () => {
    testState.discovery = {
      data: result({
        versionControlSystems: [vcsItem({ status: "missing", installHint: "Install Git first." })],
      }),
      error: null,
      isPending: false,
      refresh: vi.fn(),
    };
    const markup = render();
    expect(markup).toContain("Not available on this server");
    expect(markup).toContain("Install Git first.");
  });
});

describe("git fetch interval settings", () => {
  function renderGitRow() {
    testState.discovery = {
      data: result({ versionControlSystems: [vcsItem()] }),
      error: null,
      isPending: false,
      refresh: vi.fn(),
    };
    return render();
  }

  it("shows a reset affordance when the interval differs from the default", () => {
    expect(renderGitRow()).toContain("The default is 180 seconds");
    expect(ui.filter("SettingResetButton")).toHaveLength(1);
    (ui.find("SettingResetButton").onClick as () => void)();
    expect(testState.updateSettings).toHaveBeenCalledWith({
      automaticGitFetchInterval: DEFAULT_UNIFIED_SETTINGS.automaticGitFetchInterval,
    });
  });

  it("hides the reset affordance at the default interval", () => {
    testState.settings = {
      automaticGitFetchInterval: DEFAULT_UNIFIED_SETTINGS.automaticGitFetchInterval,
    };
    renderGitRow();
    expect(ui.filter("SettingResetButton")).toHaveLength(0);
  });

  it("normalizes a new fetch interval value", () => {
    renderGitRow();
    const field = ui.find("NumberField");
    (field.onValueChange as (value: number | null) => void)(45);
    expect(testState.updateSettings).toHaveBeenCalledWith({
      automaticGitFetchInterval: Duration.seconds(45),
    });

    testState.updateSettings.mockClear();
    (field.onValueChange as (value: number | null) => void)(null);
    expect(testState.updateSettings).toHaveBeenCalledWith({
      automaticGitFetchInterval: Duration.seconds(0),
    });
  });
});

describe("source control provider rows", () => {
  function renderProviders(item: SourceControlProviderDiscoveryItem) {
    testState.discovery = {
      data: result({ sourceControlProviders: [item] }),
      error: null,
      isPending: false,
      refresh: vi.fn(),
    };
    return render();
  }

  it("renders an authenticated provider with a redacted account", () => {
    const markup = renderProviders(providerItem());
    expect(markup).toContain("GitHub");
    expect(markup).toContain("Authenticated");
    expect(ui.find("RedactedSensitiveText").value).toBe("octocat");
    // Enabled switch on for an authenticated, available provider.
    expect(ui.find("Switch").checked).toBe(true);
  });

  it("shows the unauthenticated warning with the executable hint", () => {
    const markup = renderProviders(
      providerItem({ auth: providerAuth({ status: "unauthenticated", account: Option.none() }) }),
    );
    expect(markup).toContain("Not authenticated");
    expect(markup).toContain("is not authenticated on this server");
    expect(markup).toContain("gh");
  });

  it("prompts to install when the provider has no executable", () => {
    const markup = renderProviders(
      providerItem({
        executable: undefined,
        auth: providerAuth({ status: "unauthenticated", account: Option.none() }),
      }),
    );
    expect(markup).toContain("Available.");
    expect(markup).toContain("Install the GitHub CLI.");
  });

  it("reports an unknown auth status as unverifiable", () => {
    const markup = renderProviders(
      providerItem({ auth: providerAuth({ status: "unknown", account: Option.none() }) }),
    );
    // The "Status unknown" label only surfaces as a badge, which is null for the
    // unknown status, so only the summary line is rendered.
    expect(markup).toContain("Could not verify");
  });

  it("renders the providers scan button when there are no VCS entries", () => {
    renderProviders(providerItem());
    // Header scan lives on the providers section because VCS is empty.
    const headerScan = ui.find("Button", (p) => p["aria-label"] === "Rescan server environment");
    (headerScan.onClick as () => void)();
    expect(testState.discovery.refresh).toHaveBeenCalledTimes(1);
  });
});

describe("discovery row expand toggle", () => {
  it("toggles the git row details open", () => {
    testState.discovery = {
      data: result({ versionControlSystems: [vcsItem()] }),
      error: null,
      isPending: false,
      refresh: vi.fn(),
    };
    render();
    const toggle = ui.find("Button", (p) => p["aria-label"] === "Toggle Git details");
    (toggle.onClick as () => void)();
    expect(harness.setStateCalls.some((call) => call.applied === true)).toBe(true);
  });
});

describe("Pull Requests settings", () => {
  it("toggles the global client setting even without discovery data", () => {
    expect(render()).toContain(
      "Show the Pull Requests button on every project and enable the pull request views.",
    );
    const toggle = ui.find("Switch", (p) => p["aria-label"] === "Enable pull requests");
    expect(toggle.checked).toBe(true);
    (toggle.onCheckedChange as (checked: boolean) => void)(false);
    expect(testState.updateSettings).toHaveBeenCalledWith({ pullRequestsEnabled: false });
  });
  it.each(["github", "gitlab"] as const)(
    "shows each configured %s host with its authentication state",
    (kind) => {
      testState.discovery.data = result({
        sourceControlProviders: [
          providerItem({
            kind,
            auth: providerAuth({
              hosts: [
                { host: "public.test", account: "alice", authenticated: true },
                { host: "company.test", account: null, authenticated: false },
              ],
            }),
          }),
        ],
      });
      const markup = render();
      expect(markup).toContain("public.test");
      expect(markup).toContain("Authenticated as");
      expect(markup).toContain("company.test");
      expect(markup).toContain("Not authenticated");
      expect(ui.filter("RedactedSensitiveText").some((p) => p.value === "alice")).toBe(true);
    },
  );
});
