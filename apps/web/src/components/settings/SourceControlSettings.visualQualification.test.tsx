// @vitest-environment happy-dom
import { RegistryContext } from "@effect/atom-react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
  useLocation,
} from "@tanstack/react-router";
import { EnvironmentId, type SourceControlDiscoveryResult } from "@coddy/contracts";
import { DEFAULT_CLIENT_SETTINGS } from "@coddy/contracts/settings";
import * as Option from "effect/Option";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { compile } from "tailwindcss";
import { expect, it, vi } from "vite-plus/test";

const ports = vi.hoisted(() => ({
  discovery: null as unknown,
  targets: [] as unknown[],
  updateSettings: vi.fn(),
}));
// External environment/query/command ports supply real atoms; hooks and UI remain real.
vi.mock("../status-bar/ProfilerRecordingSegment", () => ({ ProfilerRecordingSegment: () => null }));

vi.mock("../../state/sourceControl", () => ({
  sourceControlEnvironment: {
    discovery: (target: unknown) => {
      ports.targets.push(target);
      return ports.discovery;
    },
  },
}));
vi.mock("../../state/environments", () => ({
  usePrimaryEnvironment: () => ({ environmentId: "owned-source-control" }),
}));
vi.mock("../../state/server", async () => {
  const { Atom } = await import("effect/reactivity");
  const { DEFAULT_SERVER_SETTINGS } = await import("@coddy/contracts");
  return {
    primaryServerSettingsAtom: Atom.make(DEFAULT_SERVER_SETTINGS),
    serverEnvironment: {
      updateSettings: { label: "owned settings update", run: ports.updateSettings },
    },
  };
});
vi.mock("~/connection/currentEnvironmentPresentation", async () => {
  const { createEnvironmentPresentationPolicy } =
    await import("~/connection/environmentPresentationPolicy");
  return {
    readCurrentEnvironmentPresentationPolicy: () =>
      createEnvironmentPresentationPolicy({ surface: "desktop", platform: "linux" }),
  };
});

import {
  __resetClientSettingsPersistenceForTests,
  __setClientSettingsForTests,
} from "../../hooks/useSettings";
import { Route as SettingsRoute } from "../../routes/settings";
import { AppStatusBarView } from "../status-bar/AppStatusBar";
import { Sidebar, SidebarProvider } from "../ui/sidebar";
import { SettingsSidebarNav } from "./SettingsSidebarNav";
import { SourceControlSettingsPanel } from "./SourceControlSettings";

const missingDetail = "Command was not found on the server PATH.";
const missingAuth = (detail: string) => ({
  status: "unknown" as const,
  account: Option.none<string>(),
  host: Option.none<string>(),
  detail: Option.some(detail),
});
const discovery: SourceControlDiscoveryResult = {
  versionControlSystems: [
    {
      kind: "git",
      label: "Git",
      executable: "git",
      implemented: true,
      status: "available",
      version: Option.some("git version 2.50.0"),
      installHint: "Install Git from https://git-scm.com/downloads or with your package manager.",
      detail: Option.none(),
    },
    {
      kind: "jj",
      label: "Jujutsu",
      executable: "jj",
      implemented: false,
      status: "missing",
      version: Option.none(),
      installHint: "Install Jujutsu from https://github.com/jj-vcs/jj.",
      detail: Option.some(missingDetail),
    },
  ],
  sourceControlProviders: [
    {
      kind: "github",
      label: "GitHub",
      executable: "gh",
      status: "missing",
      version: Option.none(),
      installHint: "Install GitHub CLI from https://cli.github.com/.",
      detail: Option.some(missingDetail),
      auth: missingAuth("Hosting integration command was not found on the server PATH."),
    },
    {
      kind: "gitlab",
      label: "GitLab",
      executable: "glab",
      status: "missing",
      version: Option.none(),
      installHint: "Install GitLab CLI from https://gitlab.com/gitlab-org/cli.",
      detail: Option.some(missingDetail),
      auth: missingAuth("Hosting integration command was not found on the server PATH."),
    },
    {
      kind: "azure-devops",
      label: "Azure DevOps",
      executable: "az",
      status: "missing",
      version: Option.none(),
      installHint:
        "Install Azure CLI from https://learn.microsoft.com/cli/azure/install-azure-cli.",
      detail: Option.some(missingDetail),
      auth: missingAuth("Hosting integration command was not found on the server PATH."),
    },
    {
      kind: "bitbucket",
      label: "Bitbucket",
      status: "missing",
      version: Option.none(),
      installHint: "Configure Bitbucket API credentials in server settings.",
      detail: Option.none(),
      auth: missingAuth("Bitbucket API credentials are not configured."),
    },
  ],
};

