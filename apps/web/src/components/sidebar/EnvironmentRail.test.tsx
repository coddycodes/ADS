import type { ReactElement, ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { EnvironmentId } from "@coddy/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  environments: [] as Array<unknown>,
  isReady: true,
  activeEnvironmentId: null as string | null,
  setActiveEnvironmentId: vi.fn(),
  navigate: vi.fn(),
  catalogCommandCalls: [] as Array<string>,
  buttons: [] as Array<Record<string, unknown>>,
  menuItems: [] as Array<Record<string, unknown>>,
  updateSnapshots: new Map<string, unknown>(),
  reset() {
    h.environments = [];
    h.isReady = true;
    h.activeEnvironmentId = null;
    h.setActiveEnvironmentId.mockReset();
    h.navigate.mockReset();
    h.catalogCommandCalls = [];
    h.buttons = [];
    h.menuItems = [];
    h.updateSnapshots.clear();
  },
}));

vi.mock("../../state/environments", () => ({
  useEnvironments: () => ({ environments: h.environments, isReady: h.isReady }),
}));
vi.mock("../../state/entities", () => ({
  useActiveEnvironmentId: () => h.activeEnvironmentId,
  setActiveEnvironmentId: h.setActiveEnvironmentId,
}));
vi.mock("../../connection/environmentCompat", () => ({
  resolveEnvironmentCompatVerdict: () => null,
  selectRemoteUpdateControlCapability: (serverConfig: unknown) =>
    (serverConfig as { environment?: { capabilities?: { remoteUpdateControl?: boolean } } } | null)
      ?.environment?.capabilities?.remoteUpdateControl === true,
}));
vi.mock("../../state/remoteUpdates", () => ({
  remoteUpdateEnvironment: {
    snapshot: ({ environmentId }: { environmentId: string }) => ({
      __kind: "remoteUpdateSnapshot",
      environmentId,
    }),
  },
}));
vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (atom: { __kind?: string; environmentId?: string } | null) => ({
    data:
      atom?.__kind === "remoteUpdateSnapshot" && atom.environmentId !== undefined
        ? (h.updateSnapshots.get(atom.environmentId) ?? null)
        : null,
    error: null,
    isPending: false,
    refresh: vi.fn(),
  }),
}));
vi.mock("../../connection/catalog", () => ({
  environmentCatalog: new Proxy(
    {},
    {
      get: (_target, key) => {
        h.catalogCommandCalls.push(String(key));
        return {};
      },
    },
  ),
}));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => h.navigate,
}));
vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: ReactNode }) => <>{children}</>,
  TooltipPopup: () => null,
  TooltipTrigger: ({ render, children }: { render?: ReactElement; children?: ReactNode }) => {
    if (render) {
      h.buttons.push({ ...(render.props as Record<string, unknown>) });
    }
    return <>{children}</>;
  },
}));
vi.mock("../ui/menu", () => ({
  Menu: ({ children }: { children?: ReactNode }) => <>{children}</>,
  MenuPopup: ({ children }: { children?: ReactNode }) => <>{children}</>,
  MenuTrigger: ({ render, children }: { render?: ReactElement; children?: ReactNode }) => {
    if (render) {
      h.buttons.push({ ...(render.props as Record<string, unknown>) });
    }
    return <>{children}</>;
  },
  MenuItem: (props: Record<string, unknown>) => {
    h.menuItems.push(props);
    return null;
  },
}));

const effects: Array<() => void | (() => void)> = [];
vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useEffect: (effect: () => void | (() => void)) => {
      effects.push(effect);
    },
  };
});

import { EnvironmentRail } from "./EnvironmentRail";

const ENV_PRIMARY = EnvironmentId.make("env-primary");
const ENV_REMOTE = EnvironmentId.make("env-remote");
const ENV_WSL = EnvironmentId.make("env-wsl");

function environment(input: {
  environmentId: string;
  label: string;
  target: Record<string, unknown>;
  phase?: string;
  remoteUpdateControl?: boolean;
}) {
  return {
    environmentId: input.environmentId,
    label: input.label,
    entry: { target: input.target },
    connection: { phase: input.phase ?? "connected", error: null, traceId: null },
    serverConfig:
      input.remoteUpdateControl === undefined
        ? null
        : {
            environment: {
              capabilities: { remoteUpdateControl: input.remoteUpdateControl },
            },
          },
  };
}

function renderRail() {
  effects.length = 0;
  h.buttons = [];
  h.menuItems = [];
  return renderToStaticMarkup(<EnvironmentRail />);
}

function buttonByTestId(testId: string) {
  return h.buttons.find((props) => props["data-testid"] === testId);
}