function MountedSettings() {
  const location = useLocation();
  const SettingsLayout = SettingsRoute.options.component!;
  return (
    <SidebarProvider className="h-dvh! min-h-0! border-t border-panel-separator">
      <Sidebar>
        <SettingsSidebarNav pathname={location.pathname} />
      </Sidebar>
      {/* Match __root's flex column, block Outlet host and status-bar sibling. */}
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <div className="min-h-0 min-w-0 flex-1">
          <SettingsLayout />
        </div>
        <AppStatusBarView
          usage={null}
          diagnostics={{ diagnostics: null, queryError: null }}
          localDiagnostics={null}
          terminalCount={0}
          showResourceUsage={false}
          onRefresh={() => {}}
        />
      </div>
    </SidebarProvider>
  );
}

it("keeps Settings inside its allotted viewport above the status bar", async () => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  __setClientSettingsForTests(DEFAULT_CLIENT_SETTINGS);
  ports.targets.length = 0;
  ports.updateSettings.mockReset();
  const query = Atom.make<AsyncResult.AsyncResult<SourceControlDiscoveryResult, Error>>(
    AsyncResult.initial(true),
  );
  ports.discovery = query;
  const registry = AtomRegistry.make();
  const rootRoute = createRootRoute({ component: MountedSettings });
  const initialRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/settings/keybindings",
    component: () => null,
  });
  const sourceControlRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/settings/source-control",
    component: SourceControlSettingsPanel,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([initialRoute, sourceControlRoute]),
    history: createMemoryHistory({ initialEntries: ["/settings/keybindings"] }),
  });
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const styles = document.createElement("style");
  // HappyDOM rejects dvh and the layer-order statement. Normalize those only
  // for CSS-intent inspection; this test does not simulate browser layout.
  styles.textContent = (await compile("@tailwind utilities;"))
    .build(["h-dvh", "h-dvh!", "h-full", "border-t", "flex", "flex-col", "shrink-0"])
    .replaceAll("@layer properties;", "")
    .replaceAll("100dvh", "100vh");
  document.head.append(styles);
  const network = vi.fn(() => {
    throw new Error("No network in mounted Source Control seam.");
  });
  vi.stubGlobal("fetch", network);
  vi.stubGlobal("localStorage", window.localStorage);
  const details = 'button[aria-label="Toggle Git details"]';
  try {
    await act(async () => {
      await router.load();
      root.render(
        <RegistryContext.Provider value={registry}>
          <RouterProvider router={router} />
        </RegistryContext.Provider>,
      );
    });
    const sourceControl = Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
      (button) => button.textContent?.trim() === "Source Control",
    );
    expect(sourceControl).toBeDefined();
    await act(async () => sourceControl!.click());
    expect(container.querySelector(details)).toBeNull();
    expect(container.querySelectorAll('[data-slot="skeleton"]').length).toBeGreaterThan(0);
    await act(async () => registry.set(query, AsyncResult.success(discovery)));
    const detailControl = container.querySelector<HTMLButtonElement>(details)!;
    expect(detailControl).not.toBeNull();
    expect(detailControl.disabled).toBe(false);
    await act(async () => detailControl.click());
    expect(detailControl.getAttribute("aria-expanded")).toBe("true");
    expect(
      container.querySelector<HTMLInputElement>(
        'input[aria-label="Automatic Git fetch interval in seconds"]',
      )?.value,
    ).toBe("180");

    const shell = container.querySelector<HTMLElement>('[data-slot="sidebar-wrapper"]')!;
    const inset = container.querySelector<HTMLElement>('[data-slot="sidebar-inset"]')!;
    const declaredHeight = (element: HTMLElement) =>
      Array.from(styles.sheet!.cssRules).find(
        (rule) =>
          rule instanceof CSSStyleRule &&
          rule.style.height !== "" &&
          element.matches(rule.selectorText),
      ) as CSSStyleRule | undefined;
    const outletHost = inset.parentElement!;
    const column = outletHost.parentElement!;
    const statusBar = container.querySelector<HTMLElement>("[data-status-bar]")!;
    expect(getComputedStyle(column).display).toBe("flex");
    expect(getComputedStyle(column).flexDirection).toBe("column");
    expect(getComputedStyle(outletHost).display).toBe("block");
    expect(statusBar.parentElement).toBe(column);
    expect(getComputedStyle(statusBar).flexShrink).toBe("0");
    expect(declaredHeight(shell)?.style.height).toBe("100vh");
    expect(Number.parseFloat(getComputedStyle(shell).borderTopWidth)).toBe(1);
    // Verify declared sizing ownership; HappyDOM does not supply native layout evidence.
    expect(declaredHeight(inset)?.style.height).toBe("100%");
    expect(ports.targets.length).toBeGreaterThan(0);
    expect(
      ports.targets.every(
        (target) =>
          JSON.stringify(target) ===
          JSON.stringify({
            environmentId: EnvironmentId.make("owned-source-control"),
            input: { recordHosts: true },
          }),
      ),
    ).toBe(true);
    expect(ports.updateSettings).not.toHaveBeenCalled();
    expect(network).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
    container.remove();
    styles.remove();
    registry.dispose();
    __resetClientSettingsPersistenceForTests();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  }
}, 5_000);