describe("EnvironmentRail", () => {
  it("renders Local plus remote entries with radio semantics", () => {
    h.reset();
    h.environments = [
      environment({
        environmentId: ENV_PRIMARY,
        label: "Local",
        target: { _tag: "PrimaryConnectionTarget" },
      }),
      environment({
        environmentId: ENV_REMOTE,
        label: "AI-SERVER",
        target: { _tag: "BearerConnectionTarget", connectionId: "paired-1" },
      }),
    ];
    h.activeEnvironmentId = ENV_PRIMARY;
    const markup = renderRail();
    expect(markup).toContain('role="radiogroup"');
    const local = buttonByTestId("environment-rail-local");
    expect(local?.["aria-checked"]).toBe(true);
    expect(local?.["data-selected"]).toBe(true);
    expect(local?.className).toContain("ring-primary");
    expect(local?.tabIndex).toBe(0);
    const remote = buttonByTestId(`environment-rail-entry-${ENV_REMOTE}`);
    expect(remote?.["aria-checked"]).toBe(false);
    expect(remote?.["data-selected"]).toBe(false);
    expect(remote?.className).not.toContain("ring-primary");
    expect(remote?.tabIndex).toBe(-1);
    expect(buttonByTestId("environment-rail-add-server")).toBeDefined();
    expect(buttonByTestId("environment-rail-manage")).toBeDefined();
  });

  it("stays visible with zero saved remotes", () => {
    h.reset();
    h.environments = [
      environment({
        environmentId: ENV_PRIMARY,
        label: "Local",
        target: { _tag: "PrimaryConnectionTarget" },
      }),
    ];
    h.activeEnvironmentId = ENV_PRIMARY;
    const markup = renderRail();
    expect(markup).toContain('data-testid="environment-rail"');
    expect(buttonByTestId("environment-rail-local")).toBeDefined();
    expect(markup).not.toContain('data-testid="environment-rail-divider"');
    // The fixed sidebar toggle is pinned over the rail's top strip, so the rail
    // reserves the topbar height ahead of the environments group; otherwise
    // the toggle sits on the Local entry and clicking Local collapses the sidebar.
    const topbar = markup.indexOf('data-testid="environment-rail-topbar"');
    const environments = markup.indexOf('aria-label="Environments"');
    expect(topbar).toBeGreaterThan(-1);
    expect(topbar).toBeLessThan(environments);
    expect(markup).toMatch(/environment-rail-topbar"[^>]*class="[^"]*workspace-topbar[^"]*"/);
  });

  it("writes selection to the active-environment atom and nothing else", () => {
    h.reset();
    h.environments = [
      environment({
        environmentId: ENV_PRIMARY,
        label: "Local",
        target: { _tag: "PrimaryConnectionTarget" },
      }),
      environment({
        environmentId: ENV_REMOTE,
        label: "AI-SERVER",
        target: { _tag: "BearerConnectionTarget", connectionId: "paired-1" },
      }),
    ];
    h.activeEnvironmentId = ENV_PRIMARY;
    renderRail();
    const remote = buttonByTestId(`environment-rail-entry-${ENV_REMOTE}`);
    expect(remote).toBeDefined();
    (remote!.onClick as () => void)();
    expect(h.setActiveEnvironmentId).toHaveBeenCalledExactlyOnceWith(ENV_REMOTE);
    expect(h.navigate).not.toHaveBeenCalled();
    expect(h.catalogCommandCalls).toEqual([]);
  });

  it("shows an amber attention dot when a capable remote has an update available", () => {
    h.reset();
    h.environments = [
      environment({
        environmentId: ENV_PRIMARY,
        label: "Local",
        target: { _tag: "PrimaryConnectionTarget" },
      }),
      environment({
        environmentId: ENV_REMOTE,
        label: "AI-SERVER",
        target: { _tag: "BearerConnectionTarget", connectionId: "paired-1" },
        remoteUpdateControl: true,
      }),
    ];
    h.updateSnapshots.set(ENV_REMOTE, {
      serverVersion: "0.4.2",
      latestVersion: "0.5.0",
      state: "update-available",
      error: null,
      support: { installMode: "interactive", reason: "available", installKind: "unknown" },
      downloadPercent: null,
      targetVersion: null,
      installStage: null,
    });

    expect(renderRail()).toContain('data-status="attention"');
  });

  it("groups desktop-local backends under the Local sub-picker", () => {
    h.reset();
    h.environments = [
      environment({
        environmentId: ENV_PRIMARY,
        label: "Local",
        target: { _tag: "PrimaryConnectionTarget" },
      }),
      environment({
        environmentId: ENV_WSL,
        label: "Ubuntu",
        target: { _tag: "BearerConnectionTarget", connectionId: "local:wsl-ubuntu" },
      }),
    ];
    h.activeEnvironmentId = ENV_PRIMARY;
    renderRail();
    expect(h.menuItems.map((item) => item.children)).toEqual(["This device", "Ubuntu"]);
    const ubuntu = h.menuItems[1];
    expect(ubuntu).toBeDefined();
    (ubuntu!.onClick as () => void)();
    expect(h.setActiveEnvironmentId).toHaveBeenCalledExactlyOnceWith(ENV_WSL);
  });

  it("deep-links bottom actions to Remote Servers settings", () => {
    h.reset();
    h.environments = [
      environment({
        environmentId: ENV_PRIMARY,
        label: "Local",
        target: { _tag: "PrimaryConnectionTarget" },
      }),
    ];
    renderRail();
    const addServer = buttonByTestId("environment-rail-add-server");
    expect(addServer).toBeDefined();
    (addServer!.onClick as () => void)();
    expect(h.navigate).toHaveBeenCalledWith({
      to: "/settings/remote-servers",
      search: { action: "add-server" },
    });
    const manage = buttonByTestId("environment-rail-manage");
    expect(manage).toBeDefined();
    (manage!.onClick as () => void)();
    expect(h.navigate).toHaveBeenCalledWith({ to: "/settings/remote-servers" });
    expect(h.setActiveEnvironmentId).not.toHaveBeenCalled();
  });

  it("resets a stale selection to Local once the catalog is ready", () => {
    h.reset();
    h.environments = [
      environment({
        environmentId: ENV_PRIMARY,
        label: "Local",
        target: { _tag: "PrimaryConnectionTarget" },
      }),
    ];
    h.activeEnvironmentId = ENV_REMOTE;
    renderRail();
    for (const effect of effects) effect();
    expect(h.setActiveEnvironmentId).toHaveBeenCalledExactlyOnceWith(ENV_PRIMARY);
  });

  it("does not reset while the catalog is loading", () => {
    h.reset();
    h.isReady = false;
    h.activeEnvironmentId = ENV_REMOTE;
    renderRail();
    for (const effect of effects) effect();
    expect(h.setActiveEnvironmentId).not.toHaveBeenCalled();
  });
});
